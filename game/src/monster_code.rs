//! Map-admitted monster code uses the existing, quiescent 30 KiB transfer
//! allocation. It returns that entire allocation before any map/audio load.
//! No render packet, SPU sample or gameplay state is stored in this buffer.

use crate::asset::ResidentMap;
use crate::entity::{EntityLoadError, EntityScene, MonsterFrameResult};
use alloc::vec::Vec;
use core::mem::MaybeUninit;
use psx_residency::arena::{
    CallerArena, Descriptor, Error, PackedCall, PackedDescriptor, Registry, StaticImport,
};
use psx_residency::Handle;
use quake_core::combat::WeaponState;
use quake_core::effects::ImpactParticles;
use quake_core::monster::{MonsterAction, MonsterRuntime, MonsterThinkInput};
use quake_core::mover::{QuakeMover, QuakeMoverError};
use quake_formats::Vec3I32;
use quake_formats::{BrushModel, MapEntity};

const BUFFER_BYTES: usize = 30_720;
const HALF_BYTES: usize = BUFFER_BYTES / 2;
const MAX_CALLS: usize = 4;
pub const PACKAGE_CHUNK: u32 = 4;
pub const DOOR_PACKAGE_CHUNK: u32 = 5;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Image {
    Doors,
    Monsters,
}

#[used]
#[no_mangle]
#[cfg_attr(target_arch = "mips", link_section = ".data.quake_door_descriptor")]
static mut QUAKE_DOOR_DESCRIPTOR: PackedDescriptor = PackedDescriptor {
    words: 0,
    id: 0,
    linked: 0,
    checksum: 0,
    calls: 0,
};
#[used]
#[no_mangle]
#[cfg_attr(target_arch = "mips", link_section = ".data.quake_door_calls")]
static mut QUAKE_DOOR_CALLS: [PackedCall; MAX_CALLS] = [PackedCall {
    selector: 0,
    site: 0,
    offset: 0,
}; MAX_CALLS];

#[used]
#[no_mangle]
#[cfg_attr(target_arch = "mips", link_section = ".data.quake_monster_descriptor")]
static mut QUAKE_MONSTER_DESCRIPTOR: PackedDescriptor = PackedDescriptor {
    words: 0,
    id: 0,
    linked: 0,
    checksum: 0,
    calls: 0,
};
#[used]
#[no_mangle]
#[cfg_attr(target_arch = "mips", link_section = ".data.quake_monster_calls")]
static mut QUAKE_MONSTER_CALLS: [PackedCall; MAX_CALLS] = [PackedCall {
    selector: 0,
    site: 0,
    offset: 0,
}; MAX_CALLS];

/// Select one complete signed-low code window within the existing allocation.
/// Installation and every later admission use this identical address decision.
fn split_buffers(words: &mut [u32]) -> Result<(&mut [u32], &mut [u32]), Error> {
    if words.len() != BUFFER_BYTES / 4 {
        return Err(Error::Arena);
    }
    let (first, second) = words.split_at_mut(HALF_BYTES / 4);
    if code_half(
        first.as_ptr() as usize as u32,
        second.as_ptr() as usize as u32,
    )? {
        Ok((second, first))
    } else {
        Ok((first, second))
    }
}

fn code_half(first: u32, second: u32) -> Result<bool, Error> {
    if psx_residency::overlay::placement(first, HALF_BYTES as u32, true).is_ok() {
        Ok(true)
    } else if psx_residency::overlay::placement(second, HALF_BYTES as u32, true).is_ok() {
        Ok(false)
    } else {
        Err(Error::Arena)
    }
}

struct Metadata {
    words: usize,
    descriptor: Descriptor,
    calls: [(u32, u32); MAX_CALLS],
    count: usize,
}

