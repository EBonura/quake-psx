//! Original Quake player survival policy: contents damage, fall damage, air
//! supply, powerups, and the respawn loadout.
//!
//! Everything here is expressed in 60 Hz sim ticks so the guest can drive it
//! from the same VBlank clock that already paces locomotion. The original
//! rules are stated in seconds because QuakeC compares absolute `time`
//! values; each constant below records the second-denominated original and
//! its exact tick conversion.

use crate::collision::{CONTENTS_LAVA, CONTENTS_SLIME, CONTENTS_WATER};
use crate::combat::Inventory;

/// The gameplay clock. `MovementState` advances once per display VBlank, so
/// survival timers share that tick.
pub const TICKS_PER_SECOND: u16 = 60;
/// Movement may catch up several ticks after a load; the motor uses the same
/// bound so a stall cannot turn into one enormous burst of hazard damage.
const MAX_CATCHUP_TICKS: u16 = 4;

/// `WaterMove`: `T_Damage (self, world, world, 10*self.waterlevel)`.
pub const LAVA_DAMAGE_PER_LEVEL: i16 = 10;
/// `WaterMove`: `T_Damage (self, world, world, 4*self.waterlevel)`.
pub const SLIME_DAMAGE_PER_LEVEL: i16 = 4;
/// `WaterMove`: `self.dmgtime = time + 0.2` without a biosuit.
pub const LAVA_INTERVAL_TICKS: u16 = TICKS_PER_SECOND / 5;
/// `WaterMove`: `self.dmgtime = time + 1` while `radsuit_finished > time`.
/// The original biosuit slows lava, it does not stop it.
pub const LAVA_BIOSUIT_INTERVAL_TICKS: u16 = TICKS_PER_SECOND;
/// `WaterMove`: slime always uses `self.dmgtime = time + 1`.
pub const SLIME_INTERVAL_TICKS: u16 = TICKS_PER_SECOND;

/// `PlayerPostThink`: `T_Damage (self, world, world, 5)` past the hard-land
/// threshold. Quake 1 deals a flat five points, not a scaled excess.
pub const FALL_DAMAGE: i16 = 5;

/// `WaterMove`: `self.air_finished = time + 12` whenever the head is out.
pub const AIR_TICKS: u16 = 12 * TICKS_PER_SECOND;
/// `WaterMove`: `self.dmg = 2` on the same surfacing branch, and each drown
/// tick adds two before dealing damage; `if (self.dmg > 15) self.dmg = 10`
/// wraps the ramp, so the sequence is 4, 6, 8, 10, 12, 14, 10, 12, 14 ...
pub const DROWN_DAMAGE_BASE: i16 = 2;
pub const DROWN_DAMAGE_STEP: i16 = 2;
pub const DROWN_DAMAGE_MAX: i16 = 15;
pub const DROWN_DAMAGE_WRAP: i16 = 10;
/// `WaterMove`: `self.pain_finished = time + 1` paces the drowning ticks.
pub const DROWN_INTERVAL_TICKS: u16 = TICKS_PER_SECOND;
/// `WaterMove`: `self.air_finished < time + 9` picks the shorter gasp.
const GASP_TICKS: u16 = 9 * TICKS_PER_SECOND;

/// `player/lburn1.wav`, the precached half of `PainSound`'s lava/slime pair.
pub const SOUND_BURN: i16 = 0x9e;
/// `player/drown2.wav`, the precached half of `PainSound`'s submerged pair.
pub const SOUND_DROWN: i16 = 0x94;
/// `player/gasp1.wav`. The original also has a `gasp2.wav` for a fully
/// exhausted lung; only `gasp1` is precached, so both cases use it.
pub const SOUND_GASP: i16 = 0x95;
/// `player/pain1.wav`, the first entry of `PainSound`'s roll.
pub const SOUND_PAIN: i16 = 0xa0;
/// `PainSound` picks one of six pain voices uniformly; `reslist.txt` cooks
/// three of them into worldspawn's always-resident set (pain1, pain3, pain6),
/// so the roll runs over those and never asks the bank for a voice it does
/// not hold.
pub const PAIN_VOICES: [i16; 3] = [SOUND_PAIN, 0xa2, 0xa5];
/// `player/death1.wav`. `DeathSound` rolls one of five, but death1 is the
/// only one worldspawn precaches, so the roll has a single outcome.
pub const SOUND_DEATH: i16 = 0x8e;
/// `player/h2odeath.wav`, `DeathSound`'s submerged case.
pub const SOUND_WATER_DEATH: i16 = 0x98;
/// `player/gib.wav` and `player/udeath.wav`, `GibPlayer`'s coin flip.
pub const SOUND_GIB: i16 = 0x97;
pub const SOUND_UDEATH: i16 = 0xaa;
/// `PlayerDie`: below this health the death is `GibPlayer` instead.
pub const GIB_HEALTH: i16 = -40;

/// `PainSound`: `self.pain_finished = time + 0.5` throttles the ordinary
/// pain roll. The water, slime, and lava branches return before it.
const PAIN_INTERVAL_TICKS: u16 = TICKS_PER_SECOND / 2;

/// The same seed and LCG the weapon and monster rolls use.
const INITIAL_RANDOM_STATE: u32 = 0x51f1_5e1d;

/// The player's `max_health`, the floor `item_megahealth_rot` rots down to.
pub const PLAYER_MAX_HEALTH: i16 = 100;
/// `health_touch`: `self.nextthink = time + 5` before the first rot.
pub const MEGAHEALTH_ROT_DELAY_TICKS: u16 = 5 * TICKS_PER_SECOND;
/// `item_megahealth_rot`: `self.nextthink = time + 1` between points.
pub const MEGAHEALTH_ROT_INTERVAL_TICKS: u16 = TICKS_PER_SECOND;

