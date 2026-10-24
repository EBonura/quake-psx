//! The emulated console, stepped the way `psoxide-gate` steps it, with one extra stop
//! condition: the guest publishing a new journey state block.
//!
//! The chunking (`PC_CHUNK`, per-tick deadline, SPU catch-up after every chunk) is the
//! gate's, so a tape recorded here replays through the gate to the same machine state.

use std::path::Path;

use emulator_core::input_tape::PadSample;
use emulator_core::{fast_boot_disc, Bus, Cpu, Deinterlace};

const STEPS_PER_TICK: u64 = 1_000_000;
const PC_CHUNK: u64 = 16_384;

pub struct Session {
    pub cpu: Cpu,
    pub bus: Bus,
    pub ticks: u64,
    steps: u64,
    deadline: u64,
    period: u64,
    /// Guest RAM offset of the state block's `seq` word.
    seq_off: usize,
    last_seq: u32,
    /// The sample live for the next poll (what the driver last decided).
    current: PadSample,
    /// Tape samples from `start_poll` on, one per completed port-1 poll.
    pub tape: Vec<PadSample>,
    pub start_poll: Option<u64>,
    polls_recorded: u64,
}

impl Session {
    pub fn boot(cue: &Path, state_addr: u32) -> Result<Self, String> {
        let disc = psoxide_settings::library::load_disc_from_cue(cue)?;
        let mut cpu = Cpu::new();
        let mut bus = Bus::new_without_bios();
        fast_boot_disc(&mut bus, &mut cpu, &disc)
            .map_err(|e| format!("boot {}: {e:?}", cue.display()))?;
        bus.cdrom.insert_disc(Some(disc));
        if let Ok(Some(lbas)) = psoxide_settings::library::load_sbi_for(cue) {
            bus.cdrom.set_bad_subq_sectors(lbas);
        }
        // Journey pad = "analog": a DualShock forced into analog mode.
        bus.attach_digital_pad_port1();
        let _ = bus.force_port1_analog_mode();
        let _ = psoxide_jit::install_tier(&mut cpu);
        let period = bus.vblank_period().max(1);
        let deadline = bus.cycles().saturating_add(period);
        Ok(Self {
            cpu,
            bus,
            ticks: 0,
            steps: 0,
            deadline,
            period,
            seq_off: (state_addr & 0x1f_ffff) as usize + 8,
            last_seq: 0,
            current: PadSample::default(),
            tape: Vec::new(),
            start_poll: None,
            polls_recorded: 0,
        })
    }

    fn seq(&self) -> u32 {
        let r = self.bus.ram();
        u32::from_le_bytes(r[self.seq_off..self.seq_off + 4].try_into().unwrap())
    }

