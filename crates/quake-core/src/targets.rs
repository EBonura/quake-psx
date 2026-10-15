//! Allocation-free Quake target/use dispatch shared by host tests and guest gameplay.

use quake_formats::{MapEntity, RecordSlice};

use crate::monster::MonsterKind;

pub const MAX_TARGET_ENTITIES: usize = 512;
pub const MAX_TARGET_ACTIONS: usize = 128;
pub const MAX_PENDING_USES: usize = 128;
pub const MAX_DELAYED_USES: usize = 32;

const CLASS_EVENT_LIGHTNING: u8 = 0x09;
const CLASS_FUNC_BUTTON: u8 = 0x0b;
/// `light`. Its authored `style` key is cooked into `count`
/// (`quake-cook/src/entities.rs`), and the flame and torch classes below share
/// its use function in the original.
const CLASS_LIGHT: u8 = 0x2a;
const CLASS_LIGHT_FLAME_LARGE_YELLOW: u8 = 0x2b;
const CLASS_LIGHT_FLAME_SMALL_WHITE: u8 = 0x2c;
const CLASS_LIGHT_FLAME_SMALL_YELLOW: u8 = 0x2d;
const CLASS_LIGHT_TORCH_SMALL_WALLTORCH: u8 = 0x31;

/// Every authored class the original's `light_use` is installed on.
pub const fn is_light_class(class_name: u8) -> bool {
    matches!(
        class_name,
        CLASS_LIGHT
            | CLASS_LIGHT_FLAME_LARGE_YELLOW
            | CLASS_LIGHT_FLAME_SMALL_WHITE
            | CLASS_LIGHT_FLAME_SMALL_YELLOW
            | CLASS_LIGHT_TORCH_SMALL_WALLTORCH
    )
}

const CLASS_FUNC_DOOR: u8 = 0x0c;
const CLASS_FUNC_DOOR_SECRET: u8 = 0x0d;
const CLASS_FUNC_PLAT: u8 = 0x10;
const CLASS_FUNC_TRAIN: u8 = 0x11;
const CLASS_FUNC_WALL: u8 = 0x12;
const CLASS_MONSTER_BOSS: u8 = 0x37;
const CLASS_TRAP_SPIKESHOOTER: u8 = 0x46;
const CLASS_TRIGGER_COUNTER: u8 = 0x48;
const CLASS_TRIGGER_MULTIPLE: u8 = 0x4b;
const CLASS_TRIGGER_ONCE: u8 = 0x4c;
const CLASS_TRIGGER_RELAY: u8 = 0x4f;
const CLASS_TRIGGER_SECRET: u8 = 0x50;
const CLASS_TRIGGER_TELEPORT: u8 = 0x52;

/// `SPAWNFLAG_NOMESSAGE` on a `trigger_counter`: run the count silently.
const COUNTER_NO_MESSAGE: u16 = 1;

/// `counter_use`'s countdown line, keyed on the count left after the
/// decrement, exactly as the original tests `self.count` once it is spent.
const fn counter_message(remaining: i16) -> Option<&'static str> {
    Some(match remaining {
        0 => "Sequence completed!",
        1 => "Only 1 more to go...",
        2 => "Only 2 more to go...",
        3 => "Only 3 more to go...",
        _ => "There are more to go...",
    })
}

/// `trigger_changelevel` bit 1, `NO_INTERMISSION`. `changelevel_touch` ends
/// with `if (self.spawnflags & 1) { GotoNextMap(); return; }`, so the next map
/// loads straight away with no end-of-level panel in between.
pub const CHANGELEVEL_NO_INTERMISSION: u16 = 1;

pub const SPAWNFLAG_NOT_EASY: u16 = 0x0100;
pub const SPAWNFLAG_NOT_MEDIUM: u16 = 0x0200;
pub const SPAWNFLAG_NOT_HARD: u16 = 0x0400;

/// Original Quake entity-loader skill exclusion. Skill 0 (easy) honors
/// `SPAWNFLAG_NOT_EASY`, skill 1 (normal) `SPAWNFLAG_NOT_MEDIUM`, and skills
/// 2 and 3 (hard and nightmare share one bit) `SPAWNFLAG_NOT_HARD`.
pub const fn excluded_for_skill(spawn_flags: u16, skill: u8) -> bool {
    let exclusion = match skill {
        0 => SPAWNFLAG_NOT_EASY,
        1 => SPAWNFLAG_NOT_MEDIUM,
        _ => SPAWNFLAG_NOT_HARD,
    };
    spawn_flags & exclusion != 0
}

/// `trigger_skill_touch`'s `cvar_set ("skill", self.message)`.
///
/// The cooked message is the authored decimal digit. Anything else leaves the
/// current skill alone, exactly like a `cvar_set` of a non-numeric string.
pub fn parse_setskill(message: &[u8]) -> Option<u8> {
    message
        .first()
        .copied()
        .and_then(|digit| digit.checked_sub(b'0'))
        .filter(|skill| *skill <= 3)
}