/// `powerup_touch` arms every artifact with `X_finished = time + 30`.
pub const POWERUP_TICKS: u16 = 30 * TICKS_PER_SECOND;
/// `CheckPowerups` warns once inside `X_finished < time + 3`.
pub const POWERUP_WARNING_TICKS: u16 = 3 * TICKS_PER_SECOND;
/// `T_Damage`: `damage = damage * 4` while the attacker holds the quad.
pub const QUAD_DAMAGE_MULTIPLIER: i16 = 4;

/// The four shareware artifacts, in `item_artifact_*` inventory order.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PowerupKind {
    /// `item_artifact_super_damage`, Quad Damage.
    Quad,
    /// `item_artifact_invulnerability`, Pentagram of Protection.
    Pentagram,
    /// `item_artifact_invisibility`, Ring of Shadows.
    Ring,
    /// `item_artifact_envirosuit`, Biosuit.
    Biosuit,
}

impl PowerupKind {
    pub const ALL: [Self; 4] = [Self::Quad, Self::Pentagram, Self::Ring, Self::Biosuit];

    #[optimize(size)]
    pub const fn index(self) -> usize {
        match self {
            Self::Quad => 0,
            Self::Pentagram => 1,
            Self::Ring => 2,
            Self::Biosuit => 3,
        }
    }

    #[optimize(size)]
    pub const fn bit(self) -> u8 {
        1 << self.index()
    }

    /// Decode a cooked `item_artifact_*` class name.
    #[optimize(size)]
    pub const fn from_class_name(class_name: u8) -> Option<Self> {
        match class_name {
            0x1d => Some(Self::Biosuit),
            0x1e => Some(Self::Ring),
            0x1f => Some(Self::Pentagram),
            0x20 => Some(Self::Quad),
            _ => None,
        }
    }

    /// The artifact's own `self.noise`, precached by its spawn function and
    /// therefore present in every cooked bank that authors the artifact.
    #[optimize(size)]
    pub const fn pickup_sound(self) -> i16 {
        match self {
            Self::Quad => 0x56,      // items/damage.wav
            Self::Pentagram => 0x5e, // items/protect.wav
            Self::Ring => 0x5a,      // items/inv1.wav
            Self::Biosuit => 0x63,   // items/suit.wav
        }
    }

    /// `CheckPowerups`'s one-shot "wearing off" sound. These are precached by
    /// the original spawn functions but are not in this port's resource list,
    /// so the caller must treat them as optional.
    #[optimize(size)]
    pub const fn expiry_sound(self) -> i16 {
        match self {
            Self::Quad => 0x57,      // items/damage2.wav
            Self::Pentagram => 0x5f, // items/protect2.wav
            Self::Ring => 0x5b,      // items/inv2.wav
            Self::Biosuit => 0x64,   // items/suit2.wav
        }
    }

    /// Short status-bar label for the text HUD.
    #[optimize(size)]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Quad => "QUAD",
            Self::Pentagram => "PENT",
            Self::Ring => "RING",
            Self::Biosuit => "SUIT",
        }
    }
}

/// The four timed artifact effects. Powerups are per-life and per-map, so
/// they live beside the inventory but are cleared on every map load.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Powerups {
    remaining_ticks: [u16; 4],
    warned: u8,
}

impl Powerups {
    #[optimize(size)]
    pub const fn new() -> Self {
        Self {
            remaining_ticks: [0; 4],
            warned: 0,
        }
    }

    #[optimize(size)]
    pub const fn active(self, kind: PowerupKind) -> bool {
        self.remaining_ticks[kind.index()] != 0
    }

    #[optimize(size)]
    pub const fn remaining_ticks(self, kind: PowerupKind) -> u16 {
        self.remaining_ticks[kind.index()]
    }

    /// Whole seconds left, rounded up so an active artifact never reads zero.
    #[optimize(size)]
    pub const fn remaining_seconds(self, kind: PowerupKind) -> u8 {
        let ticks = self.remaining_ticks[kind.index()];
        let seconds = ticks.div_ceil(TICKS_PER_SECOND);
        if seconds > u8::MAX as u16 {
            u8::MAX
        } else {
            seconds as u8
        }
    }

    /// `powerup_touch` overwrites the finish time, so a second artifact of
    /// the same kind restarts the full thirty seconds instead of stacking.
    #[optimize(size)]
    pub fn arm(&mut self, kind: PowerupKind) {
        self.remaining_ticks[kind.index()] = POWERUP_TICKS;
        self.warned &= !kind.bit();
    }

    #[optimize(size)]
    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// Advance every timer. Returns `(warnings, flashes)`: the kinds whose
    /// one-shot expiry warning sound fires on this step, and the kinds that
    /// owe a `bf` screen flash. `CheckPowerups` flashes as the last three
    /// seconds begin and then once more each second (`X_time < time`), so
    /// the flash lands at three, two and one seconds left.
    #[optimize(size)]
    pub fn tick(&mut self, ticks: u16) -> (u8, u8) {
        let mut warnings = 0u8;
        let mut flashes = 0u8;
        let mut index = 0usize;
        while index < self.remaining_ticks.len() {
            let remaining = self.remaining_ticks[index];
            if remaining != 0 {
                let bit = 1u8 << index;
                let next = remaining.saturating_sub(ticks);
                self.remaining_ticks[index] = next;
                if next == 0 {
                    self.warned &= !bit;
                } else if next <= POWERUP_WARNING_TICKS && self.warned & bit == 0 {
                    self.warned |= bit;
                    warnings |= bit;
                }
                if next != 0
                    && next <= POWERUP_WARNING_TICKS
                    && remaining.div_ceil(TICKS_PER_SECOND) != next.div_ceil(TICKS_PER_SECOND)
                {
                    flashes |= bit;
                }
            }
            index += 1;
        }
        (warnings, flashes)
    }
}