    /// Write the machine to `path` (a GUI-compatible save state) with a small sidecar of
    /// the driver's own counters, so a later run can continue from this exact publish stop.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let tick = self.cpu.tick();
        let (cpu, bus) = (&self.cpu, &self.bus);
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(64 * 1024 * 1024)
                .spawn_scoped(scope, || {
                    let snapshot = emulator_core::EmulatorStateRef { cpu, bus };
                    psoxide_settings::savestate::SaveStateV1::new(snapshot, "journey", tick).write_to(path)
                })
                .map_err(|e| e.to_string())?
                .join()
                .map_err(|_| "save-state writer panicked".to_string())?
                .map_err(|e| e.to_string())
        })?;
        let side = format!(
            "ticks={}\nsteps={}\ndeadline={}\nlast_seq={}\nbuttons={}\nrx={}\nry={}\nlx={}\nly={}\n",
            self.ticks, self.steps, self.deadline, self.last_seq, self.current.buttons,
            self.current.right_x, self.current.right_y, self.current.left_x, self.current.left_y
        );
        std::fs::write(path.with_extension("side"), side).map_err(|e| e.to_string())
    }

    /// Replace this freshly booted session's machine with a saved one.
    pub fn resume(&mut self, path: &Path) -> Result<(), String> {
        let loaded = std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(64 * 1024 * 1024)
                .spawn_scoped(scope, || {
                    psoxide_settings::savestate::SaveStateV1::<emulator_core::EmulatorState>::read_from(path)
                })
                .map_err(|e| e.to_string())?
                .join()
                .map_err(|_| "save-state loader panicked".to_string())?
                .map_err(|e| e.to_string())
        })?;
        let mut payload = loaded.payload;
        payload.bus.restore_excluded_from(&mut self.bus);
        self.cpu = payload.cpu;
        self.bus = payload.bus;
        let _ = psoxide_jit::install_tier(&mut self.cpu);
        let side = std::fs::read_to_string(path.with_extension("side")).map_err(|e| e.to_string())?;
        let get = |k: &str| -> u64 {
            side.lines()
                .find_map(|l| l.strip_prefix(&format!("{k}=")))
                .unwrap_or_else(|| panic!("sidecar missing {k}"))
                .parse()
                .unwrap()
        };
        self.ticks = get("ticks");
        self.steps = get("steps");
        self.deadline = get("deadline");
        self.last_seq = get("last_seq") as u32;
        self.current = PadSample {
            buttons: get("buttons") as u16,
            right_x: get("rx") as u8,
            right_y: get("ry") as u8,
            left_x: get("lx") as u8,
            left_y: get("ly") as u8,
        };
        self.tape.clear();
        self.start_poll = None;
        Ok(())
    }

    pub fn polls(&self) -> u64 {
        self.bus.port1_completed_polls()
    }

    /// Run until the guest publishes a new state block. Returns false when
    /// `max_ticks` more ticks passed without one.
    pub fn run_to_publish(&mut self, max_ticks: u64) -> Result<bool, String> {
        let stop_at = self.ticks + max_ticks;
        loop {
            let polls = self.bus.port1_completed_polls();
            let seq0 = self.last_seq;
            let seq_off = self.seq_off;
            let n = (STEPS_PER_TICK - self.steps).min(PC_CHUNK).max(1);
            let (ran, result) = self.cpu.run(&mut self.bus, n, self.deadline, |bus| {
                let r = bus.ram();
                bus.port1_completed_polls() != polls
                    || u32::from_le_bytes(r[seq_off..seq_off + 4].try_into().unwrap()) != seq0
            });
            self.steps += ran;
            self.bus.run_spu_to_current_cycle();
            if self.bus.spu.audio_queue_len() != 0 {
                let _ = self.bus.spu.drain_audio();
            }
            if let Err(e) = result {
                return Err(format!(
                    "guest fault at tick {} pc={:#010x} ra={:#010x}: {e:?}",
                    self.ticks,
                    self.cpu.pc(),
                    self.cpu.gpr(31)
                ));
            }
            let after = self.bus.port1_completed_polls();
            if self.start_poll.is_some() {
                for _ in polls..after {
                    self.tape.push(self.current);
                    self.polls_recorded += 1;
                }
            }
            let tick_over = self.bus.cycles() >= self.deadline || self.steps >= STEPS_PER_TICK;
            if tick_over {
                self.deadline = self.bus.cycles().saturating_add(self.period);
                self.steps = 0;
                self.ticks += 1;
            } else if ran == 0 {
                return Err(format!("cpu made no progress at tick {}", self.ticks));
            }
            let seq = self.seq();
            if seq != self.last_seq {
                self.last_seq = seq;
                return Ok(true);
            }
            if tick_over && self.ticks >= stop_at {
                return Ok(false);
            }
        }
    }

    /// Choose the sample for the next poll. The first call starts the tape.
    pub fn set_pad(&mut self, sample: PadSample) {
        if self.start_poll.is_none() {
            self.start_poll = Some(self.bus.port1_completed_polls());
        }
        self.current = sample;
        sample.apply_to_bus(&mut self.bus);
    }

    pub fn ram(&self, addr: u32, len: usize) -> &[u8] {
        let off = (addr & 0x1f_ffff) as usize;
        &self.bus.ram()[off..off + len]
    }

    pub fn screenshot(&self, path: &Path) -> Result<(), String> {
        let (rgba, w, h) = self.bus.gpu.display_rgba8_with(Deinterlace::from_env());
        let img = image::RgbaImage::from_raw(w, h, rgba).ok_or("bad frame")?;
        img.save(path).map_err(|e| e.to_string())
    }
}