fn metadata(image: Image) -> Result<Metadata, Error> {
    // The shared SDK finalizer patches initialized writable data in this same
    // linked image. Volatile reads prohibit folding the unpatched placeholders.
    let (header, records, id, linked) = match image {
        Image::Doors => (
            core::ptr::addr_of!(QUAKE_DOOR_DESCRIPTOR),
            core::ptr::addr_of!(QUAKE_DOOR_CALLS).cast::<PackedCall>(),
            DOOR_PACKAGE_CHUNK,
            0x80418000,
        ),
        Image::Monsters => (
            core::ptr::addr_of!(QUAKE_MONSTER_DESCRIPTOR),
            core::ptr::addr_of!(QUAKE_MONSTER_CALLS).cast::<PackedCall>(),
            PACKAGE_CHUNK,
            0x80408000,
        ),
    };
    let header = unsafe { core::ptr::read_volatile(header) };
    let count = header.calls as usize;
    if header.id != id
        || header.linked != linked
        || header.words == 0
        || header.words as usize > HALF_BYTES / 4
        || count == 0
        || count > MAX_CALLS
    {
        return Err(Error::Integrity);
    }
    let mut calls = [(0, 0); MAX_CALLS];
    for i in 0..count {
        let record = unsafe { core::ptr::read_volatile(records.add(i)) };
        if record.selector >= if image == Image::Monsters { 3 } else { 2 }
            || record.site & 3 != 0
            || record.offset & 3 != 0
            || !(0x80010000..0x80200000).contains(&record.site)
            || record.offset as usize >= HALF_BYTES
            || calls[..i].iter().any(|&(site, _)| site == record.site)
        {
            return Err(Error::Integrity);
        }
        calls[i] = (record.site, record.offset);
    }
    Ok(Metadata {
        words: header.words as usize,
        count,
        calls,
        descriptor: Descriptor {
            id: header.id,
            linked: header.linked,
            checksum: header.checksum,
        },
    })
}

#[inline(never)]
#[no_mangle]
pub fn quake_monster_call(
    runtime: &mut MonsterRuntime,
    elapsed: u16,
    input: MonsterThinkInput,
) -> Option<MonsterAction> {
    core::hint::black_box(quake_core::monster::quake_monster_advance(
        runtime, elapsed, input,
    ))
}

#[inline(never)]
#[no_mangle]
pub fn quake_door_call(
    scene: &mut EntityScene,
    world: &ResidentMap,
) -> Result<(), EntityLoadError> {
    core::hint::black_box(crate::entity::quake_door_link(scene, world))
}

#[inline(never)]
#[no_mangle]
pub fn quake_missile_call(
    scene: &mut EntityScene,
    world: &ResidentMap,
    mins: Vec3I32,
    maxs: Vec3I32,
    weapon: &mut WeaponState,
    elapsed: u16,
) -> Option<MonsterFrameResult> {
    core::hint::black_box(quake_monster_missiles(
        scene, world, mins, maxs, weapon, elapsed,
    ))
}

#[inline(never)]
#[no_mangle]
#[cfg_attr(target_arch = "mips", link_section = ".overlay.quake_monsters.text")]
pub fn quake_monster_missiles(
    scene: &mut EntityScene,
    world: &ResidentMap,
    mins: Vec3I32,
    maxs: Vec3I32,
    weapon: &mut WeaponState,
    elapsed: u16,
) -> Option<MonsterFrameResult> {
    scene.update_monster_missiles(world, mins, maxs, weapon, elapsed)
}

#[inline(never)]
#[no_mangle]
pub fn quake_bubble_call(
    particles: &mut ImpactParticles,
    world: &ResidentMap,
    elapsed: u16,
    origin: Vec3I32,
    water: u8,
) {
    core::hint::black_box(quake_bubble_tick(particles, world, elapsed, origin, water));
}

#[inline(never)]
#[no_mangle]
#[cfg_attr(target_arch = "mips", link_section = ".overlay.quake_monsters.text")]
pub fn quake_bubble_tick(
    particles: &mut ImpactParticles,
    world: &ResidentMap,
    elapsed: u16,
    origin: Vec3I32,
    water: u8,
) {
    particles.tick_death_bubbles(elapsed, origin, water, |point| {
        world
            .point_leaf_index(point)
            .and_then(|leaf| world.leaves().get(leaf))
            .is_some_and(|leaf| leaf.contents <= quake_core::movement::CONTENTS_WATER)
    });
}

#[inline(never)]
#[no_mangle]
pub fn quake_mover_call(
    source: MapEntity,
    model: BrushModel,
) -> Result<Option<QuakeMover>, QuakeMoverError> {
    core::hint::black_box(quake_core::mover::quake_mover_from_entity(source, model))
}