/// Sound identifiers raised by one survival frame. Four covers the worst
/// authored overlap (a hazard tick, a fall, and two expiring powerups).
pub const MAX_SURVIVAL_SOUNDS: usize = 4;

/// Per-frame gameplay facts the survival rules consume.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct SurvivalInput {
    /// Elapsed 60 Hz sim ticks, clamped exactly like the movement motor.
    pub elapsed_ticks: u16,
    /// `self.waterlevel` from the movement motor's contents sampling.
    pub water_level: u8,
    /// `self.watertype` from the same sampling.
    pub water_type: i16,
    /// The movement motor raised `HARD_LAND` this frame.
    pub hard_land: bool,
}

/// Aggregate result of advancing the survival rules over one frame.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct SurvivalFrame {
    /// Health actually lost after armor absorption.
    pub damage_taken: i16,
    /// Health crossed from positive to zero or below during this frame.
    pub died: bool,
    /// The death was `GibPlayer` (health below -40): the corpse bursts and
    /// the gib voice replaces `DeathSound`.
    pub gibbed: bool,
    /// `CheckPowerups` issued a `bf` flash for an artifact about to expire.
    pub bonus_flash: bool,
    /// `DeathSound` took its submerged branch, which calls `DeathBubbles(20)`
    /// beside the h2odeath voice. Presentation only: the caller turns it into
    /// an approximation of the bubbles, never the sprite.
    pub death_bubbles: bool,
    sounds: [i16; MAX_SURVIVAL_SOUNDS],
    sound_count: u8,
}

impl SurvivalFrame {
    #[optimize(size)]
    pub fn sounds(&self) -> &[i16] {
        &self.sounds[..self.sound_count as usize]
    }

    #[optimize(size)]
    fn push_sound(&mut self, id: i16) {
        let index = self.sound_count as usize;
        if index < MAX_SURVIVAL_SOUNDS {
            self.sounds[index] = id;
            self.sound_count += 1;
        }
    }
}

/// Persistent survival timers. These are per-life and per-map state, so the
/// caller resets them on a map load rather than carrying them like inventory.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Survival {
    hazard_ticks: u16,
    pain_ticks: u16,
    air_ticks: u16,
    drown_damage: i16,
    last_water_level: u8,
    in_liquid: bool,
    alive: bool,
    /// The port's LCG, shared shape with `WeaponState` and `MonsterRuntime`
    /// so a run is reproducible on host and MIPS alike.
    random_state: u32,
}

impl Survival {
    #[optimize(size)]
    pub const fn new() -> Self {
        Self {
            hazard_ticks: 0,
            pain_ticks: 0,
            air_ticks: AIR_TICKS,
            drown_damage: DROWN_DAMAGE_BASE,
            last_water_level: 0,
            in_liquid: false,
            alive: true,
            random_state: INITIAL_RANDOM_STATE,
        }
    }

    /// Remaining lungful in 60 Hz ticks.
    #[optimize(size)]
    pub const fn air_ticks(&self) -> u16 {
        self.air_ticks
    }

    /// `self.waterlevel` as sampled by the last survival frame.
    #[optimize(size)]
    pub const fn water_level(&self) -> u8 {
        self.last_water_level
    }

    /// Clear per-map survival state. Inventory persistence is unaffected.
    #[optimize(size)]
    pub fn map_loaded(&mut self) {
        *self = Self::new();
    }

    /// Advance the survival rules over the frame's elapsed sim ticks.
    #[optimize(size)]
    pub fn tick(&mut self, inventory: &mut Inventory, input: SurvivalInput) -> SurvivalFrame {
        let mut frame = SurvivalFrame::default();
        let ticks = input.elapsed_ticks.clamp(1, MAX_CATCHUP_TICKS);
        for _ in 0..ticks {
            self.step(inventory, input, &mut frame);
        }
        // `PlayerPostThink` checks the landing once per frame, after physics.
        // The motor already folded every catch-up tick's events together.
        if input.hard_land && inventory.health() > 0 {
            self.damage(inventory, FALL_DAMAGE, input, &mut frame);
        }
        // `CheckPowerups` closes PlayerPostThink.
        let (warnings, flashes) = inventory.tick_powerups(ticks);
        frame.bonus_flash = flashes != 0;
        let mut index = 0usize;
        while index < PowerupKind::ALL.len() {
            let kind = PowerupKind::ALL[index];
            if warnings & kind.bit() != 0 {
                frame.push_sound(kind.expiry_sound());
            }
            index += 1;
        }
        // One latch covers every cause of death, including weapon splash and
        // monster attacks that reach the inventory outside this module.
        if self.alive && inventory.health() <= 0 {
            self.alive = false;
            frame.died = true;
            let health = inventory.health();
            frame.gibbed = health < GIB_HEALTH;
            // `DeathSound`'s submerged branch opens with `DeathBubbles(20)`,
            // and `PlayerDie` returns out of `GibPlayer` before it ever gets
            // there, so the bubbles ride exactly the same condition as the
            // h2odeath voice: not gibbed, and fully under.
            frame.death_bubbles = !frame.gibbed && self.last_water_level == 3;
            frame.push_sound(if frame.gibbed {
                // `random() < 0.5`: the health parity stands in for the roll.
                if health & 1 == 0 {
                    SOUND_GIB
                } else {
                    SOUND_UDEATH
                }
            } else if frame.death_bubbles {
                SOUND_WATER_DEATH
            } else {
                SOUND_DEATH
            });
        }
        frame
    }

