//! Plays the Quake journey image through the emulator with a host-side controller and
//! records the pad it held as a poll-bound tape for `psoxide-gate`.

mod fight;
mod emit;
mod machine;
mod obs;
mod pad;
mod script;
mod walk;
mod world;

use std::path::PathBuf;

use emulator_core::input_tape::{write_tape_poll_bound, PadSample};

use machine::Session;
use obs::Obs;
use pad::{button, Command};

struct Args {
    disc: PathBuf,
    map: PathBuf,
    out: PathBuf,
    mode: String,
    rest: Vec<String>,
}

fn parse() -> Args {
    let mut it = std::env::args().skip(1);
    let mut disc = None;
    let mut map = None;
    let mut out = PathBuf::from("journey-out");
    let mut mode = String::from("probe");
    let mut rest = Vec::new();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--disc" => disc = it.next().map(PathBuf::from),
            "--map" => map = it.next().map(PathBuf::from),
            "--out" => out = it.next().map(PathBuf::from).expect("--out dir"),
            "--mode" => mode = it.next().expect("--mode name"),
            _ => rest.push(a),
        }
    }
    Args { disc: disc.expect("--disc"), map: map.expect("--map"), out, mode, rest }
}

/// Address of the `JOURNEY_STATE` static from an lld link map.
fn state_address(map: &PathBuf) -> u32 {
    let text = std::fs::read_to_string(map).expect("link map");
    for line in text.lines() {
        if line.contains("JOURNEY_STATE") && !line.contains(".bss.") {
            let first = line.split_whitespace().next().unwrap_or("");
            if let Ok(v) = u32::from_str_radix(first.trim_start_matches("0x"), 16) {
                if v >= 0x8000_0000 {
                    return v;
                }
            }
        }
    }
    panic!("JOURNEY_STATE not found in link map");
}

fn main() {
    let args = parse();
    let addr = state_address(&args.map);
    eprintln!("journey state at {addr:#x}");
    std::fs::create_dir_all(&args.out).unwrap();
    let mut session = Session::boot(&args.disc, addr).expect("boot");
    match args.mode.as_str() {
        "probe" => probe(&mut session, addr, &args),
        "play" => play(&mut session, addr, &args),
        other => panic!("unknown mode {other}"),
    }
}

fn read(session: &Session, addr: u32) -> Obs {
    Obs::parse(session.ram(addr, obs::WORDS * 4)).expect("state block")
}

/// Blind pulses: `--press <frame>:<button>[:<hold>]`, `--frames N`, `--shot every`.
fn probe(session: &mut Session, addr: u32, args: &Args) {
    let mut presses: Vec<(u32, u16, u32)> = Vec::new();
    let mut frames = 600u32;
    let mut shot_every = 60u32;
    let mut i = 0;
    while i < args.rest.len() {
        match args.rest[i].as_str() {
            "--press" => {
                let parts: Vec<&str> = args.rest[i + 1].split(':').collect();
                let b = match parts[1] {
                    "cross" => button::CROSS,
                    "start" => button::START,
                    "down" => button::DOWN,
                    "up" => button::UP,
                    "circle" => button::CIRCLE,
                    "square" => button::SQUARE,
                    other => panic!("button {other}"),
                };
                presses.push((parts[0].parse().unwrap(), b, parts.get(2).map_or(3, |h| h.parse().unwrap())));
                i += 2;
            }
            "--frames" => {
                frames = args.rest[i + 1].parse().unwrap();
                i += 2;
            }
            "--shot" => {
                shot_every = args.rest[i + 1].parse().unwrap();
                i += 2;
            }
            other => panic!("probe arg {other}"),
        }
    }
    let mut n = 0u32;
    while n < frames {
        if !session.run_to_publish(4000).expect("run") {
            eprintln!("no publish after 4000 ticks at frame {n}");
            break;
        }
        let o = read(session, addr);
        n += 1;
        let mut b = 0u16;
        for (at, btn, hold) in &presses {
            if n >= *at && n < at + hold {
                b |= btn;
            }
        }
        session.set_pad(PadSample::from_buttons(b));
        if n % 30 == 0 {
            eprintln!(
                "f{n} guest#{} map{} mode{} pos{:?} yaw{} hp{} mon{} mov{}",
                o.frame, o.map, o.mode, o.pos(), o.angles[1], o.health, o.monsters.len(), o.movers.len()
            );
        }
        if shot_every > 0 && n % shot_every == 0 {
            session.screenshot(&args.out.join(format!("probe-{n:05}.png"))).unwrap();
        }
    }
    let samples = session.tape.clone();
    if let Some(start) = session.start_poll {
        write_tape_poll_bound(&args.out.join("probe.pxtape"), &samples, start).unwrap();
    }
    let _ = Command::default();
}

