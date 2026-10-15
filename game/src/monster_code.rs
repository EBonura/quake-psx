//! Map-admitted monster code uses the existing, quiescent 30 KiB transfer
//! allocation. It returns that entire allocation before any map/audio load.
//! No render packet, SPU sample or gameplay state is stored in this buffer.

use crate::asset::ResidentMap;
use crate::entity::{EntityLoadError, EntityScene, MonsterFrameResult};
use crate::savegame::{Header, Outcome};
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
pub const SAVE_PACKAGE_CHUNK: u32 = 6;
pub const LOAD_PACKAGE_CHUNK: u32 = 7;
pub const CARD_PACKAGE_CHUNK: u32 = 8;
/// A save's payload waits in the transport half behind whichever package was
/// read there last, so every save package must end before this offset (the
/// builder rejects one that does not).
pub const STAGING_OFFSET: usize = 10_240;
pub const STAGING_BYTES: usize = HALF_BYTES - STAGING_OFFSET;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Image {
    Doors,
    Monsters,
    /// The save encoder.
    Save,
    /// The save decoder.
    Load,
    /// The memory-card driver.
    Card,
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

#[used]
#[no_mangle]
#[cfg_attr(target_arch = "mips", link_section = ".data.quake_save_descriptor")]
static mut QUAKE_SAVE_DESCRIPTOR: PackedDescriptor = PackedDescriptor {
    words: 0,
    id: 0,
    linked: 0,
    checksum: 0,
    calls: 0,
};
#[used]
#[no_mangle]
#[cfg_attr(target_arch = "mips", link_section = ".data.quake_save_calls")]
static mut QUAKE_SAVE_CALLS: [PackedCall; MAX_CALLS] = [PackedCall {
    selector: 0,
    site: 0,
    offset: 0,
}; MAX_CALLS];

#[used]
#[no_mangle]
#[cfg_attr(target_arch = "mips", link_section = ".data.quake_load_descriptor")]
static mut QUAKE_LOAD_DESCRIPTOR: PackedDescriptor = PackedDescriptor {
    words: 0,
    id: 0,
    linked: 0,
    checksum: 0,
    calls: 0,
};
#[used]
#[no_mangle]
#[cfg_attr(target_arch = "mips", link_section = ".data.quake_load_calls")]
static mut QUAKE_LOAD_CALLS: [PackedCall; MAX_CALLS] = [PackedCall {
    selector: 0,
    site: 0,
    offset: 0,
}; MAX_CALLS];

#[used]
#[no_mangle]
#[cfg_attr(target_arch = "mips", link_section = ".data.quake_card_descriptor")]
static mut QUAKE_CARD_DESCRIPTOR: PackedDescriptor = PackedDescriptor {
    words: 0,
    id: 0,
    linked: 0,
    checksum: 0,
    calls: 0,
};
#[used]
#[no_mangle]
#[cfg_attr(target_arch = "mips", link_section = ".data.quake_card_calls")]
static mut QUAKE_CARD_CALLS: [PackedCall; MAX_CALLS] = [PackedCall {
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
        Image::Save => (
            core::ptr::addr_of!(QUAKE_SAVE_DESCRIPTOR),
            core::ptr::addr_of!(QUAKE_SAVE_CALLS).cast::<PackedCall>(),
            SAVE_PACKAGE_CHUNK,
            0x80428000,
        ),
        Image::Load => (
            core::ptr::addr_of!(QUAKE_LOAD_DESCRIPTOR),
            core::ptr::addr_of!(QUAKE_LOAD_CALLS).cast::<PackedCall>(),
            LOAD_PACKAGE_CHUNK,
            0x80438000,
        ),
        Image::Card => (
            core::ptr::addr_of!(QUAKE_CARD_DESCRIPTOR),
            core::ptr::addr_of!(QUAKE_CARD_CALLS).cast::<PackedCall>(),
            CARD_PACKAGE_CHUNK,
            0x80448000,
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

#[inline(never)]
#[no_mangle]
pub fn quake_save_call(
    buf: &mut [u8],
    world: &ResidentMap,
    entities: &EntityScene,
    player: &crate::player::Player,
    weapon: &WeaponState,
    presentation: &quake_core::level_session::LevelPresentation,
) -> Option<usize> {
    core::hint::black_box(crate::savegame::quake_save_encode(
        buf,
        world,
        entities,
        player,
        weapon,
        presentation,
    ))
}

#[inline(never)]
#[no_mangle]
pub fn quake_load_header_call(bytes: &[u8]) -> Option<Header> {
    core::hint::black_box(crate::savegame::quake_load_header(bytes))
}

#[inline(never)]
#[no_mangle]
pub fn quake_load_restore_call(
    bytes: &[u8],
    world: &ResidentMap,
    entities: &mut EntityScene,
    player: &mut crate::player::Player,
    weapon: &mut WeaponState,
    presentation: &mut quake_core::level_session::LevelPresentation,
) -> bool {
    core::hint::black_box(crate::savegame::quake_load_restore(
        bytes,
        world,
        entities,
        player,
        weapon,
        presentation,
    ))
}

#[inline(never)]
#[no_mangle]
pub fn quake_card_write_call(bytes: &[u8]) -> Outcome {
    core::hint::black_box(crate::savegame::quake_card_write(bytes))
}

#[inline(never)]
#[no_mangle]
pub fn quake_card_read_call(staging: &mut [u8]) -> Result<usize, Outcome> {
    core::hint::black_box(crate::savegame::quake_card_read(staging))
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
    // The save calls carry the staging area as a raw pointer: it lies inside
    // the buffer the admission itself borrows, in the half no package occupies.
    Encode {
        world: &'a ResidentMap,
        entities: &'a EntityScene,
        player: &'a crate::player::Player,
        weapon: &'a WeaponState,
        presentation: &'a quake_core::level_session::LevelPresentation,
        staging: *mut u8,
        result: &'a mut Option<usize>,
    },
    Header {
        staging: *mut u8,
        len: usize,
        result: &'a mut Option<Header>,
    },
    Restore {
        world: &'a ResidentMap,
        entities: &'a mut EntityScene,
        player: &'a mut crate::player::Player,
        weapon: &'a mut WeaponState,
        presentation: &'a mut quake_core::level_session::LevelPresentation,
        staging: *mut u8,
        len: usize,
        result: &'a mut bool,
    },
    CardWrite {
        staging: *mut u8,
        len: usize,
        result: &'a mut Outcome,
    },
    CardRead {
        staging: *mut u8,
        result: &'a mut Result<usize, Outcome>,
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
        let scratch = world.take_stream_scratch();
        self.buffer = scratch;
        if self.buffer.capacity() != BUFFER_BYTES {
            return Err(Error::Arena);
        }
        // Within the allocation already made at boot; this never grows it.
        self.buffer.resize(BUFFER_BYTES, 0);
        self.admit_image(world, image)
    }

    /// Replace the installed package with another in the same buffer without
    /// returning it to the loader. The transport half past `STAGING_OFFSET`
    /// keeps whatever the last package left there, which is how a save's
    /// payload survives from the encoder to the card driver.
    pub fn swap_image(&mut self, world: &ResidentMap, image: Image) -> Result<(), Error> {
        if self.buffer.len() != BUFFER_BYTES {
            return Err(Error::NotReady);
        }
        self.registry.invalidate_all();
        self.handle = None;
        self.image = None;
        self.admit_image(world, image)
    }

    fn admit_image(&mut self, world: &ResidentMap, image: Image) -> Result<(), Error> {
        let meta = metadata(image)?;
        let address = self.buffer.as_ptr() as usize;
        if address & 3 != 0 {
            return Err(Error::Arena);
        }
        if matches!(image, Image::Save | Image::Load | Image::Card)
            && meta.words * 4 > STAGING_OFFSET
        {
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

    /// The staging area: the tail of the transport half. Valid only while a
    /// package is installed, and never overlapping the code half or the
    /// payload the installer reads into the head of the transport half.
    fn staging(&mut self) -> Result<*mut u8, Error> {
        let words = unsafe {
            core::slice::from_raw_parts_mut(
                self.buffer.as_mut_ptr().cast::<u32>(),
                BUFFER_BYTES / 4,
            )
        };
        let (transport, _) = split_buffers(words)?;
        // SAFETY: the offset lies inside the transport half.
        Ok(unsafe { transport.as_mut_ptr().cast::<u8>().add(STAGING_OFFSET) })
    }

    /// `Host_Savegame_f`: encode the game into the staging area and write it
    /// to the card. The buffer is returned to the loader before this returns.
    pub fn save_game(
        &mut self,
        world: &mut ResidentMap,
        entities: &EntityScene,
        player: &crate::player::Player,
        weapon: &WeaponState,
        presentation: &quake_core::level_session::LevelPresentation,
    ) -> Outcome {
        let outcome = self.save_game_inner(world, entities, player, weapon, presentation);
        self.release(world);
        outcome
    }

    fn save_game_inner(
        &mut self,
        world: &mut ResidentMap,
        entities: &EntityScene,
        player: &crate::player::Player,
        weapon: &WeaponState,
        presentation: &quake_core::level_session::LevelPresentation,
    ) -> Outcome {
        let Some(len) = self.encode(world, entities, player, weapon, presentation) else {
            return Outcome::Failed;
        };
        let Ok(staging) = self.staging() else {
            return Outcome::Failed;
        };
        if self.swap_image(world, Image::Card).is_err() {
            return Outcome::Failed;
        }
        let mut outcome = Outcome::Failed;
        let call = Call::CardWrite {
            staging,
            len,
            result: &mut outcome,
        };
        let admitted = self.invoke(world.generation(), Image::Card, &mut { call });
        if admitted.is_ok() {
            outcome
        } else {
            Outcome::Failed
        }
    }

    /// Serialise into the staging area: releases whatever is installed, then
    /// leaves the encoder installed. The byte count, or `None` if the game
    /// did not fit or the package could not be admitted.
    pub fn encode(
        &mut self,
        world: &mut ResidentMap,
        entities: &EntityScene,
        player: &crate::player::Player,
        weapon: &WeaponState,
        presentation: &quake_core::level_session::LevelPresentation,
    ) -> Option<usize> {
        self.release(world);
        self.install(world, Image::Save).ok()?;
        let staging = self.staging().ok()?;
        let mut result = None;
        let call = Call::Encode {
            world,
            entities,
            player,
            weapon,
            presentation,
            staging,
            result: &mut result,
        };
        let admitted = self.invoke(world.generation(), Image::Save, &mut { call });
        admitted.ok()?;
        result
    }

    /// The length and checksum of the game as it would be saved, for the
    /// save and load regression's before and after comparison.
    #[cfg(feature = "e1m1-save-regression")]
    pub fn encode_fingerprint(
        &mut self,
        world: &mut ResidentMap,
        entities: &EntityScene,
        player: &crate::player::Player,
        weapon: &WeaponState,
        presentation: &quake_core::level_session::LevelPresentation,
    ) -> (usize, u32) {
        let encoded = self.encode(world, entities, player, weapon, presentation);
        let hash = match (encoded, self.staging()) {
            (Some(len), Ok(staging)) => {
                // SAFETY: `len` bytes were just written at the staging pointer.
                quake_core::save::checksum(unsafe { core::slice::from_raw_parts(staging, len) })
            }
            _ => 0,
        };
        self.release(world);
        (encoded.unwrap_or(0), hash)
    }

    /// Read the card into the staging area and, from the decoder, the header
    /// of what it holds. Leaves the buffer with the loader.
    pub fn card_header(&mut self, world: &mut ResidentMap) -> Result<Header, Outcome> {
        let outcome = self.card_header_inner(world);
        self.release(world);
        outcome
    }

    fn card_header_inner(&mut self, world: &mut ResidentMap) -> Result<Header, Outcome> {
        let len = self.read_card(world)?;
        let staging = self.staging().map_err(|_| Outcome::Failed)?;
        self.swap_image(world, Image::Load).map_err(|_| Outcome::Failed)?;
        let mut header = None;
        let call = Call::Header {
            staging,
            len,
            result: &mut header,
        };
        let admitted = self.invoke(world.generation(), Image::Load, &mut { call });
        admitted.map_err(|_| Outcome::Failed)?;
        header.ok_or(Outcome::Refused)
    }

    /// Install the card driver, read the save into the staging area and
    /// return its length. Leaves the card driver installed.
    fn read_card(&mut self, world: &mut ResidentMap) -> Result<usize, Outcome> {
        self.release(world);
        self.install(world, Image::Card).map_err(|_| Outcome::Failed)?;
        let staging = self.staging().map_err(|_| Outcome::Failed)?;
        let mut result = Err(Outcome::Failed);
        let call = Call::CardRead {
            staging,
            result: &mut result,
        };
        let admitted = self.invoke(world.generation(), Image::Card, &mut { call });
        admitted.map_err(|_| Outcome::Failed)?;
        result
    }

    /// Read the card again and put what it holds into a level that the
    /// header has just loaded. `false` means the save did not fit the level.
    pub fn restore_game(
        &mut self,
        world: &mut ResidentMap,
        entities: &mut EntityScene,
        player: &mut crate::player::Player,
        weapon: &mut WeaponState,
        presentation: &mut quake_core::level_session::LevelPresentation,
    ) -> bool {
        let restored = self.restore_game_inner(world, entities, player, weapon, presentation);
        self.release(world);
        restored
    }

    fn restore_game_inner(
        &mut self,
        world: &mut ResidentMap,
        entities: &mut EntityScene,
        player: &mut crate::player::Player,
        weapon: &mut WeaponState,
        presentation: &mut quake_core::level_session::LevelPresentation,
    ) -> bool {
        let Ok(len) = self.read_card(world) else {
            return false;
        };
        let Ok(staging) = self.staging() else {
            return false;
        };
        if self.swap_image(world, Image::Load).is_err() {
            return false;
        }
        let mut restored = false;
        let call = Call::Restore {
            world,
            entities,
            player,
            weapon,
            presentation,
            staging,
            len,
            result: &mut restored,
        };
        let admitted = self.invoke(world.generation(), Image::Load, &mut { call });
        admitted.is_ok() && restored
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
                    Call::Encode {
                        world,
                        entities,
                        player,
                        weapon,
                        presentation,
                        staging,
                        result,
                    } => {
                        let buf = core::slice::from_raw_parts_mut(*staging, STAGING_BYTES);
                        **result =
                            quake_save_call(buf, world, entities, player, weapon, presentation);
                    }
                    Call::Header {
                        staging,
                        len,
                        result,
                    } => {
                        let bytes = core::slice::from_raw_parts(*staging, *len);
                        **result = quake_load_header_call(bytes);
                    }
                    Call::Restore {
                        world,
                        entities,
                        player,
                        weapon,
                        presentation,
                        staging,
                        len,
                        result,
                    } => {
                        let bytes = core::slice::from_raw_parts(*staging, *len);
                        **result = quake_load_restore_call(
                            bytes,
                            world,
                            entities,
                            player,
                            weapon,
                            presentation,
                        );
                    }
                    Call::CardWrite {
                        staging,
                        len,
                        result,
                    } => {
                        let bytes = core::slice::from_raw_parts(*staging, *len);
                        **result = quake_card_write_call(bytes);
                    }
                    Call::CardRead { staging, result } => {
                        let buf = core::slice::from_raw_parts_mut(*staging, STAGING_BYTES);
                        **result = quake_card_read_call(buf);
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