    #[optimize(size)]
    fn step(&mut self, inventory: &mut Inventory, input: SurvivalInput, frame: &mut SurvivalFrame) {
        self.hazard_ticks = self.hazard_ticks.saturating_sub(1);
        self.pain_ticks = self.pain_ticks.saturating_sub(1);
        // `item_megahealth_rot` is an independent entity think, so it runs
        // beside the player's own frame rather than inside WaterMove.
        inventory.tick_megahealth_rot();
        // `WaterMove` returns immediately for a dead player.
        if inventory.health() <= 0 {
            return;
        }
        self.last_water_level = input.water_level;
        self.breathe(inventory, input, frame);
        if inventory.health() <= 0 {
            return;
        }
        if input.water_level == 0 {
            self.in_liquid = false;
            return;
        }
        if !self.in_liquid {
            // `self.dmgtime = 0` on the FL_INWATER transition, so the first
            // tick inside a hazard always burns.
            self.in_liquid = true;
            self.hazard_ticks = 0;
        }
        if self.hazard_ticks != 0 {
            return;
        }
        let level = i16::from(input.water_level);
        let biosuit = inventory.powerups().active(PowerupKind::Biosuit);
        match input.water_type {
            CONTENTS_LAVA => {
                // The original biosuit only stretches lava's cadence.
                self.hazard_ticks = if biosuit {
                    LAVA_BIOSUIT_INTERVAL_TICKS
                } else {
                    LAVA_INTERVAL_TICKS
                };
                self.damage(
                    inventory,
                    LAVA_DAMAGE_PER_LEVEL.saturating_mul(level),
                    input,
                    frame,
                );
            }
            CONTENTS_SLIME => {
                // `radsuit_finished < time` guards the whole slime branch,
                // including its dmgtime write, so slime burns the instant the
                // suit runs out.
                if biosuit {
                    return;
                }
                self.hazard_ticks = SLIME_INTERVAL_TICKS;
                self.damage(
                    inventory,
                    SLIME_DAMAGE_PER_LEVEL.saturating_mul(level),
                    input,
                    frame,
                );
            }
            _ => {}
        }
    }

    /// `WaterMove`'s air supply: twelve seconds of lung, then an escalating
    /// drowning tick every second until the head clears the surface.
    #[optimize(size)]
    fn breathe(
        &mut self,
        inventory: &mut Inventory,
        input: SurvivalInput,
        frame: &mut SurvivalFrame,
    ) {
        if inventory.powerups().active(PowerupKind::Biosuit) {
            // `CheckPowerups`: `self.air_finished = time + 12` every frame,
            // so a suited player never runs the drowning branch and never
            // gasps on surfacing.
            self.air_ticks = AIR_TICKS;
            self.drown_damage = DROWN_DAMAGE_BASE;
            return;
        }
        if input.water_level != 3 {
            if self.air_ticks < GASP_TICKS {
                // The original picks gasp2 for an exhausted lung and gasp1
                // otherwise; only gasp1 is precached.
                frame.push_sound(SOUND_GASP);
            }
            self.air_ticks = AIR_TICKS;
            self.drown_damage = DROWN_DAMAGE_BASE;
            return;
        }
        self.air_ticks = self.air_ticks.saturating_sub(1);
        if self.air_ticks != 0 || self.pain_ticks != 0 {
            return;
        }
        self.drown_damage = self.drown_damage.saturating_add(DROWN_DAMAGE_STEP);
        if self.drown_damage > DROWN_DAMAGE_MAX {
            self.drown_damage = DROWN_DAMAGE_WRAP;
        }
        self.pain_ticks = DROWN_INTERVAL_TICKS;
        self.damage(inventory, self.drown_damage, input, frame);
    }

    /// Route one hit through the shared armor-aware damage path and pick
    /// `PainSound`/`DeathSound` exactly as the original does.
    #[optimize(size)]
    fn damage(
        &mut self,
        inventory: &mut Inventory,
        points: i16,
        input: SurvivalInput,
        frame: &mut SurvivalFrame,
    ) {
        if points <= 0 {
            return;
        }
        let taken = inventory.take_damage(points);
        frame.damage_taken = frame.damage_taken.saturating_add(taken);
        if inventory.health() <= 0 {
            // `T_Damage` calls `Killed` and returns before the pain callback;
            // the death latch in `tick` raises DeathSound.
            return;
        }
        match liquid_pain_sound(input.water_level, input.water_type) {
            // The submerged, slime, and lava branches of `PainSound` speak on
            // every hit; only the ordinary roll below them is throttled.
            Some(sound) => frame.push_sound(sound),
            None => {
                if self.pain_ticks == 0 {
                    self.pain_ticks = PAIN_INTERVAL_TICKS;
                    frame.push_sound(self.pain_voice());
                }
            }
        }
    }

    /// `PainSound`'s `rint((random() * 5) + 1)`, drawn over the pain voices
    /// this port actually cooks.
    #[optimize(size)]
    fn pain_voice(&mut self) -> i16 {
        self.random_state = self
            .random_state
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        let roll = (self.random_state >> 16) as usize % PAIN_VOICES.len();
        PAIN_VOICES[roll]
    }
}

impl Default for Survival {
    #[optimize(size)]
    fn default() -> Self {
        Self::new()
    }
}