// A single concrete closure type keeps SDK admission/binding code shared.
// All entries remain direct, statically typed calls; no function pointer is stored.
enum Call<'a> {
    Bubbles {
        world: &'a ResidentMap,
        particles: &'a mut ImpactParticles,
        elapsed: u16,
        origin: Vec3I32,
        water: u8,
    },
    Doors {
        world: &'a ResidentMap,
        scene: &'a mut EntityScene,
        result: &'a mut Result<(), EntityLoadError>,
    },
    Monsters {
        world: &'a ResidentMap,
        scene: &'a mut EntityScene,
        origin: Vec3I32,
        velocity: Vec3I32,
        mins: Vec3I32,
        maxs: Vec3I32,
        hostile: bool,
        weapon: &'a mut WeaponState,
        elapsed: u16,
        result: &'a mut (Option<MonsterFrameResult>, Option<MonsterFrameResult>),
    },
}

#[inline(never)]
#[no_mangle]
fn quake_monster_unloaded() -> ! {
    psx_rt::tty::println("quake-psx: monster call outside code admission");
    psx_rt::halt()
}

pub struct MonsterCode {
    // This owns the world loader's allocation, never a newly allocated buffer.
    buffer: Vec<u8>,
    registry: Registry<1>,
    handle: Option<Handle>,
    map_generation: u32,
    image: Option<Image>,
}