/// Run the Episode 1 journey script, logging as it goes, and write the tape.
fn play(session: &mut Session, addr: u32, args: &Args) {
    use script::Script;
    let mut max_frames = 3000u32;
    let mut shot_every = 0u32;
    let mut maps = String::from("id1psx/maps");
    let mut routesim = String::new();
    let mut seed = 0u32;
    let mut save_at: Vec<u32> = Vec::new();
    let mut resume: Option<(PathBuf, u32)> = None;
    let mut trace = (0u32, 0u32);
    let mut trace_every = 6u32;
    let mut i = 0;
    while i < args.rest.len() {
        match args.rest[i].as_str() {
            "--frames" => { max_frames = args.rest[i + 1].parse().unwrap(); i += 2; }
            "--shot" => { shot_every = args.rest[i + 1].parse().unwrap(); i += 2; }
            "--maps" => { maps = args.rest[i + 1].clone(); i += 2; }
            "--save-at" => { save_at.push(args.rest[i + 1].parse().unwrap()); i += 2; }
            "--resume" => { resume = Some((PathBuf::from(&args.rest[i + 1]), args.rest[i + 2].parse().unwrap())); i += 3; }
            "--seed" => { seed = args.rest[i + 1].parse().unwrap(); i += 2; }
            "--routesim" => { routesim = args.rest[i + 1].clone(); i += 2; }
            "--trace" => { trace = (args.rest[i + 1].parse().unwrap(), args.rest[i + 2].parse().unwrap()); i += 3; }
            "--trace-every" => { trace_every = args.rest[i + 1].parse().unwrap(); i += 2; }
            other => panic!("play arg {other}"),
        }
    }
    let mut journey = match &resume {
        Some((_, map)) => script::Journey::resumed(&maps, *map),
        None => script::Journey::new(&maps),
    };
    journey.routesim = routesim;
    journey.seed = seed;
    journey.fighter.seed = seed;
    let mut pending = false;
    let mut n = 0u32;
    let mut last_map = resume.as_ref().map_or(0, |(_, m)| *m);
    if let Some((path, _)) = &resume {
        session.resume(path).expect("resume");
        pending = true;
        eprintln!("resumed from {}", path.display());
    }
    // (label, exclusive end index in the tape, state seen at the frame that ended it)
    let mut boundaries: Vec<(String, usize, Obs)> = Vec::new();
    let mut finished = false;
    while n < max_frames {
        if pending {
            pending = false;
        } else if !session.run_to_publish(4000).expect("run") {
            eprintln!("no publish after 4000 ticks at frame {n}");
            break;
        }
        let o = read(session, addr);
        n += 1;
        if save_at.contains(&o.map) && o.map != last_map && o.mode & 3 == 0 {
            let path = args.out.join(format!("state-map{}.psx", o.map));
            session.save(&path).expect("save state");
            eprintln!("saved state {}", path.display());
            save_at.retain(|m| *m != o.map);
        }
        if n == 1 || o.map != last_map {
            let list: Vec<String> = o
                .pickups
                .iter()
                .map(|p| {
                    format!(
                        "{}:{}/{}/{}@{},{},{}",
                        p.index,
                        p.code >> 16,
                        (p.code >> 8) & 255,
                        p.code & 255,
                        p.origin[0] >> 12,
                        p.origin[1] >> 12,
                        p.origin[2] >> 12
                    )
                })
                .collect();
            eprintln!("PICKUPS map{} [{}]", o.map, list.join(" "));
        }
        let step = journey.step(&o);
        let Some(sample) = step else {
            if let Some(f) = &journey.failure { eprintln!("FAILED: {f}"); } else { finished = true; }
            break;
        };
        session.set_pad(sample);
        if o.map != last_map {
            let index = (session.polls() - session.start_poll.unwrap_or(0)) as usize + 1;
            boundaries.push((MAP_LABELS[o.map as usize].to_string(), index, o.clone()));
            last_map = o.map;
        }
        if n >= trace.0 && n <= trace.1 && n % trace_every == 0 {
            let p = o.pos();
            let near: Vec<String> = o.monsters.iter().filter(|m| m.alive()).filter_map(|m| {
                let (dx, dy) = ((m.origin[0] >> 12) - p[0], (m.origin[1] >> 12) - p[1]);
                let d = ((dx * dx + dy * dy) as f64).sqrt() as i32;
                (d < 500).then(|| format!("m{}k{}s{}hp{}@{}", m.index, m.kind, m.state, m.health, d))
            }).collect();
            let mv: Vec<String> = o.movers.iter().filter(|m| m.state != 0 || m.class == 0x10).map(|m| format!("{}:{}@{},{},{}", m.index, m.state, m.origin[0] >> 12, m.origin[1] >> 12, m.origin[2] >> 12)).collect();
            eprintln!("T f{n} [{}] pos{:?} vel{:?} g{} w{} hp{} wpn{} ammo{:?} own{:x} mon[{}] movers[{}]", journey.last, p, [o.velocity[0] >> 12, o.velocity[1] >> 12, o.velocity[2] >> 12], o.grounded as u8, o.water, o.health, o.weapon, o.ammo, o.weapons, near.join(" "), mv.join(" "));
        }
        if n % 30 == 0 {
            eprintln!(
                "f{n} map{} mode{} pos{:?} yaw{} hp{} mon{} [{}]",
                o.map, o.mode, o.pos(), o.angles[1] & 4095, o.health, o.monsters.len(), journey.status()
            );
        }
        if shot_every > 0 && n % shot_every == 0 {
            session.screenshot(&args.out.join(format!("play-{n:05}.png"))).unwrap();
        }
    }
    let samples = session.tape.clone();
    if let Some(start) = session.start_poll {
        write_tape_poll_bound(&args.out.join("play.pxtape"), &samples, start).unwrap();
        eprintln!("tape: {} samples from poll {start}; finished={finished}", samples.len());
        emit::write_chain(&args.out, addr, start, &samples, &boundaries, finished);
    }
}

const MAP_LABELS: [&str; 9] = ["start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8"];