/// `PainSound`'s liquid branches: drowning while fully submerged in water,
/// and the burn pair in slime or lava. `None` falls through to the ordinary
/// throttled roll.
#[optimize(size)]
const fn liquid_pain_sound(water_level: u8, water_type: i16) -> Option<i16> {
    if water_level == 0 {
        return None;
    }
    if water_type == CONTENTS_WATER && water_level == 3 {
        return Some(SOUND_DROWN);
    }
    if water_type == CONTENTS_SLIME || water_type == CONTENTS_LAVA {
        return Some(SOUND_BURN);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::Pickup;

    #[optimize(size)]
    fn submerged(water_type: i16, water_level: u8) -> SurvivalInput {
        SurvivalInput {
            elapsed_ticks: 1,
            water_level,
            water_type,
            hard_land: false,
        }
    }

    #[optimize(size)]
    fn run(state: &mut Survival, inventory: &mut Inventory, input: SurvivalInput, ticks: u16) {
        for _ in 0..ticks {
            state.tick(inventory, input);
        }
    }

    #[optimize(size)]
    #[test]
    fn lava_burns_ten_per_water_level_five_times_a_second() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        // One second at waist level: 5 ticks of 20 points.
        run(
            &mut state,
            &mut inventory,
            submerged(CONTENTS_LAVA, 2),
            TICKS_PER_SECOND,
        );
        assert_eq!(inventory.health(), 0);

        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        run(&mut state, &mut inventory, submerged(CONTENTS_LAVA, 1), 1);
        assert_eq!(inventory.health(), 90);
        run(
            &mut state,
            &mut inventory,
            submerged(CONTENTS_LAVA, 1),
            LAVA_INTERVAL_TICKS - 1,
        );
        assert_eq!(
            inventory.health(),
            90,
            "lava may not retrigger inside 0.2 s"
        );
        run(&mut state, &mut inventory, submerged(CONTENTS_LAVA, 1), 1);
        assert_eq!(inventory.health(), 80);
    }

    #[optimize(size)]
    #[test]
    fn slime_burns_four_per_water_level_once_a_second() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        run(&mut state, &mut inventory, submerged(CONTENTS_SLIME, 3), 1);
        assert_eq!(inventory.health(), 88);
        run(
            &mut state,
            &mut inventory,
            submerged(CONTENTS_SLIME, 3),
            SLIME_INTERVAL_TICKS - 1,
        );
        assert_eq!(inventory.health(), 88, "slime may not retrigger inside 1 s");
        run(&mut state, &mut inventory, submerged(CONTENTS_SLIME, 3), 1);
        assert_eq!(inventory.health(), 76);
    }

    #[optimize(size)]
    #[test]
    fn water_never_burns() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        run(
            &mut state,
            &mut inventory,
            submerged(CONTENTS_WATER, 3),
            10 * TICKS_PER_SECOND,
        );
        assert_eq!(inventory.health(), 100);
    }

    #[optimize(size)]
    #[test]
    fn leaving_and_re_entering_a_hazard_burns_immediately() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        run(&mut state, &mut inventory, submerged(CONTENTS_LAVA, 1), 1);
        assert_eq!(inventory.health(), 90);
        run(
            &mut state,
            &mut inventory,
            submerged(crate::collision::CONTENTS_EMPTY, 0),
            1,
        );
        run(&mut state, &mut inventory, submerged(CONTENTS_LAVA, 1), 1);
        assert_eq!(inventory.health(), 80);
    }

    #[optimize(size)]
    #[test]
    fn hazard_damage_is_absorbed_by_armor() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        inventory.apply_pickup(crate::combat::Pickup::Armor {
            tier: crate::combat::ArmorTier::Red,
            amount: 200,
        });
        run(&mut state, &mut inventory, submerged(CONTENTS_LAVA, 1), 1);
        // Red armor absorbs 80 percent: 8 of 10 points.
        assert_eq!(inventory.health(), 98);
        assert_eq!(inventory.armor(), 192);
    }

    #[optimize(size)]
    #[test]
    fn a_hard_landing_costs_five_points_once_per_frame() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        let frame = state.tick(
            &mut inventory,
            SurvivalInput {
                elapsed_ticks: 4,
                hard_land: true,
                ..SurvivalInput::default()
            },
        );
        assert_eq!(frame.damage_taken, FALL_DAMAGE);
        assert_eq!(inventory.health(), 95);
    }

    #[optimize(size)]
    #[test]
    fn the_dry_pain_voice_rolls_over_every_cooked_pain_sound() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        let mut seen = [false; PAIN_VOICES.len()];
        for _ in 0..64 {
            inventory.apply_pickup(Pickup::Health {
                amount: 100,
                maximum: PLAYER_MAX_HEALTH as u16,
            });
            let frame = state.tick(
                &mut inventory,
                SurvivalInput {
                    elapsed_ticks: 1,
                    hard_land: true,
                    ..SurvivalInput::default()
                },
            );
            for &sound in frame.sounds() {
                let index = PAIN_VOICES
                    .iter()
                    .position(|voice| *voice == sound)
                    .expect("a dry pain sound is one of the cooked pain voices");
                seen[index] = true;
            }
            // `pain_finished` throttles the roll to twice a second.
            run(
                &mut state,
                &mut inventory,
                SurvivalInput::default(),
                PAIN_INTERVAL_TICKS,
            );
        }
        assert!(seen.iter().all(|hit| *hit), "every voice is reachable");
    }

    #[optimize(size)]
    #[test]
    fn an_ordinary_landing_costs_nothing() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        let frame = state.tick(
            &mut inventory,
            SurvivalInput {
                elapsed_ticks: 1,
                ..SurvivalInput::default()
            },
        );
        assert_eq!(frame.damage_taken, 0);
        assert_eq!(inventory.health(), 100);
    }

    #[optimize(size)]
    #[test]
    fn a_lethal_hazard_reports_death_and_the_submerged_death_sound() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        let mut died = None;
        for tick in 0..TICKS_PER_SECOND {
            let frame = state.tick(&mut inventory, submerged(CONTENTS_LAVA, 3));
            if frame.died {
                died = Some((tick, frame));
                break;
            }
        }
        let (_, frame) = died.expect("lava kills inside one second");
        assert_eq!(frame.sounds(), &[SOUND_WATER_DEATH]);
        assert!(inventory.health() <= 0);
    }

    #[optimize(size)]
    #[test]
    fn every_burn_speaks_because_pain_sound_returns_before_its_throttle() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        inventory.apply_pickup(crate::combat::Pickup::Health {
            amount: 100,
            maximum: 250,
        });
        let mut sounds = 0usize;
        for _ in 0..TICKS_PER_SECOND {
            for &sound in state
                .tick(&mut inventory, submerged(CONTENTS_LAVA, 1))
                .sounds()
            {
                assert_eq!(sound, SOUND_BURN);
                sounds += 1;
            }
        }
        // Five lava ticks in a second, and the lava branch of PainSound
        // returns above `self.pain_finished`, so all five speak.
        assert_eq!(sounds, 5);
    }

    #[optimize(size)]
    #[test]
    fn twelve_seconds_of_air_precede_the_first_drowning_tick() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        assert_eq!(state.air_ticks(), AIR_TICKS);
        run(
            &mut state,
            &mut inventory,
            submerged(CONTENTS_WATER, 3),
            AIR_TICKS - 1,
        );
        assert_eq!(inventory.health(), 100);
        assert_eq!(state.air_ticks(), 1);
        let frame = state.tick(&mut inventory, submerged(CONTENTS_WATER, 3));
        assert_eq!(frame.damage_taken, 4);
        assert_eq!(frame.sounds(), &[SOUND_DROWN]);
    }

    #[optimize(size)]
    #[test]
    fn drowning_damage_climbs_by_two_a_second_and_wraps_to_ten() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        inventory.apply_pickup(crate::combat::Pickup::Health {
            amount: 100,
            maximum: 250,
        });
        let mut taken = [0i16; 8];
        let mut index = 0usize;
        for _ in 0..(AIR_TICKS + 8 * DROWN_INTERVAL_TICKS) {
            let frame = state.tick(&mut inventory, submerged(CONTENTS_WATER, 3));
            if frame.damage_taken != 0 && index < taken.len() {
                taken[index] = frame.damage_taken;
                index += 1;
            }
        }
        // self.dmg starts at 2 and gains 2 before each hit; past 15 it
        // wraps back to 10.
        assert_eq!(taken, [4, 6, 8, 10, 12, 14, 10, 12]);
    }

    #[optimize(size)]
    #[test]
    fn surfacing_restores_the_lungful_and_gasps() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        // Three seconds under is not enough to gasp: the original only
        // speaks once air_finished drops inside nine seconds.
        run(
            &mut state,
            &mut inventory,
            submerged(CONTENTS_WATER, 3),
            3 * TICKS_PER_SECOND - 1,
        );
        let quiet = state.tick(&mut inventory, submerged(CONTENTS_WATER, 2));
        assert!(quiet.sounds().is_empty());
        assert_eq!(state.air_ticks(), AIR_TICKS);

        run(
            &mut state,
            &mut inventory,
            submerged(CONTENTS_WATER, 3),
            4 * TICKS_PER_SECOND,
        );
        let gasp = state.tick(&mut inventory, submerged(CONTENTS_WATER, 2));
        assert_eq!(gasp.sounds(), &[SOUND_GASP]);
        assert_eq!(state.air_ticks(), AIR_TICKS);
        assert_eq!(inventory.health(), 100);
    }

    /// Drown until `wanted` ticks have landed and report each hit.
    #[optimize(size)]
    fn drown(state: &mut Survival, inventory: &mut Inventory, wanted: usize) -> [i16; 4] {
        let mut hits = [0i16; 4];
        let mut seen = 0usize;
        for _ in 0..(AIR_TICKS + 32 * DROWN_INTERVAL_TICKS) {
            let frame = state.tick(inventory, submerged(CONTENTS_WATER, 3));
            if frame.damage_taken != 0 {
                hits[seen.min(hits.len() - 1)] = frame.damage_taken;
                seen += 1;
                if seen == wanted {
                    return hits;
                }
            }
        }
        panic!("the submerged player never took {wanted} drowning ticks");
    }

    #[optimize(size)]
    #[test]
    fn surfacing_resets_the_drowning_damage_ramp() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        inventory.apply_pickup(crate::combat::Pickup::Health {
            amount: 100,
            maximum: 250,
        });
        assert_eq!(drown(&mut state, &mut inventory, 3), [4, 6, 8, 0]);
        state.tick(&mut inventory, submerged(CONTENTS_WATER, 1));
        assert_eq!(drown(&mut state, &mut inventory, 1)[0], 4);
    }

    #[optimize(size)]
    #[test]
    fn drowning_can_kill_and_uses_the_submerged_death_sound() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        let mut died = None;
        for _ in 0..(AIR_TICKS + 60 * DROWN_INTERVAL_TICKS) {
            let frame = state.tick(&mut inventory, submerged(CONTENTS_WATER, 3));
            if frame.died {
                died = Some(frame);
                break;
            }
        }
        let frame = died.expect("a submerged player eventually drowns");
        assert_eq!(frame.sounds(), &[SOUND_WATER_DEATH]);
        assert!(inventory.health() <= 0);
    }

    #[optimize(size)]
    #[test]
    fn every_artifact_class_arms_a_thirty_second_timer() {
        for (class_name, kind) in [
            (0x20u8, PowerupKind::Quad),
            (0x1f, PowerupKind::Pentagram),
            (0x1e, PowerupKind::Ring),
            (0x1d, PowerupKind::Biosuit),
        ] {
            let mut inventory = Inventory::new();
            let pickup =
                crate::combat::pickup_for_entity(class_name, 0).expect("authored artifact");
            let outcome = inventory.apply_pickup(pickup);
            assert!(outcome.consumed);
            assert_eq!(outcome.sound_id, Some(kind.pickup_sound()));
            assert!(inventory.powerups().active(kind));
            assert_eq!(inventory.powerups().remaining_ticks(kind), POWERUP_TICKS);
            assert_eq!(inventory.powerups().remaining_seconds(kind), 30);
        }
    }

    #[optimize(size)]
    #[test]
    fn an_artifact_expires_after_thirty_seconds_and_warns_once_at_three() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        inventory.apply_pickup(Pickup::Powerup {
            kind: PowerupKind::Quad,
        });
        let mut warnings = 0usize;
        let mut flashes = [0u8; 4];
        let mut flash_count = 0usize;
        let mut expired_at = None;
        for tick in 0..(40 * TICKS_PER_SECOND) {
            let frame = state.tick(&mut inventory, SurvivalInput::default());
            if frame.bonus_flash {
                flashes[flash_count.min(3)] =
                    inventory.powerups().remaining_seconds(PowerupKind::Quad);
                flash_count += 1;
            }
            for &sound in frame.sounds() {
                assert_eq!(sound, PowerupKind::Quad.expiry_sound());
                warnings += 1;
                assert_eq!(
                    inventory.powerups().remaining_seconds(PowerupKind::Quad),
                    3,
                    "the warning fires as the last three seconds begin"
                );
            }
            if expired_at.is_none() && !inventory.powerups().active(PowerupKind::Quad) {
                expired_at = Some(tick);
            }
        }
        assert_eq!(warnings, 1);
        assert_eq!(flash_count, 3);
        assert_eq!(flashes, [3, 2, 1, 0]);
        assert_eq!(expired_at, Some(POWERUP_TICKS - 1));
    }

    #[optimize(size)]
    #[test]
    fn re_arming_restarts_the_timer_and_the_warning() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        inventory.apply_pickup(Pickup::Powerup {
            kind: PowerupKind::Ring,
        });
        run(
            &mut state,
            &mut inventory,
            SurvivalInput::default(),
            POWERUP_TICKS - POWERUP_WARNING_TICKS,
        );
        assert!(!inventory.powerups().active(PowerupKind::Quad));
        inventory.apply_pickup(Pickup::Powerup {
            kind: PowerupKind::Ring,
        });
        assert_eq!(
            inventory.powerups().remaining_ticks(PowerupKind::Ring),
            POWERUP_TICKS
        );
        let mut warnings = 0usize;
        for _ in 0..POWERUP_TICKS {
            warnings += state
                .tick(&mut inventory, SurvivalInput::default())
                .sounds()
                .len();
        }
        assert_eq!(warnings, 1);
    }

    #[optimize(size)]
    #[test]
    fn the_pentagram_stops_health_loss_but_not_armor_loss() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        inventory.apply_pickup(Pickup::Armor {
            tier: crate::combat::ArmorTier::Green,
            amount: 100,
        });
        inventory.apply_pickup(Pickup::Powerup {
            kind: PowerupKind::Pentagram,
        });
        run(
            &mut state,
            &mut inventory,
            submerged(CONTENTS_LAVA, 3),
            5 * TICKS_PER_SECOND,
        );
        assert_eq!(inventory.health(), 100);
        assert!(inventory.armor() < 100, "armor still burns down");
    }

    #[optimize(size)]
    #[test]
    fn the_pentagram_also_stops_drowning() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        inventory.apply_pickup(Pickup::Powerup {
            kind: PowerupKind::Pentagram,
        });
        run(
            &mut state,
            &mut inventory,
            submerged(CONTENTS_WATER, 3),
            AIR_TICKS + 10 * DROWN_INTERVAL_TICKS,
        );
        assert_eq!(inventory.health(), 100);
    }

    #[optimize(size)]
    #[test]
    fn the_biosuit_stops_slime_and_drowning_but_only_slows_lava() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        inventory.apply_pickup(Pickup::Powerup {
            kind: PowerupKind::Biosuit,
        });
        run(
            &mut state,
            &mut inventory,
            submerged(CONTENTS_SLIME, 3),
            10 * TICKS_PER_SECOND,
        );
        assert_eq!(
            inventory.health(),
            100,
            "slime cannot touch a suited player"
        );

        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        inventory.apply_pickup(Pickup::Powerup {
            kind: PowerupKind::Biosuit,
        });
        run(
            &mut state,
            &mut inventory,
            submerged(CONTENTS_WATER, 3),
            AIR_TICKS + 10 * DROWN_INTERVAL_TICKS,
        );
        assert_eq!(inventory.health(), 100, "the suit supplies air");
        assert_eq!(state.air_ticks(), AIR_TICKS);

        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        inventory.apply_pickup(Pickup::Powerup {
            kind: PowerupKind::Biosuit,
        });
        // One second of level 1 lava: one 10 point tick, not five.
        run(
            &mut state,
            &mut inventory,
            submerged(CONTENTS_LAVA, 1),
            TICKS_PER_SECOND,
        );
        assert_eq!(inventory.health(), 90);
    }

    #[optimize(size)]
    #[test]
    fn slime_burns_the_instant_the_biosuit_runs_out() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        inventory.apply_pickup(Pickup::Powerup {
            kind: PowerupKind::Biosuit,
        });
        run(
            &mut state,
            &mut inventory,
            submerged(CONTENTS_SLIME, 2),
            POWERUP_TICKS,
        );
        assert!(!inventory.powerups().active(PowerupKind::Biosuit));
        assert_eq!(inventory.health(), 100);
        state.tick(&mut inventory, submerged(CONTENTS_SLIME, 2));
        assert_eq!(inventory.health(), 92);
    }

    #[optimize(size)]
    #[test]
    fn a_megahealth_rots_one_point_a_second_after_a_five_second_delay() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        let megahealth = crate::combat::pickup_for_entity(0x22, 2).expect("megahealth");
        assert!(inventory.apply_pickup(megahealth).consumed);
        assert_eq!(inventory.health(), 200);
        assert!(inventory.megahealth_rotting());

        run(
            &mut state,
            &mut inventory,
            SurvivalInput::default(),
            MEGAHEALTH_ROT_DELAY_TICKS - 1,
        );
        assert_eq!(inventory.health(), 200, "the first rot waits five seconds");
        state.tick(&mut inventory, SurvivalInput::default());
        assert_eq!(inventory.health(), 199);
        run(
            &mut state,
            &mut inventory,
            SurvivalInput::default(),
            MEGAHEALTH_ROT_INTERVAL_TICKS,
        );
        assert_eq!(inventory.health(), 198);

        // Rot all the way down and then stop dead on max_health.
        run(
            &mut state,
            &mut inventory,
            SurvivalInput::default(),
            120 * TICKS_PER_SECOND,
        );
        assert_eq!(inventory.health(), PLAYER_MAX_HEALTH);
        assert!(!inventory.megahealth_rotting());
        run(
            &mut state,
            &mut inventory,
            SurvivalInput::default(),
            10 * TICKS_PER_SECOND,
        );
        assert_eq!(inventory.health(), PLAYER_MAX_HEALTH);
    }

    #[optimize(size)]
    #[test]
    fn ordinary_health_boxes_never_arm_the_rot() {
        for spawn_flags in [0u16, 1] {
            let mut inventory = Inventory::new();
            inventory.take_damage(60);
            let pickup = crate::combat::pickup_for_entity(0x22, spawn_flags).expect("health box");
            assert!(inventory.apply_pickup(pickup).consumed);
            assert!(!inventory.megahealth_rotting());
        }
    }

    #[optimize(size)]
    #[test]
    fn a_megahealth_taken_while_hurt_rots_only_down_to_max_health() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        inventory.take_damage(50);
        let megahealth = crate::combat::pickup_for_entity(0x22, 2).expect("megahealth");
        assert!(inventory.apply_pickup(megahealth).consumed);
        assert_eq!(inventory.health(), 150);
        run(
            &mut state,
            &mut inventory,
            SurvivalInput::default(),
            120 * TICKS_PER_SECOND,
        );
        assert_eq!(inventory.health(), PLAYER_MAX_HEALTH);
    }

    #[optimize(size)]
    #[test]
    fn death_from_outside_this_module_is_latched_once() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        let quiet = state.tick(&mut inventory, SurvivalInput::default());
        assert!(!quiet.died);

        // A monster or splash hit reaches the inventory elsewhere in the
        // frame; the next survival tick is what notices.
        inventory.take_damage(100);
        let died = state.tick(&mut inventory, SurvivalInput::default());
        assert!(died.died);
        assert_eq!(died.sounds(), &[SOUND_DEATH]);

        let after = state.tick(&mut inventory, SurvivalInput::default());
        assert!(!after.died);
        assert!(after.sounds().is_empty());
    }

    #[optimize(size)]
    #[test]
    fn a_death_under_water_uses_the_last_submerged_level() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        state.tick(&mut inventory, submerged(CONTENTS_WATER, 3));
        inventory.take_damage(100);
        // The guest stops advancing the motor once health hits zero, so the
        // killing frame reports no water at all.
        let died = state.tick(&mut inventory, SurvivalInput::default());
        assert!(died.died);
        assert_eq!(died.sounds(), &[SOUND_WATER_DEATH]);
        // `DeathSound`'s submerged branch calls `DeathBubbles(20)` beside the
        // voice, so the two signals are raised together or not at all.
        assert!(died.death_bubbles);
    }

    /// `PlayerDie` returns out of `GibPlayer` before `DeathSound` runs, so a
    /// player who bursts under water never bubbles. Dying dry never does
    /// either, which is the branch the other tests already cover.
    #[optimize(size)]
    #[test]
    fn only_an_intact_submerged_corpse_bubbles() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        state.tick(&mut inventory, submerged(CONTENTS_WATER, 3));
        inventory.take_damage(200);
        let died = state.tick(&mut inventory, SurvivalInput::default());
        assert!(died.gibbed);
        assert!(!died.death_bubbles);

        let mut dry = Survival::new();
        let mut dry_inventory = Inventory::new();
        dry_inventory.take_damage(100);
        assert!(
            !dry.tick(&mut dry_inventory, SurvivalInput::default())
                .death_bubbles
        );
    }

    #[optimize(size)]
    #[test]
    fn a_dead_player_stops_burning() {
        let mut state = Survival::new();
        let mut inventory = Inventory::new();
        inventory.take_damage(200);
        run(
            &mut state,
            &mut inventory,
            submerged(CONTENTS_LAVA, 3),
            TICKS_PER_SECOND,
        );
        assert_eq!(inventory.health(), -100);
    }
}