impl MonsterCode {
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            registry: Registry::new(0x51, HALF_BYTES as u32),
            handle: None,
            map_generation: 0,
            image: None,
        }
    }

    /// The last call scope has ended before this mutable phase handoff.
    /// Incoming sites were trapped by its Drop. No asynchronous user owns any
    /// buffer byte; CPU-only monster actions contain values, never callbacks.
    pub fn release(&mut self, world: &mut ResidentMap) {
        self.registry.invalidate_all();
        self.handle = None;
        self.map_generation = 0;
        self.image = None;
        if self.buffer.capacity() != 0 {
            world.restore_stream_scratch(core::mem::take(&mut self.buffer));
        }
    }

    /// All texture CD/GPU and audio CD/SPU uploads must have completed. Package
    /// transport stops the CD before the shared installer publishes execution.
    pub fn install(&mut self, world: &mut ResidentMap, image: Image) -> Result<(), Error> {
        if self.handle.is_some() || self.buffer.capacity() != 0 {
            return Err(Error::NotReady);
        }
        let meta = metadata(image)?;
        let scratch = world.take_stream_scratch();
        self.buffer = scratch;
        if self.buffer.capacity() != BUFFER_BYTES {
            return Err(Error::Arena);
        }
        // Within the allocation already made at boot; this never grows it.
        self.buffer.resize(BUFFER_BYTES, 0);
        let address = self.buffer.as_ptr() as usize;
        if address & 3 != 0 {
            return Err(Error::Arena);
        }
        let words = unsafe {
            core::slice::from_raw_parts_mut(
                self.buffer.as_mut_ptr().cast::<u32>(),
                BUFFER_BYTES / 4,
            )
        };
        let (transport, code) = split_buffers(words)?;
        let payload_bytes = unsafe {
            core::slice::from_raw_parts_mut(transport.as_mut_ptr().cast::<u8>(), meta.words * 4)
        };
        if crate::platform::chunk_size(meta.descriptor.id).map_err(|_| Error::Integrity)?
            != payload_bytes.len() as u32
        {
            return Err(Error::Integrity);
        }
        crate::platform::read_chunk_exact(meta.descriptor.id, 0, payload_bytes)
            .map_err(|_| Error::Integrity)?;
        let code_address = code.as_ptr() as usize as u32;
        let mut arena = CallerArena::new(code, code_address)?;
        self.handle = Some(self.registry.install(
            &mut arena,
            &transport[..meta.words],
            meta.descriptor,
        )?);
        self.map_generation = world.generation();
        self.image = Some(image);
        Ok(())
    }

    pub fn load_entities(
        &mut self,
        world: &ResidentMap,
        scene: &mut EntityScene,
    ) -> Result<Result<(), EntityLoadError>, Error> {
        let mut result = Ok(());
        self.invoke(
            world.generation(),
            Image::Doors,
            &mut Call::Doors {
                world,
                scene,
                result: &mut result,
            },
        )?;
        Ok(result)
    }

    /// The bubble phase has its own short lease before any death/map reload.
    /// Both phases reuse the installed image: no CD, copy, relocation or new allocation.
    pub fn tick_bubbles(
        &mut self,
        world: &ResidentMap,
        particles: &mut ImpactParticles,
        elapsed: u16,
        origin: Vec3I32,
        water: u8,
    ) -> Result<(), Error> {
        if elapsed == 0 || !particles.has_death_bubble_work() {
            return Ok(());
        }
        self.invoke(
            world.generation(),
            Image::Monsters,
            &mut Call::Bubbles {
                world,
                particles,
                elapsed,
                origin,
                water,
            },
        )
    }

    /// One late lease covers missiles then the complete actor think batch.
    pub fn update_monsters(
        &mut self,
        world: &ResidentMap,
        scene: &mut EntityScene,
        origin: Vec3I32,
        velocity: Vec3I32,
        mins: Vec3I32,
        maxs: Vec3I32,
        hostile: bool,
        weapon: &mut WeaponState,
        elapsed: u16,
    ) -> Result<(Option<MonsterFrameResult>, Option<MonsterFrameResult>), Error> {
        let mut result = (None, None);
        self.invoke(
            world.generation(),
            Image::Monsters,
            &mut Call::Monsters {
                world,
                scene,
                origin,
                velocity,
                mins,
                maxs,
                hostile,
                weapon,
                elapsed,
                result: &mut result,
            },
        )?;
        Ok(result)
    }

    #[inline(never)]
    fn invoke(&mut self, generation: u32, image: Image, call: &mut Call<'_>) -> Result<(), Error> {
        let handle = self.handle.ok_or(Error::NotReady)?;
        if self.map_generation != generation
            || self.buffer.len() != BUFFER_BYTES
            || self.image != Some(image)
        {
            return Err(Error::Stale);
        }
        let meta = metadata(image)?;
        let words = unsafe {
            core::slice::from_raw_parts_mut(
                self.buffer.as_mut_ptr().cast::<u32>(),
                BUFFER_BYTES / 4,
            )
        };
        let (_, code) = split_buffers(words)?;
        let address = code.as_ptr() as usize as u32;
        let mut arena = CallerArena::new(code, address)?;
        let dependencies = [handle];
        let admission = self.registry.admit(&mut arena, &dependencies)?;
        let mut imports: [MaybeUninit<StaticImport<'_>>; MAX_CALLS] =
            core::array::from_fn(|_| MaybeUninit::uninit());
        for (slot, &(site, offset)) in imports.iter_mut().zip(&meta.calls[..meta.count]) {
            slot.write(unsafe {
                StaticImport::new(&mut *(site as *mut u32), site, handle, offset)
            });
        }
        let imports = unsafe {
            core::slice::from_raw_parts_mut(
                imports.as_mut_ptr().cast::<StaticImport<'_>>(),
                meta.count,
            )
        };
        // The caller enters the selected typed door or monster wrapper, with the same Rust
        // parameter/return types compiled into this package. Actions return by
        // value; neither pointers nor asynchronous work outlive the admission.
        unsafe {
            admission.invoke(
                imports,
                quake_monster_unloaded as *const () as usize as u32,
                || match call {
                    Call::Bubbles {
                        world,
                        particles,
                        elapsed,
                        origin,
                        water,
                    } => quake_bubble_call(particles, world, *elapsed, *origin, *water),
                    Call::Doors {
                        world,
                        scene,
                        result,
                    } => **result = scene.load(world),
                    Call::Monsters {
                        world,
                        scene,
                        origin,
                        velocity,
                        mins,
                        maxs,
                        hostile,
                        weapon,
                        elapsed,
                        result,
                    } => {
                        result.0 = quake_missile_call(scene, world, *mins, *maxs, weapon, *elapsed);
                        result.1 = scene.update_monsters(
                            world, *origin, *velocity, *mins, *maxs, *hostile, weapon, *elapsed,
                        );
                    }
                },
            )
        }
    }
}

impl Drop for MonsterCode {
    fn drop(&mut self) {
        // No admission can survive the mutable owner. Its incoming sites have
        // already been trapped before backing storage can be released.
        self.registry.invalidate_all();
    }
}