pub trait TargetEntitySource {
    fn entity_count(&self) -> usize;
    fn entity_at(&self, index: usize) -> Option<MapEntity>;
}

impl TargetEntitySource for [MapEntity] {
    fn entity_count(&self) -> usize {
        self.len()
    }

    fn entity_at(&self, index: usize) -> Option<MapEntity> {
        self.get(index).copied()
    }
}

impl TargetEntitySource for RecordSlice<'_, MapEntity> {
    fn entity_count(&self) -> usize {
        self.len()
    }

    fn entity_at(&self, index: usize) -> Option<MapEntity> {
        self.get(index)
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TargetError {
    TooManyEntities,
    TooManyActions,
    TooManyPendingUses,
    TooManyDelayedUses,
    MissingSource,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TargetActivator {
    None,
    Player,
    Entity(u16),
}

impl Default for TargetActivator {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TargetAction {
    Activate(u16, TargetActivator),
    Disable(u16),
    EnableTeleport(u16),
    ToggleWall(u16),
    /// `monster_boss` use: raise Chthon and give him his skill shock count.
    AwakenMonster(u16, TargetActivator),
    /// `monster_use`: a targeted ordinary monster takes the activator as its
    /// enemy and hunts. The original ignores every activator but a living,
    /// visible player; the host applies that to the activator it is handed.
    WakeMonster(u16, TargetActivator),
    /// `event_lightning` use: one shock of the authored Chthon kill chain.
    ShockBoss(u16),
    FireShooter(u16),
    /// `light_use`: flip an authored switchable light style on or off. The
    /// original writes `"m"` or `"a"` into that style and remembers the new
    /// state in the light's own `START_OFF` spawnflag.
    ToggleLight(u16),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TargetActions {
    entries: [Option<TargetAction>; MAX_TARGET_ACTIONS],
    len: usize,
    fired_edges: u16,
    completed_counters: u16,
    counter_message: Option<&'static str>,
}

impl TargetActions {
    pub const fn new() -> Self {
        Self {
            entries: [None; MAX_TARGET_ACTIONS],
            len: 0,
            fired_edges: 0,
            completed_counters: 0,
            counter_message: None,
        }
    }

    pub fn clear(&mut self) {
        self.entries[..self.len].fill(None);
        self.len = 0;
        self.fired_edges = 0;
        self.completed_counters = 0;
        self.counter_message = None;
    }

    pub fn iter(&self) -> impl Iterator<Item = TargetAction> + '_ {
        self.entries[..self.len].iter().flatten().copied()
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub const fn fired_edges(&self) -> u16 {
        self.fired_edges
    }

    pub const fn completed_counters(&self) -> u16 {
        self.completed_counters
    }

    pub const fn counter_message(&self) -> Option<&'static str> {
        self.counter_message
    }

    fn push(&mut self, action: TargetAction) -> Result<(), TargetError> {
        let Some(slot) = self.entries.get_mut(self.len) else {
            return Err(TargetError::TooManyActions);
        };
        *slot = Some(action);
        self.len += 1;
        Ok(())
    }
}

impl Default for TargetActions {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct UseCommand {
    target: u16,
    kill_target: u16,
    activator: TargetActivator,
}

/// The first unused delayed-use slot.
///
/// Taken over a slice with an opaque bound and kept out of line on purpose.
/// Written as `self.delayed.iter_mut().find(..)` the fixed 32-entry length let
/// LLVM unroll the scan whole, once into every caller of `enqueue_or_delay`,
/// and that unrolled scan was the largest single block of `apply_command`.
#[cold]
#[inline(never)]
fn free_delayed_slot(delayed: &[Option<DelayedUse>]) -> Option<usize> {
    let count = core::hint::black_box(delayed.len());
    delayed[..count].iter().position(|slot| slot.is_none())
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct DelayedUse {
    remaining_ticks: u16,
    command: UseCommand,
}

pub struct TargetGraph {
    disabled: [bool; MAX_TARGET_ENTITIES],
    counters: [i16; MAX_TARGET_ENTITIES],
    cooldowns: [u16; MAX_TARGET_ENTITIES],
    delayed: [Option<DelayedUse>; MAX_DELAYED_USES],
    entity_count: usize,
}

impl TargetGraph {
    pub const fn new() -> Self {
        Self {
            disabled: [false; MAX_TARGET_ENTITIES],
            counters: [0; MAX_TARGET_ENTITIES],
            cooldowns: [0; MAX_TARGET_ENTITIES],
            delayed: [None; MAX_DELAYED_USES],
            entity_count: 0,
        }
    }

    pub fn load<S: TargetEntitySource + ?Sized>(&mut self, source: &S) -> Result<(), TargetError> {
        let count = source.entity_count();
        if count > MAX_TARGET_ENTITIES {
            return Err(TargetError::TooManyEntities);
        }
        self.disabled.fill(false);
        self.counters.fill(0);
        self.cooldowns.fill(0);
        self.delayed.fill(None);
        self.entity_count = count;
        for index in 0..count {
            let entity = source.entity_at(index).ok_or(TargetError::MissingSource)?;
            if entity.class_name == CLASS_TRIGGER_COUNTER {
                self.counters[index] = if entity.count == 0 { 2 } else { entity.count };
            }
        }
        Ok(())
    }

    pub fn fire_source<S: TargetEntitySource + ?Sized>(
        &mut self,
        source: &S,
        source_index: u16,
        actions: &mut TargetActions,
    ) -> Result<(), TargetError> {
        self.fire_source_by(source, source_index, TargetActivator::None, actions)
    }

    /// Kept out of line: it has two guest callers, and once `apply_command`
    /// stopped unrolling its delayed-slot scan this became small enough for
    /// LLVM to copy the whole use-dispatch prologue into both of them.
    #[inline(never)]
    pub fn fire_source_by<S: TargetEntitySource + ?Sized>(
        &mut self,
        source: &S,
        source_index: u16,
        activator: TargetActivator,
        actions: &mut TargetActions,
    ) -> Result<(), TargetError> {
        let index = source_index as usize;
        if index >= self.entity_count || self.disabled[index] {
            return Ok(());
        }
        let entity = source.entity_at(index).ok_or(TargetError::MissingSource)?;
        if !self.begin_trigger_use(index, entity) {
            return Ok(());
        }
        self.enqueue_or_delay(
            UseCommand {
                target: entity.target,
                kill_target: entity.kill_target,
                activator,
            },
            entity.delay,
            source,
            actions,
        )
    }

    pub fn tick<S: TargetEntitySource + ?Sized>(
        &mut self,
        elapsed_ticks: u16,
        source: &S,
        actions: &mut TargetActions,
    ) -> Result<(), TargetError> {
        let ticks = elapsed_ticks.max(1);
        for cooldown in &mut self.cooldowns[..self.entity_count] {
            *cooldown = cooldown.saturating_sub(ticks);
        }
        let mut due = [None; MAX_DELAYED_USES];
        let mut due_count = 0usize;
        for slot in &mut self.delayed {
            let Some(mut delayed) = *slot else {
                continue;
            };
            if delayed.remaining_ticks > ticks {
                delayed.remaining_ticks -= ticks;
                *slot = Some(delayed);
            } else {
                *slot = None;
                due[due_count] = Some(delayed.command);
                due_count += 1;
            }
        }
        for command in due[..due_count].iter().flatten().copied() {
            self.apply_command(command, source, actions)?;
        }
        Ok(())
    }

    pub fn is_enabled(&self, source_index: u16) -> bool {
        let index = source_index as usize;
        index < self.entity_count && !self.disabled[index]
    }

    pub fn disable_entity(&mut self, source_index: u16) -> Result<(), TargetError> {
        let index = source_index as usize;
        if index >= self.entity_count {
            return Err(TargetError::MissingSource);
        }
        self.disabled[index] = true;
        Ok(())
    }

    pub fn counter_remaining(&self, source_index: u16) -> Option<i16> {
        let index = source_index as usize;
        (index < self.entity_count).then_some(self.counters[index])
    }

    fn enqueue_or_delay<S: TargetEntitySource + ?Sized>(
        &mut self,
        command: UseCommand,
        delay_q12: i32,
        source: &S,
        actions: &mut TargetActions,
    ) -> Result<(), TargetError> {
        if delay_q12 > 0 {
            let Some(slot) = free_delayed_slot(&self.delayed) else {
                return Err(TargetError::TooManyDelayedUses);
            };
            self.delayed[slot] = Some(DelayedUse {
                remaining_ticks: fixed_seconds_to_ticks(delay_q12),
                command,
            });
            Ok(())
        } else {
            self.apply_command(command, source, actions)
        }
    }

    fn apply_command<S: TargetEntitySource + ?Sized>(
        &mut self,
        first: UseCommand,
        source: &S,
        actions: &mut TargetActions,
    ) -> Result<(), TargetError> {
        let mut queue = [None; MAX_PENDING_USES];
        queue[0] = Some(first);
        let mut read = 0usize;
        let mut write = 1usize;
        while read < write {
            let command = queue[read].take().expect("queued target use");
            read += 1;

            if command.kill_target != 0 {
                for index in 0..self.entity_count {
                    if self.disabled[index] {
                        continue;
                    }
                    let entity = source.entity_at(index).ok_or(TargetError::MissingSource)?;
                    if entity.target_name == command.kill_target {
                        self.disabled[index] = true;
                        actions.push(TargetAction::Disable(index as u16))?;
                    }
                }
            }

            if command.target == 0 {
                continue;
            }
            for index in 0..self.entity_count {
                if self.disabled[index] {
                    continue;
                }
                let entity = source.entity_at(index).ok_or(TargetError::MissingSource)?;
                if entity.target_name != command.target {
                    continue;
                }
                match entity.class_name {
                    CLASS_TRIGGER_RELAY => {
                        actions.fired_edges = actions.fired_edges.saturating_add(1);
                        self.queue_entity_command(
                            entity,
                            command.activator,
                            &mut queue,
                            &mut write,
                            source,
                            actions,
                        )?
                    }
                    CLASS_TRIGGER_MULTIPLE | CLASS_TRIGGER_ONCE | CLASS_TRIGGER_SECRET => {
                        if self.begin_trigger_use(index, entity) {
                            actions.fired_edges = actions.fired_edges.saturating_add(1);
                            self.queue_entity_command(
                                entity,
                                command.activator,
                                &mut queue,
                                &mut write,
                                source,
                                actions,
                            )?;
                        }
                    }
                    CLASS_TRIGGER_COUNTER => {
                        if self.counters[index] > 0 {
                            actions.fired_edges = actions.fired_edges.saturating_add(1);
                            self.counters[index] -= 1;
                            // `counter_use` centerprints the countdown only
                            // for a player activator, and only when the
                            // counter is not authored NOMESSAGE.
                            if matches!(command.activator, TargetActivator::Player)
                                && entity.spawn_flags & COUNTER_NO_MESSAGE == 0
                            {
                                actions.counter_message = counter_message(self.counters[index]);
                            }
                            if self.counters[index] == 0 {
                                actions.completed_counters =
                                    actions.completed_counters.saturating_add(1);
                                self.queue_entity_command(
                                    entity,
                                    command.activator,
                                    &mut queue,
                                    &mut write,
                                    source,
                                    actions,
                                )?;
                            }
                        }
                    }
                    CLASS_FUNC_BUTTON
                    | CLASS_FUNC_DOOR
                    | CLASS_FUNC_DOOR_SECRET
                    | CLASS_FUNC_PLAT
                    | CLASS_FUNC_TRAIN => {
                        actions.fired_edges = actions.fired_edges.saturating_add(1);
                        actions.push(TargetAction::Activate(index as u16, command.activator))?;
                    }
                    CLASS_FUNC_WALL => {
                        actions.fired_edges = actions.fired_edges.saturating_add(1);
                        actions.push(TargetAction::ToggleWall(index as u16))?;
                    }
                    CLASS_TRIGGER_TELEPORT => {
                        actions.fired_edges = actions.fired_edges.saturating_add(1);
                        actions.push(TargetAction::EnableTeleport(index as u16))?;
                    }
                    // The minimal Chthon encounter dispatch. Generic target
                    // handling is owned elsewhere; these two recipients exist
                    // because E1M7's boss has no other way to be raised or
                    // killed.
                    CLASS_MONSTER_BOSS => {
                        actions.fired_edges = actions.fired_edges.saturating_add(1);
                        actions
                            .push(TargetAction::AwakenMonster(index as u16, command.activator))?;
                    }
                    CLASS_EVENT_LIGHTNING => {
                        actions.fired_edges = actions.fired_edges.saturating_add(1);
                        actions.push(TargetAction::ShockBoss(index as u16))?;
                    }
                    CLASS_TRAP_SPIKESHOOTER => {
                        actions.fired_edges = actions.fired_edges.saturating_add(1);
                        actions.push(TargetAction::FireShooter(index as u16))?;
                    }
                    class if is_light_class(class) => {
                        actions.fired_edges = actions.fired_edges.saturating_add(1);
                        actions.push(TargetAction::ToggleLight(index as u16))?;
                    }
                    class if MonsterKind::from_class_name(class).is_some() => {
                        actions.fired_edges = actions.fired_edges.saturating_add(1);
                        actions.push(TargetAction::WakeMonster(index as u16, command.activator))?;
                    }
                    _ => {
                        // A target edge still fired even when this checkpoint
                        // does not yet model the recipient's class-specific
                        // use.
                        actions.fired_edges = actions.fired_edges.saturating_add(1);
                    }
                }
            }
        }
        Ok(())
    }

    fn begin_trigger_use(&mut self, index: usize, entity: MapEntity) -> bool {
        match entity.class_name {
            CLASS_TRIGGER_ONCE | CLASS_TRIGGER_SECRET => {
                self.disabled[index] = true;
            }
            CLASS_TRIGGER_MULTIPLE => {
                if self.cooldowns[index] != 0 {
                    return false;
                }
                self.cooldowns[index] = if entity.wait > 0 {
                    fixed_seconds_to_ticks(entity.wait)
                } else {
                    12
                };
            }
            _ => {}
        }
        true
    }

    fn queue_entity_command<S: TargetEntitySource + ?Sized>(
        &mut self,
        entity: MapEntity,
        activator: TargetActivator,
        queue: &mut [Option<UseCommand>; MAX_PENDING_USES],
        write: &mut usize,
        source: &S,
        actions: &mut TargetActions,
    ) -> Result<(), TargetError> {
        let command = UseCommand {
            target: entity.target,
            kill_target: entity.kill_target,
            activator,
        };
        if entity.delay > 0 {
            self.enqueue_or_delay(command, entity.delay, source, actions)
        } else {
            let Some(slot) = queue.get_mut(*write) else {
                return Err(TargetError::TooManyPendingUses);
            };
            *slot = Some(command);
            *write += 1;
            Ok(())
        }
    }
}

impl Default for TargetGraph {
    fn default() -> Self {
        Self::new()
    }
}

fn fixed_seconds_to_ticks(value: i32) -> u16 {
    let whole = value >> 12;
    let fraction = value & 0x0fff;
    whole
        .saturating_mul(60)
        .saturating_add(fraction.saturating_mul(60) >> 12)
        .clamp(1, i32::from(u16::MAX)) as u16
}

/// Write an activator: none, the player, or a scene entity.
pub fn save_activator(w: &mut crate::save::Writer, activator: TargetActivator) {
    match activator {
        TargetActivator::None => w.u8(0),
        TargetActivator::Player => w.u8(1),
        TargetActivator::Entity(index) => {
            w.u8(2);
            w.u16(index);
        }
    }
}

/// The inverse of [`save_activator`].
pub fn load_activator(r: &mut crate::save::Reader) -> TargetActivator {
    match r.below(3) {
        0 => TargetActivator::None,
        1 => TargetActivator::Player,
        _ => TargetActivator::Entity(r.u16()),
    }
}

impl TargetGraph {
    /// Write the target graph's moving parts sparsely: the disabled entities,
    /// the counters and cooldowns that are not at rest, and every pending
    /// delayed use. The graph's shape comes back from the map.
    pub fn save_dynamic(&self, w: &mut crate::save::Writer) {
        w.u16(self.entity_count as u16);
        let count = |keep: &dyn Fn(usize) -> bool| -> u16 {
            (0..self.entity_count).filter(|&index| keep(index)).count() as u16
        };
        w.u16(count(&|index| self.disabled[index]));
        for index in (0..self.entity_count).filter(|&index| self.disabled[index]) {
            w.u16(index as u16);
        }
        // Every counter and cooldown is written, zero runs coded as a zero
        // and a length: a `trigger_counter` that has counted down to zero must
        // come back at zero, not at the authored count a fresh load gives it.
        crate::save::write_words(w, &self.counters[..self.entity_count], |v| v as u16);
        crate::save::write_words(w, &self.cooldowns[..self.entity_count], |v| v);
        w.u8(self.delayed.iter().flatten().count() as u8);
        for (slot, delayed) in self.delayed.iter().enumerate() {
            let Some(delayed) = delayed else {
                continue;
            };
            w.u8(slot as u8);
            w.u16(delayed.remaining_ticks);
            w.u16(delayed.command.target);
            w.u16(delayed.command.kill_target);
            save_activator(w, delayed.command.activator);
        }
    }

    /// Put a graph freshly loaded from the same map back where
    /// [`Self::save_dynamic`] found it.
    pub fn load_dynamic(&mut self, r: &mut crate::save::Reader) {
        if usize::from(r.u16()) != self.entity_count {
            // A save from another map or another cook.
            r.reject();
            return;
        }
        let limit = self.entity_count;
        let index = |r: &mut crate::save::Reader| -> usize {
            let index = usize::from(r.u16());
            if index >= limit {
                r.reject();
                return 0;
            }
            index
        };
        for _ in 0..r.u16() {
            let at = index(r);
            self.disabled[at] = true;
        }
        crate::save::read_words(r, &mut self.counters[..limit], |v| v as i16);
        crate::save::read_words(r, &mut self.cooldowns[..limit], |v| v);
        self.delayed = [None; MAX_DELAYED_USES];
        for _ in 0..r.u8() {
            let slot = usize::from(r.u8());
            let remaining_ticks = r.u16();
            let target = r.u16();
            let kill_target = r.u16();
            let activator = load_activator(r);
            match self.delayed.get_mut(slot) {
                Some(entry) => {
                    *entry = Some(DelayedUse {
                        remaining_ticks,
                        command: UseCommand {
                            target,
                            kill_target,
                            activator,
                        },
                    });
                }
                None => r.reject(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    /// A graph's counters, cooldowns, disabled entities and pending delayed
    /// uses come back exactly, and a save from a graph of another size is
    /// refused.
    #[test]
    fn a_saved_target_graph_restores_every_moving_part() {
        use crate::save::{Reader, Writer};
        let mut graph = TargetGraph::new();
        graph.entity_count = 100;
        graph.disabled[3] = true;
        graph.disabled[99] = true;
        graph.counters[7] = -2;
        graph.counters[8] = 3;
        graph.cooldowns[50] = 9;
        graph.delayed[2] = Some(DelayedUse {
            remaining_ticks: 77,
            command: UseCommand {
                target: 5,
                kill_target: 6,
                activator: TargetActivator::Entity(40),
            },
        });
        graph.delayed[31] = Some(DelayedUse {
            remaining_ticks: 1,
            command: UseCommand {
                target: 1,
                kill_target: 0,
                activator: TargetActivator::Player,
            },
        });
        let mut fresh = TargetGraph::new();
        fresh.entity_count = 100;
        // A fresh load gives a counter its authored count; the save says it ran out.
        fresh.counters[9] = 2;
        graph.counters[9] = 0;
        let mut buf = [0u8; 128];
        let mut w = Writer::new(&mut buf);
        graph.save_dynamic(&mut w);
        let len = w.len();
        let mut r = Reader::raw(&buf[..len]);
        fresh.load_dynamic(&mut r);
        assert!(r.finish());
        assert_eq!(fresh.disabled[..100], graph.disabled[..100]);
        assert_eq!(fresh.counters[..100], graph.counters[..100]);
        assert_eq!(fresh.cooldowns[..100], graph.cooldowns[..100]);
        assert_eq!(fresh.delayed, graph.delayed);

        let mut other = TargetGraph::new();
        other.entity_count = 99;
        let mut r = Reader::raw(&buf[..len]);
        other.load_dynamic(&mut r);
        assert!(r.is_bad());
        // An index outside the graph is refused too.
        let mut bad = TargetGraph::new();
        bad.entity_count = 100;
        let mut bytes = [0u8; 16];
        bytes[..2].copy_from_slice(&100u16.to_le_bytes());
        bytes[2..4].copy_from_slice(&1u16.to_le_bytes());
        bytes[4..6].copy_from_slice(&100u16.to_le_bytes());
        let mut r = Reader::raw(&bytes);
        bad.load_dynamic(&mut r);
        assert!(r.is_bad());
    }

    extern crate std;

    use super::*;

    fn entity(class_name: u8, target: u16, target_name: u16) -> MapEntity {
        MapEntity {
            class_name,
            target,
            target_name,
            ..MapEntity::default()
        }
    }

    #[test]
    fn delayed_relay_killtarget_and_counter_preserve_quake_order() {
        let mut source = [MapEntity::default(); 6];
        source[0] = MapEntity {
            class_name: CLASS_TRIGGER_RELAY,
            target: 1,
            kill_target: 9,
            delay: 2_048,
            ..MapEntity::default()
        };
        source[1] = entity(CLASS_TRIGGER_RELAY, 2, 1);
        source[2] = MapEntity {
            class_name: CLASS_TRIGGER_COUNTER,
            target: 3,
            target_name: 2,
            count: 2,
            ..MapEntity::default()
        };
        source[3] = entity(CLASS_FUNC_DOOR, 0, 3);
        source[4] = entity(0x36, 0, 9);
        source[5] = entity(0x36, 0, 9);

        let mut graph = TargetGraph::new();
        graph.load(&source[..]).unwrap();
        let mut actions = TargetActions::new();
        graph
            .fire_source_by(&source[..], 0, TargetActivator::Player, &mut actions)
            .unwrap();
        assert!(actions.is_empty());
        graph.tick(29, &source[..], &mut actions).unwrap();
        assert!(actions.is_empty());
        graph.tick(1, &source[..], &mut actions).unwrap();
        assert_eq!(
            actions.iter().collect::<std::vec::Vec<_>>(),
            [TargetAction::Disable(4), TargetAction::Disable(5)]
        );
        assert_eq!(actions.fired_edges(), 2);
        assert_eq!(graph.counter_remaining(2), Some(1));
        assert_eq!(actions.counter_message(), Some("Only 1 more to go..."));

        actions.clear();
        graph
            .fire_source_by(&source[..], 0, TargetActivator::Player, &mut actions)
            .unwrap();
        graph.tick(30, &source[..], &mut actions).unwrap();
        assert_eq!(
            actions.iter().collect::<std::vec::Vec<_>>(),
            [TargetAction::Activate(3, TargetActivator::Player)]
        );
        assert_eq!(actions.completed_counters(), 1);
        assert_eq!(graph.counter_remaining(2), Some(0));
        assert_eq!(actions.counter_message(), Some("Sequence completed!"));
    }

    #[test]
    fn changelevel_use_reaches_the_e1m7_style_finale_relay() {
        let mut source = [MapEntity::default(); 8];
        source[0] = entity(0x47, 18, 0);
        source[1] = entity(CLASS_TRIGGER_RELAY, 16, 18);
        for entity in &mut source[2..7] {
            *entity = MapEntity {
                class_name: CLASS_TRIGGER_ONCE,
                target_name: 16,
                delay: 819,
                ..MapEntity::default()
            };
        }
        source[2].target = 17;
        source[7] = entity(CLASS_FUNC_DOOR, 0, 17);

        let mut graph = TargetGraph::new();
        graph.load(&source[..]).unwrap();
        let mut actions = TargetActions::new();
        graph
            .fire_source_by(&source[..], 0, TargetActivator::Player, &mut actions)
            .unwrap();
        // Exit -> relay, then relay -> all five finale triggers. Their own
        // delayed uses have been scheduled but have not matured yet.
        assert_eq!(actions.fired_edges(), 6);
        assert!(actions.is_empty());

        actions.clear();
        graph
            .tick(fixed_seconds_to_ticks(819), &source[..], &mut actions)
            .unwrap();
        assert_eq!(
            actions.iter().collect::<std::vec::Vec<_>>(),
            [TargetAction::Activate(7, TargetActivator::Player)]
        );
    }

    #[test]
    fn counter_defaults_to_two_and_does_not_refire_after_completion() {
        let source = [
            entity(CLASS_TRIGGER_RELAY, 1, 0),
            MapEntity {
                class_name: CLASS_TRIGGER_COUNTER,
                target: 2,
                target_name: 1,
                ..MapEntity::default()
            },
            entity(CLASS_FUNC_BUTTON, 0, 2),
        ];
        let mut graph = TargetGraph::new();
        graph.load(&source[..]).unwrap();
        let mut actions = TargetActions::new();
        graph.fire_source(&source[..], 0, &mut actions).unwrap();
        assert!(actions.is_empty());
        graph.fire_source(&source[..], 0, &mut actions).unwrap();
        assert_eq!(
            actions.iter().collect::<std::vec::Vec<_>>(),
            [TargetAction::Activate(2, TargetActivator::None)]
        );
        actions.clear();
        graph.fire_source(&source[..], 0, &mut actions).unwrap();
        assert!(actions.is_empty());
    }

    #[test]
    fn killtarget_removes_an_entity_before_the_same_target_can_use_it() {
        let source = [
            MapEntity {
                class_name: CLASS_TRIGGER_ONCE,
                target: 7,
                kill_target: 7,
                ..MapEntity::default()
            },
            entity(CLASS_FUNC_DOOR, 0, 7),
        ];
        let mut graph = TargetGraph::new();
        graph.load(&source[..]).unwrap();
        let mut actions = TargetActions::new();
        graph.fire_source(&source[..], 0, &mut actions).unwrap();
        assert_eq!(
            actions.iter().collect::<std::vec::Vec<_>>(),
            [TargetAction::Disable(1)]
        );
        assert_eq!(actions.fired_edges(), 0);
        assert!(!graph.is_enabled(1));
    }

    #[test]
    fn target_cycles_fail_at_the_bounded_pending_queue() {
        let source = [
            entity(CLASS_TRIGGER_RELAY, 1, 2),
            entity(CLASS_TRIGGER_RELAY, 2, 1),
            entity(CLASS_TRIGGER_RELAY, 1, 0),
        ];
        let mut graph = TargetGraph::new();
        graph.load(&source[..]).unwrap();
        let error = graph
            .fire_source(&source[..], 2, &mut TargetActions::new())
            .expect_err("cycle must fail instead of looping");
        assert_eq!(error, TargetError::TooManyPendingUses);
    }

    #[test]
    fn targeted_once_disables_and_multiple_observes_its_wait() {
        let source = [
            entity(CLASS_TRIGGER_RELAY, 1, 0),
            entity(CLASS_TRIGGER_ONCE, 2, 1),
            entity(CLASS_FUNC_DOOR, 0, 2),
            entity(CLASS_TRIGGER_RELAY, 3, 0),
            MapEntity {
                class_name: CLASS_TRIGGER_MULTIPLE,
                target: 4,
                target_name: 3,
                wait: 2_048,
                ..MapEntity::default()
            },
            entity(CLASS_FUNC_BUTTON, 0, 4),
        ];
        let mut graph = TargetGraph::new();
        graph.load(&source[..]).unwrap();
        let mut actions = TargetActions::new();

        graph.fire_source(&source[..], 0, &mut actions).unwrap();
        assert_eq!(
            actions.iter().collect::<std::vec::Vec<_>>(),
            [TargetAction::Activate(2, TargetActivator::None)]
        );
        assert!(!graph.is_enabled(1));
        actions.clear();
        graph.fire_source(&source[..], 0, &mut actions).unwrap();
        assert!(actions.is_empty());

        graph.fire_source(&source[..], 3, &mut actions).unwrap();
        assert_eq!(
            actions.iter().collect::<std::vec::Vec<_>>(),
            [TargetAction::Activate(5, TargetActivator::None)]
        );
        actions.clear();
        graph.fire_source(&source[..], 3, &mut actions).unwrap();
        assert!(actions.is_empty());
        graph.tick(29, &source[..], &mut actions).unwrap();
        graph.fire_source(&source[..], 3, &mut actions).unwrap();
        assert!(actions.is_empty());
        graph.tick(1, &source[..], &mut actions).unwrap();
        graph.fire_source(&source[..], 3, &mut actions).unwrap();
        assert_eq!(
            actions.iter().collect::<std::vec::Vec<_>>(),
            [TargetAction::Activate(5, TargetActivator::None)]
        );
    }

    #[test]
    fn targeted_spikeshooter_fires_once_per_use_edge() {
        let source = [
            entity(CLASS_TRIGGER_MULTIPLE, 1, 0),
            entity(CLASS_TRAP_SPIKESHOOTER, 0, 1),
            entity(CLASS_TRAP_SPIKESHOOTER, 0, 1),
        ];
        let mut graph = TargetGraph::new();
        graph.load(&source[..]).unwrap();
        let mut actions = TargetActions::new();
        graph
            .fire_source_by(&source[..], 0, TargetActivator::Player, &mut actions)
            .unwrap();
        assert_eq!(
            actions.iter().collect::<std::vec::Vec<_>>(),
            [TargetAction::FireShooter(1), TargetAction::FireShooter(2)]
        );
        assert_eq!(actions.fired_edges(), 2);
    }

    /// `monster_use`: a trigger aimed at a monster's targetname wakes it with
    /// the activator as enemy. Chthon keeps his own raise action.
    #[test]
    fn targeted_monsters_wake_with_the_activator() {
        let source = [
            entity(CLASS_TRIGGER_ONCE, 1, 0),
            entity(0x36, 0, 1),
            entity(0x3e, 0, 1),
            entity(CLASS_MONSTER_BOSS, 0, 1),
        ];
        let mut graph = TargetGraph::new();
        graph.load(&source[..]).unwrap();
        let mut actions = TargetActions::new();
        graph
            .fire_source_by(&source[..], 0, TargetActivator::Player, &mut actions)
            .unwrap();
        assert_eq!(
            actions.iter().collect::<std::vec::Vec<_>>(),
            [
                TargetAction::WakeMonster(1, TargetActivator::Player),
                TargetAction::WakeMonster(2, TargetActivator::Player),
                TargetAction::AwakenMonster(3, TargetActivator::Player),
            ]
        );
        assert_eq!(actions.fired_edges(), 3);
    }

    #[test]
    fn skill_exclusion_matches_original_spawnflag_bits() {
        assert!(excluded_for_skill(SPAWNFLAG_NOT_EASY, 0));
        assert!(!excluded_for_skill(SPAWNFLAG_NOT_EASY, 1));
        assert!(!excluded_for_skill(SPAWNFLAG_NOT_EASY, 2));
        assert!(excluded_for_skill(SPAWNFLAG_NOT_MEDIUM, 1));
        assert!(!excluded_for_skill(SPAWNFLAG_NOT_MEDIUM, 2));
        assert!(excluded_for_skill(SPAWNFLAG_NOT_HARD, 2));
        // Nightmare shares the hard exclusion in the original loader.
        assert!(excluded_for_skill(SPAWNFLAG_NOT_HARD, 3));
        let all = SPAWNFLAG_NOT_EASY | SPAWNFLAG_NOT_MEDIUM | SPAWNFLAG_NOT_HARD;
        for skill in 0..4 {
            assert!(excluded_for_skill(all, skill));
        }
    }

    #[test]
    fn setskill_reads_the_authored_digit_and_rejects_anything_else() {
        assert_eq!(parse_setskill(b"0"), Some(0));
        assert_eq!(parse_setskill(b"1"), Some(1));
        assert_eq!(parse_setskill(b"3"), Some(3));
        assert_eq!(parse_setskill(b"4"), None);
        assert_eq!(parse_setskill(b"9"), None);
        assert_eq!(parse_setskill(b"easy"), None);
        assert_eq!(parse_setskill(b""), None);
    }

    #[test]
    fn entity_capacity_accepts_512_and_rejects_513() {
        let exact = [MapEntity::default(); MAX_TARGET_ENTITIES];
        let overflow = [MapEntity::default(); MAX_TARGET_ENTITIES + 1];
        let mut graph = TargetGraph::new();
        graph.load(&exact[..]).unwrap();
        assert_eq!(graph.load(&overflow[..]), Err(TargetError::TooManyEntities));
    }

    #[test]
    fn delayed_use_capacity_accepts_32_and_rejects_the_33rd() {
        let source = [MapEntity {
            class_name: CLASS_TRIGGER_RELAY,
            target: 1,
            delay: 4_096,
            ..MapEntity::default()
        }];
        let mut graph = TargetGraph::new();
        graph.load(&source[..]).unwrap();
        let mut actions = TargetActions::new();
        for _ in 0..MAX_DELAYED_USES {
            graph.fire_source(&source[..], 0, &mut actions).unwrap();
        }
        assert_eq!(
            graph.fire_source(&source[..], 0, &mut actions),
            Err(TargetError::TooManyDelayedUses)
        );
        graph.tick(60, &source[..], &mut actions).unwrap();
        graph.fire_source(&source[..], 0, &mut actions).unwrap();
    }

    #[test]
    fn explicitly_disabled_skill_branch_cannot_dispatch() {
        let source = [
            entity(CLASS_TRIGGER_RELAY, 1, 0),
            entity(CLASS_FUNC_DOOR, 0, 1),
        ];
        let mut graph = TargetGraph::new();
        graph.load(&source[..]).unwrap();
        graph.disable_entity(1).unwrap();
        let mut actions = TargetActions::new();
        graph.fire_source(&source[..], 0, &mut actions).unwrap();
        assert!(actions.is_empty());
        assert_eq!(actions.fired_edges(), 0);
        assert!(!graph.is_enabled(1));
        assert_eq!(graph.disable_entity(2), Err(TargetError::MissingSource));
    }
}
