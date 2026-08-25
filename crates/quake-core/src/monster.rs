//! Fixed-capacity monster policy for the authored Episode 1 bestiary.
//!
//! Every frame number, ten Hz think cadence, distance table, attack range,
//! damage roll, pain threshold, gib threshold, and sound identifier below is
//! transcribed from the preserved Quake runtime at commit `001246d`
//! (`src/progs/monster_*.c` and `src/progs/ai.c`). Nothing here allocates,
//! recurses, or leaves 32-bit integer arithmetic.
//!
//! Shape of the port: the original spends one C function per monster state and
//! keeps its cursor in `nextthink`. This module keeps the same *data* (state
//! frame ranges, per-frame move tables, per-frame attack events) and runs one
//! shared bounded interpreter over it, so adding a monster is a table, not an
//! engine. Behaviour that genuinely differs per monster (attack selection, pain
//! thresholds, the zombie knockdown, the Chthon shock kill) stays as small
//! explicit matches.
//!
//! `MonsterState::Walk` is Quake's `path_corner` patrol: the host resolves the
//! monster's `target` chain and hands the current corner in through
//! [`MonsterThinkInput::goal`]; without one the original parks `pause_time`
//! at 0xFFFF and stands forever, which is what this runtime does. The enemy
//! is normally the player; `T_Damage` from another monster of a different
//! class (or any soldier) switches it to that monster until it dies.

use quake_formats::Vec3I32;

pub const CLASS_ARMY: u8 = 0x36;
pub const CLASS_BOSS: u8 = 0x37;
pub const CLASS_DEMON: u8 = 0x38;
pub const CLASS_DOG: u8 = 0x39;
pub const CLASS_KNIGHT: u8 = 0x3d;
pub const CLASS_OGRE: u8 = 0x3e;
pub const CLASS_SHAMBLER: u8 = 0x41;
pub const CLASS_WIZARD: u8 = 0x43;
pub const CLASS_ZOMBIE: u8 = 0x44;

pub const SOLDIER_MODEL_ID: i16 = 0x49;
pub const DOG_MODEL_ID: i16 = 0x17;
pub const OGRE_MODEL_ID: i16 = 0x41;
pub const KNIGHT_MODEL_ID: i16 = 0x3b;
pub const ZOMBIE_MODEL_ID: i16 = 0x5c;
pub const WIZARD_MODEL_ID: i16 = 0x5a;
pub const SHAMBLER_MODEL_ID: i16 = 0x48;
pub const DEMON_MODEL_ID: i16 = 0x16;
pub const BOSS_MODEL_ID: i16 = 0x15;
pub const ZOMBIE_GIB_MODEL_ID: i16 = 0x5b;
pub const LAVA_BALL_MODEL_ID: i16 = 0x3d;

pub const DEMON_HEAD_MODEL_ID: i16 = 0x2b;
pub const DOG_HEAD_MODEL_ID: i16 = 0x2c;
pub const SOLDIER_HEAD_MODEL_ID: i16 = 0x2d;
pub const KNIGHT_HEAD_MODEL_ID: i16 = 0x2f;
pub const OGRE_HEAD_MODEL_ID: i16 = 0x31;
pub const SHAMBLER_HEAD_MODEL_ID: i16 = 0x34;
pub const WIZARD_HEAD_MODEL_ID: i16 = 0x35;
pub const ZOMBIE_HEAD_MODEL_ID: i16 = 0x36;

pub const SOLDIER_SIGHT_SOUND: i16 = 0xbf;
pub const SOLDIER_IDLE_SOUND: i16 = 0xbb;
/// soldier/pain2: the only cooked soldier pain voice.
pub const SOLDIER_PAIN_SOUND: i16 = 0xbd;
pub const SOLDIER_DEATH_SOUND: i16 = 0xba;
pub const SOLDIER_ATTACK_SOUND: i16 = 0xc3;
pub const DOG_SIGHT_SOUND: i16 = 0x2a;
pub const DOG_IDLE_SOUND: i16 = 0x2b;
pub const DOG_PAIN_SOUND: i16 = 0x29;
pub const DOG_DEATH_SOUND: i16 = 0x28;
pub const DOG_ATTACK_SOUND: i16 = 0x27;
pub const OGRE_SIGHT_SOUND: i16 = 0x85;
pub const OGRE_IDLE_SOUND: i16 = 0x81;
pub const OGRE_IDLE2_SOUND: i16 = 0x82;
pub const OGRE_PAIN_SOUND: i16 = 0x83;
pub const OGRE_DEATH_SOUND: i16 = 0x80;
pub const OGRE_SAW_SOUND: i16 = 0x84;
pub const KNIGHT_SIGHT_SOUND: i16 = 0x68;
pub const KNIGHT_IDLE_SOUND: i16 = 0x65;
pub const KNIGHT_PAIN_SOUND: i16 = 0x67;
pub const KNIGHT_DEATH_SOUND: i16 = 0x66;
pub const KNIGHT_SWORD_SOUND: i16 = 0x69;
pub const KNIGHT_SWORD2_SOUND: i16 = 0x6a;
pub const ZOMBIE_IDLE_SOUND: i16 = 0xdc;
pub const ZOMBIE_PAIN_SOUND: i16 = 0xdf;
pub const ZOMBIE_FALL_SOUND: i16 = 0xd9;
pub const ZOMBIE_THROW_SOUND: i16 = 0xe1;
pub const ZOMBIE_MISS_SOUND: i16 = 0xde;
pub const ZOMBIE_HIT_SOUND: i16 = 0xdb;
pub const ZOMBIE_CRUCIFIED_SOUND: i16 = 0xd8;
pub const WIZARD_SIGHT_SOUND: i16 = 0xd7;
pub const WIZARD_IDLE_SOUND: i16 = 0xd4;
pub const WIZARD_IDLE2_SOUND: i16 = 0xd5;
pub const WIZARD_PAIN_SOUND: i16 = 0xd6;
pub const WIZARD_DEATH_SOUND: i16 = 0xd3;
pub const WIZARD_ATTACK_SOUND: i16 = 0xd2;
pub const SHAMBLER_SIGHT_SOUND: i16 = 0xb9;
pub const SHAMBLER_IDLE_SOUND: i16 = 0xb7;
pub const SHAMBLER_PAIN_SOUND: i16 = 0xb6;
pub const SHAMBLER_DEATH_SOUND: i16 = 0xb5;
pub const SHAMBLER_MELEE1_SOUND: i16 = 0xb1;
pub const SHAMBLER_MELEE2_SOUND: i16 = 0xb2;
pub const SHAMBLER_SMACK_SOUND: i16 = 0xb8;
pub const SHAMBLER_MAGIC_SOUND: i16 = 0xb3;
/// `sham_magic6` voices the discharge itself; the later `CastLightning` frames
/// only repeat the bolt.
pub const SHAMBLER_BOOM_SOUND: i16 = 0xb4;
pub const DEMON_SIGHT_SOUND: i16 = 0x26;
pub const DEMON_IDLE_SOUND: i16 = 0x25;
pub const DEMON_PAIN_SOUND: i16 = 0x24;
pub const DEMON_DEATH_SOUND: i16 = 0x20;
pub const DEMON_HIT_SOUND: i16 = 0x21;
pub const DEMON_JUMP_SOUND: i16 = 0x22;
pub const BOSS_SIGHT_SOUND: i16 = 0x16;
pub const BOSS_OUT_SOUND: i16 = 0x14;
pub const BOSS_PAIN_SOUND: i16 = 0x15;
pub const BOSS_THROW_SOUND: i16 = 0x17;
pub const BOSS_DEATH_SOUND: i16 = 0x13;
pub const GRENADE_LAUNCH_SOUND: i16 = 0xc2;
/// Gib voices: player/udeath for every monster, zombie/z_gib for the zombie.
pub const MONSTER_GIB_SOUND: i16 = 0xaa;
pub const ZOMBIE_GIB_SOUND: i16 = 0xda;

pub const MONSTER_THINK_TICKS: u16 = 6;
/// Soldier and dog gib threshold, kept as the historical name. Every other
/// monster carries its own threshold in [`MonsterKind::gib_health`].
pub const MONSTER_GIB_HEALTH: i16 = -35;
pub const MONSTER_FAR_RANGE: i32 = 1_000;
pub const MONSTER_NEAR_RANGE: i32 = 500;
pub const MONSTER_MELEE_RANGE: i32 = 100;
/// Quake clips a voice at `sound_nominal_clip_dist / attenuation`.
pub const MONSTER_VOICE_RANGE: i32 = 1_000;
pub const MONSTER_IDLE_VOICE_RANGE: i32 = 666;

/// Chthon's authored Easy health, in lightning shocks.
pub const BOSS_EASY_SHOCKS: i16 = 1;
/// Chthon's authored Normal-and-above health, in lightning shocks.
pub const BOSS_HARD_SHOCKS: i16 = 3;

const Q12_ONE: u16 = 1 << 12;
/// [`MonsterRuntime::enemy_index`] value meaning the player.
const NO_ENEMY_MONSTER: u16 = u16::MAX;
const INITIAL_ATTACK_COOLDOWN: u16 = 60;
/// Ten Hz think ticks in one second of Quake game time.
const ONE_SECOND: u16 = 60;
/// `T_Damage` on skill 3: `self.pain_finished = time + 5`.
const NIGHTMARE_PAIN_TICKS: u16 = 5 * ONE_SECOND;
/// `sv_gravity`, units per second squared, pulling on a monster in mid-leap.
const LEAP_GRAVITY: i32 = 800;
/// `sv_maxvelocity`, the per-axis clamp Quake puts on a falling body.
const LEAP_MAX_FALL: i32 = 2_000;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MonsterKind {
    Soldier,
    Dog,
    Ogre,
    Zombie,
    Knight,
    Wizard,
    Shambler,
    Demon,
    Boss,
}

impl MonsterKind {
    pub const fn from_class_name(class_name: u8) -> Option<Self> {
        match class_name {
            CLASS_ARMY => Some(Self::Soldier),
            CLASS_DOG => Some(Self::Dog),
            CLASS_OGRE => Some(Self::Ogre),
            CLASS_ZOMBIE => Some(Self::Zombie),
            CLASS_KNIGHT => Some(Self::Knight),
            CLASS_WIZARD => Some(Self::Wizard),
            CLASS_SHAMBLER => Some(Self::Shambler),
            CLASS_DEMON => Some(Self::Demon),
            CLASS_BOSS => Some(Self::Boss),
            _ => None,
        }
    }

    /// The cooked class byte, the inverse of [`Self::from_class_name`].
    pub const fn class_name(self) -> u8 {
        match self {
            Self::Soldier => CLASS_ARMY,
            Self::Dog => CLASS_DOG,
            Self::Ogre => CLASS_OGRE,
            Self::Zombie => CLASS_ZOMBIE,
            Self::Knight => CLASS_KNIGHT,
            Self::Wizard => CLASS_WIZARD,
            Self::Shambler => CLASS_SHAMBLER,
            Self::Demon => CLASS_DEMON,
            Self::Boss => CLASS_BOSS,
        }
    }

    pub const fn model_id(self) -> i16 {
        match self {
            Self::Soldier => SOLDIER_MODEL_ID,
            Self::Dog => DOG_MODEL_ID,
            Self::Ogre => OGRE_MODEL_ID,
            Self::Zombie => ZOMBIE_MODEL_ID,
            Self::Knight => KNIGHT_MODEL_ID,
            Self::Wizard => WIZARD_MODEL_ID,
            Self::Shambler => SHAMBLER_MODEL_ID,
            Self::Demon => DEMON_MODEL_ID,
            Self::Boss => BOSS_MODEL_ID,
        }
    }

    /// The one-frame `h_*.mdl` passed to each original monster's
    /// `ThrowHead`. Chthon has no entry: weapons cannot gib him and his
    /// authored lightning death never throws a head.
    pub const fn gib_head_model_id(self) -> Option<i16> {
        Some(match self {
            Self::Soldier => SOLDIER_HEAD_MODEL_ID,
            Self::Dog => DOG_HEAD_MODEL_ID,
            Self::Ogre => OGRE_HEAD_MODEL_ID,
            Self::Zombie => ZOMBIE_HEAD_MODEL_ID,
            Self::Knight => KNIGHT_HEAD_MODEL_ID,
            Self::Wizard => WIZARD_HEAD_MODEL_ID,
            Self::Shambler => SHAMBLER_HEAD_MODEL_ID,
            Self::Demon => DEMON_HEAD_MODEL_ID,
            Self::Boss => return None,
        })
    }

    /// Clip hull used by this monster's own movement traces. Quake sizes the
    /// ogre, shambler, demon, and dog from hull 2 and everything else hull 1.
    pub const fn collision_hull(self) -> usize {
        match self {
            Self::Soldier | Self::Zombie | Self::Knight | Self::Wizard => 1,
            Self::Dog | Self::Ogre | Self::Shambler | Self::Demon | Self::Boss => 2,
        }
    }

    /// Flying monsters do not step down to a floor; they clip a box move and
    /// trim their height toward the enemy instead.
    pub const fn flies(self) -> bool {
        matches!(self, Self::Wizard)
    }

    /// Health below which the death sequence is replaced by gibs, from each
    /// `*_start_die` in the preserved runtime.
    pub const fn gib_health(self) -> i16 {
        match self {
            Self::Soldier | Self::Dog => MONSTER_GIB_HEALTH,
            Self::Knight | Self::Wizard => -40,
            Self::Ogre | Self::Demon => -80,
            Self::Shambler => -60,
            // The zombie always gibs, and Chthon never dies from damage.
            Self::Zombie | Self::Boss => 0,
        }
    }

    /// Eye height used for sight and range, in whole units.
    pub const fn view_height(self) -> i32 {
        match self {
            Self::Boss => 128,
            _ => 25,
        }
    }

    /// Turn rate per think, in Quake's 4096-unit turn.
    pub const fn yaw_speed(self) -> i16 {
        match self {
            // TO_DEG16(20) and TO_DEG16(10).
            Self::Wizard => 114,
            _ => 228,
        }
    }

    pub const fn sight_sound(self) -> i16 {
        match self {
            Self::Soldier => SOLDIER_SIGHT_SOUND,
            Self::Dog => DOG_SIGHT_SOUND,
            Self::Ogre => OGRE_SIGHT_SOUND,
            Self::Zombie => ZOMBIE_IDLE_SOUND,
            Self::Knight => KNIGHT_SIGHT_SOUND,
            Self::Wizard => WIZARD_SIGHT_SOUND,
            Self::Shambler => SHAMBLER_SIGHT_SOUND,
            Self::Demon => DEMON_SIGHT_SOUND,
            Self::Boss => BOSS_SIGHT_SOUND,
        }
    }

    pub const fn idle_sound(self) -> i16 {
        match self {
            Self::Soldier => SOLDIER_IDLE_SOUND,
            Self::Dog => DOG_IDLE_SOUND,
            Self::Ogre => OGRE_IDLE2_SOUND,
            Self::Zombie => ZOMBIE_IDLE_SOUND,
            Self::Knight => KNIGHT_IDLE_SOUND,
            Self::Wizard => WIZARD_IDLE_SOUND,
            Self::Shambler => SHAMBLER_IDLE_SOUND,
            Self::Demon => DEMON_IDLE_SOUND,
            Self::Boss => BOSS_SIGHT_SOUND,
        }
    }

    pub const fn pain_sound(self) -> i16 {
        match self {
            Self::Soldier => SOLDIER_PAIN_SOUND,
            Self::Dog => DOG_PAIN_SOUND,
            Self::Ogre => OGRE_PAIN_SOUND,
            Self::Zombie => ZOMBIE_PAIN_SOUND,
            Self::Knight => KNIGHT_PAIN_SOUND,
            Self::Wizard => WIZARD_PAIN_SOUND,
            Self::Shambler => SHAMBLER_PAIN_SOUND,
            Self::Demon => DEMON_PAIN_SOUND,
            Self::Boss => BOSS_PAIN_SOUND,
        }
    }

    pub const fn death_sound(self) -> i16 {
        match self {
            Self::Soldier => SOLDIER_DEATH_SOUND,
            Self::Dog => DOG_DEATH_SOUND,
            Self::Ogre => OGRE_DEATH_SOUND,
            Self::Zombie => ZOMBIE_GIB_SOUND,
            Self::Knight => KNIGHT_DEATH_SOUND,
            Self::Wizard => WIZARD_DEATH_SOUND,
            Self::Shambler => SHAMBLER_DEATH_SOUND,
            Self::Demon => DEMON_DEATH_SOUND,
            Self::Boss => BOSS_DEATH_SOUND,
        }
    }

    /// Voice on a gib death: `player/udeath.wav` everywhere except the
    /// zombie's own `zombie/z_gib.wav`.
    pub const fn gib_sound(self) -> i16 {
        match self {
            Self::Zombie => ZOMBIE_GIB_SOUND,
            _ => MONSTER_GIB_SOUND,
        }
    }

    /// Chthon loops his throw cycle instead of returning to run, and is woken
    /// by his authored trigger rather than by sight.
    pub const fn is_boss(self) -> bool {
        matches!(self, Self::Boss)
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MonsterState {
    Stand,
    Walk,
    Run,
    Missile,
    Melee,
    MeleeB,
    MeleeC,
    PainA,
    PainB,
    PainC,
    PainD,
    PainE,
    DeathA,
    DeathB,
    Gib,
    Rise,
    Crucified,
}

impl MonsterState {
    pub const fn is_pain(self) -> bool {
        matches!(
            self,
            Self::PainA | Self::PainB | Self::PainC | Self::PainD | Self::PainE
        )
    }

    pub const fn is_melee(self) -> bool {
        matches!(self, Self::Melee | Self::MeleeB | Self::MeleeC)
    }

    pub const fn is_death(self) -> bool {
        matches!(self, Self::DeathA | Self::DeathB | Self::Gib)
    }
}

/// One attack event authored on a specific animation frame.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MonsterAttack {
    /// Four soldier bullets with the authored per-shot spread.
    SoldierShot { spread: [[i16; 2]; 4] },
    /// Instant contact damage if the target centre is within `reach` units.
    Contact { damage: i16, reach: i32 },
    /// `dog_leap1` / `demon1_jump1`: the monster leaves the ground as a
    /// projectile with this launch velocity, units per second along its yaw
    /// and straight up. The blow lands on whatever the arc touches, so the
    /// host flies the body and asks [`MonsterRuntime::leap_touch_damage`].
    Leap { forward: i16, up: i16 },
    /// Ogre grenade: 40 damage, 600 units/second toward the enemy, +200 z.
    Grenade { damage: i16 },
    /// Zombie flesh gib: 10 damage on touch, 600 units/second, +200 z.
    ZombieGib { damage: i16, offset: [i16; 3] },
    /// Wizard acid spit: one 9-damage spike at 600 units/second. `Wiz_StartFast`
    /// arms two of these per attack, so the missile state emits it twice.
    Spit { damage: i16, side: i16 },
    /// Shambler lightning bolt over a 600-unit trace.
    Lightning { damage: i16 },
    /// Chthon lava ball, launched `side` units right of his centre.
    LavaBall { damage: i16, side: i16 },
}

/// The velocity of a monster in mid-leap, units per second: `forward` runs
/// along the yaw it launched with, `up` is the vertical term with the gravity
/// so far already taken out.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MonsterLeap {
    pub forward: i16,
    pub up: i16,
}

/// Who a monster hunts. `T_Damage` from another monster of a different class
/// (or from any monster onto a soldier) switches the enemy to that monster;
/// its death, or damage from the player, switches back.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum MonsterEnemy {
    #[default]
    Player,
    /// Host-side entity index of the enemy monster.
    Monster(u16),
}

/// The `player_*` fields describe the current enemy, which is the player until
/// infighting substitutes another monster's origin and life for it.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MonsterThinkInput {
    pub distance: i32,
    pub visible: bool,
    pub in_front: bool,
    pub player_hostile: bool,
    pub player_alive: bool,
    pub leap_height_ok: bool,
    /// `FindTarget` refuses a client holding `IT_INVISIBILITY`. An already
    /// acquired monster keeps its enemy, so this only gates acquisition.
    pub player_invisible: bool,
    /// The current `path_corner` the host resolved for this monster, if any.
    /// A resting monster walks toward it; the host turns and steps it.
    pub goal: Option<Vec3I32>,
    /// `sight_entity`: another monster that just found the player is in range
    /// and visible from here, so a non-ambush monster wakes with it.
    pub pack_alert: bool,
    /// `skill == 3`. Carried per think rather than stored per monster: `skill`
    /// is a QuakeC global, and [`MonsterRuntime`] is exactly full at
    /// twenty-four bytes.
    pub nightmare: bool,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MonsterAction {
    pub frame: u16,
    /// Units to move along the facing yaw this think. Negative values move
    /// backwards, which is how Quake's `ai_pain` recoil is expressed.
    pub move_units: i16,
    pub face_target: bool,
    /// Turn toward [`MonsterThinkInput::goal`] instead of the enemy.
    pub face_goal: bool,
    pub attack: Option<MonsterAttack>,
    pub sound_id: Option<i16>,
    /// The voice is `ATTN_IDLE` rather than `ATTN_NORM`. Quake clips an idle
    /// voice at 1000/1.5 units and a normal voice at 1000, so the guest can
    /// drop a distant idle instead of playing every monster in the map.
    pub sound_idle: bool,
    pub activated: bool,
    pub corpse_finished: bool,
    /// `DropBackpack` fires on this death frame (`army_die3`, `ogre_die3`).
    pub drop_backpack: Option<BackpackAmmo>,
}

/// What a dropped backpack carries: `self.ammo_*` set just before
/// `DropBackpack`. Only the shareware droppers exist (soldier, ogre).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct BackpackAmmo {
    pub shells: u8,
    pub rockets: u8,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MonsterDamageTransition {
    pub sound_id: Option<i16>,
    pub killed: bool,
    pub gibbed: bool,
    /// The zombie resets to full health on every survivable hit, so only a
    /// single gib-level blow can ever kill it.
    pub reset_health: bool,
    pub frame: u16,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MonsterRuntime {
    kind: MonsterKind,
    state: MonsterState,
    frame: u16,
    /// Ticks toward the next think, always below `MONSTER_THINK_TICKS + 4`.
    think_ticks: u8,
    attack_cooldown: u16,
    pain_cooldown: u16,
    hold_ticks: u16,
    random_state: u32,
    /// Narrowed to a byte so the leap counter below fits the 24-byte budget;
    /// Chthon never takes more than [`BOSS_HARD_SHOCKS`] of them.
    boss_shocks: i8,
    /// Ticks since `dog_leap1` / `demon1_jump1` launched this monster, biased
    /// by one so that zero means it is on the ground. The velocity is rebuilt
    /// from the count rather than integrated, so a 10 Hz think and a 60 Hz
    /// body share one arc without drift.
    leap_ticks: u8,
    /// `pause_time`: ticks left standing at a `path_corner` before walking on.
    pause_ticks: u16,
    /// Host index of the enemy monster, or [`NO_ENEMY_MONSTER`] for the
    /// player. Stored narrow so the runtime stays inside 24 bytes.
    enemy_index: u16,
    active: bool,
    corpse_finished: bool,
    /// `SPAWNFLAG_AMBUSH`: never woken by another monster's sighting.
    ambush: bool,
}

impl MonsterRuntime {
    pub const fn new(kind: MonsterKind, source_index: u16) -> Self {
        let initial = initial_state(kind, false);
        Self {
            kind,
            state: initial,
            frame: first_frame(kind, initial),
            think_ticks: (source_index % MONSTER_THINK_TICKS) as u8,
            attack_cooldown: 0,
            pain_cooldown: 0,
            hold_ticks: 0,
            random_state: 0x51f1_5e1d ^ (source_index as u32).wrapping_mul(0x9e37_79b9),
            boss_shocks: BOSS_HARD_SHOCKS as i8,
            leap_ticks: 0,
            pause_ticks: 0,
            enemy_index: NO_ENEMY_MONSTER,
            active: false,
            corpse_finished: false,
            ambush: false,
        }
    }

    pub fn set_ambush(&mut self, ambush: bool) {
        self.ambush = ambush;
    }

    pub const fn enemy(self) -> MonsterEnemy {
        if self.enemy_index == NO_ENEMY_MONSTER {
            MonsterEnemy::Player
        } else {
            MonsterEnemy::Monster(self.enemy_index)
        }
    }

    /// `T_Damage`'s infight branch and `monster_use`: hunt `enemy` from now
    /// on. Waking from rest is `FoundTarget`; a monster already hunting just
    /// swaps targets. Returns whether the monster was woken by this call.
    pub fn set_enemy(&mut self, enemy: MonsterEnemy) -> bool {
        if self.dead() || self.crucified() || self.kind.is_boss() {
            return false;
        }
        self.enemy_index = match enemy {
            MonsterEnemy::Player => NO_ENEMY_MONSTER,
            MonsterEnemy::Monster(index) => index,
        };
        if self.active {
            return false;
        }
        self.active = true;
        self.attack_cooldown = INITIAL_ATTACK_COOLDOWN;
        if matches!(self.state, MonsterState::Stand | MonsterState::Walk) {
            self.enter_state(MonsterState::Run);
        }
        true
    }

    /// `t_movetarget`: the monster touched its corner. A corner with `wait`
    /// holds it standing that long before it walks toward the next goal the
    /// host hands it; a corner without a next goal is a stand forever.
    pub fn arrive_at_goal(&mut self, wait_ticks: u16) {
        self.pause_ticks = wait_ticks;
        if wait_ticks != 0 && matches!(self.state, MonsterState::Walk) {
            self.enter_state(MonsterState::Stand);
        }
    }

    /// Crucified zombies are authored decoration: they hang, idle, and are
    /// never counted or woken.
    pub const fn new_crucified(source_index: u16) -> Self {
        let mut runtime = Self::new(MonsterKind::Zombie, source_index);
        runtime.state = MonsterState::Crucified;
        runtime.frame = first_frame(MonsterKind::Zombie, MonsterState::Crucified);
        runtime
    }

    pub const fn kind(self) -> MonsterKind {
        self.kind
    }

    pub const fn state(self) -> MonsterState {
        self.state
    }

    pub const fn frame(self) -> u16 {
        self.frame
    }

    pub const fn active(self) -> bool {
        self.active
    }

    pub const fn dead(self) -> bool {
        self.state.is_death()
    }

    pub const fn gibbed(self) -> bool {
        matches!(self.state, MonsterState::Gib)
    }

    pub const fn corpse_finished(self) -> bool {
        self.corpse_finished
    }

    pub const fn crucified(self) -> bool {
        matches!(self.state, MonsterState::Crucified)
    }

    pub const fn boss_shocks(self) -> i16 {
        self.boss_shocks as i16
    }

    /// A monster's box only blocks while it is upright and alive. Quake drops
    /// `SOLID_SLIDEBOX` partway through every death sequence, and the knocked
    /// down zombie is explicitly non-solid until it stands back up.
    pub const fn body_solid(self) -> bool {
        if self.dead() || self.crucified() {
            return false;
        }
        if matches!(self.kind, MonsterKind::Zombie) && matches!(self.state, MonsterState::PainE) {
            // PAINE10 clears SOLID and PAINE12 restores it.
            let down = first_frame(MonsterKind::Zombie, MonsterState::PainE) + 9;
            return self.frame < down || self.frame > down + 2;
        }
        true
    }

    /// The monster is between its launch frame and its landing, so the host
    /// owns the body: it flies the arc every frame while the animation keeps
    /// the ordinary 10 Hz cadence.
    pub const fn leaping(self) -> bool {
        self.leap_ticks != 0
    }

    /// The airborne counter belongs to `trigger_monsterjump`, not a dog or
    /// fiend attack. Non-boss monsters otherwise never read `boss_shocks`.
    pub const fn forced_jump(self) -> bool {
        self.boss_shocks < 0
    }

    /// Carry the arc on by `ticks` and report the velocity to fly with this
    /// frame. Gravity is taken before the move, exactly as `SV_Physics_Toss`
    /// orders it, and `None` means the monster is back on the ground.
    pub fn advance_leap(&mut self, ticks: u16) -> Option<MonsterLeap> {
        if !self.leaping() {
            return None;
        }
        let ticks = if ticks > 4 { 4 } else { ticks } as u8;
        self.leap_ticks = self.leap_ticks.saturating_add(ticks);
        if self.leap_ticks == u8::MAX {
            // Nothing in the original ends a leap that never touches anything.
            // Four seconds of flight means the monster is wedged somewhere the
            // sweep cannot resolve, so put it back on its feet.
            self.land_leap(true);
            return None;
        }
        Some(self.leap_velocity())
    }

    /// `trigger_monsterjump_touch`: borrow the existing airborne counter but
    /// keep the monster's AI state and animation unchanged.
    #[optimize(size)]
    pub fn begin_forced_jump(&mut self) {
        self.boss_shocks = -1;
        self.leap_ticks = 1;
    }

    /// `Dog_JumpTouch` / `Demon_JumpTouch` reaching something that takes
    /// damage: `if (vlen(self.velocity) > n)` gates the roll, so a leap that
    /// has slowed past its apex lands nothing.
    pub fn leap_touch_damage(&mut self) -> Option<i16> {
        if !self.leaping() || self.dead() || self.forced_jump() {
            return None;
        }
        let (threshold, base) = match self.kind {
            MonsterKind::Dog => (300i32, 10i16),
            MonsterKind::Demon => (400, 40),
            _ => return None,
        };
        let leap = self.leap_velocity();
        let forward = i32::from(leap.forward);
        let up = i32::from(leap.up);
        if forward * forward + up * up <= threshold * threshold {
            return None;
        }
        Some(base + (self.random_fraction_q12() % 10) as i16)
    }

    /// The end of the arc. `on_floor` is `checkbottom`: with ground under the
    /// box the monster picks its run cycle back up, without it `dog_leap1`
    /// fires again and it throws itself forward once more.
    pub fn land_leap(&mut self, on_floor: bool) {
        if !self.leaping() {
            return;
        }
        let forced = self.forced_jump();
        self.leap_ticks = 0;
        if forced {
            self.boss_shocks = 0;
            return;
        }
        if self.dead() || self.crucified() {
            return;
        }
        self.enter_state(if on_floor {
            MonsterState::Run
        } else {
            MonsterState::Missile
        });
    }

    fn leap_velocity(self) -> MonsterLeap {
        let (forward, up) = if self.forced_jump() {
            (200, 200)
        } else {
            leap_launch(self.kind)
        };
        let fallen = LEAP_GRAVITY * i32::from(self.leap_ticks.saturating_sub(1)) / 60;
        MonsterLeap {
            forward,
            up: (i32::from(up) - fallen).max(-LEAP_MAX_FALL) as i16,
        }
    }

    /// Give Chthon his authored skill health and start the rise sequence. The
    /// map's encounter trigger owns this edge; sight never wakes him.
    pub fn awaken(&mut self, skill: u8) {
        if !self.kind.is_boss() || self.active {
            return;
        }
        self.boss_shocks = if skill == 0 {
            BOSS_EASY_SHOCKS as i8
        } else {
            BOSS_HARD_SHOCKS as i8
        };
        self.active = true;
        self.enter_state(MonsterState::Rise);
    }

    /// One `event_lightning` shock. Chthon is immune to every weapon; this is
    /// the only thing that can hurt or kill him.
    pub fn apply_shock(&mut self) -> Option<MonsterDamageTransition> {
        if !self.kind.is_boss() || !self.active || self.dead() || self.boss_shocks <= 0 {
            return None;
        }
        self.boss_shocks -= 1;
        let state = match self.boss_shocks {
            remaining if remaining >= 2 => MonsterState::PainA,
            1 => MonsterState::PainB,
            _ => MonsterState::PainC,
        };
        self.enter_state(state);
        Some(MonsterDamageTransition {
            sound_id: Some(BOSS_PAIN_SOUND),
            killed: false,
            gibbed: false,
            reset_health: false,
            frame: self.frame,
        })
    }

    /// Whether [`Self::advance_ticks`] with `elapsed_ticks` reaches a think,
    /// so the host can skip building expensive input on the frames between.
    pub const fn think_due(self, elapsed_ticks: u16) -> bool {
        let elapsed = if elapsed_ticks > 4 { 4 } else { elapsed_ticks };
        (self.think_ticks as u16).saturating_add(elapsed) >= MONSTER_THINK_TICKS
    }

    pub fn advance_ticks(
        &mut self,
        elapsed_ticks: u16,
        input: MonsterThinkInput,
    ) -> Option<MonsterAction> {
        let elapsed = elapsed_ticks.min(4);
        self.attack_cooldown = self.attack_cooldown.saturating_sub(elapsed);
        self.pain_cooldown = self.pain_cooldown.saturating_sub(elapsed);
        self.pause_ticks = self.pause_ticks.saturating_sub(elapsed);
        self.think_ticks = self.think_ticks.saturating_add(elapsed as u8);
        if u16::from(self.think_ticks) < MONSTER_THINK_TICKS {
            return None;
        }
        self.think_ticks -= MONSTER_THINK_TICKS as u8;
        if self.hold_ticks != 0 {
            self.hold_ticks = self.hold_ticks.saturating_sub(MONSTER_THINK_TICKS);
            return Some(MonsterAction {
                frame: self.frame,
                ..MonsterAction::default()
            });
        }
        Some(self.think(input))
    }

    pub fn take_damage(
        &mut self,
        damage: i16,
        remaining_health: i16,
        nightmare: bool,
    ) -> MonsterDamageTransition {
        let unchanged = MonsterDamageTransition {
            frame: self.frame,
            ..MonsterDamageTransition::default()
        };
        if self.dead() || self.crucified() || self.kind.is_boss() {
            return unchanged;
        }
        self.active = true;
        if remaining_health <= 0 {
            return self.start_die(remaining_health);
        }
        // A zombie is the one monster whose pain gate is `self.inpain` rather
        // than `pain_finished`, and `pain_cooldown` stands in for it here, so
        // the nightmare rule below deliberately leaves it alone.
        if matches!(self.kind, MonsterKind::Zombie) {
            return self.zombie_pain(damage);
        }
        if self.pain_cooldown != 0 {
            if nightmare {
                self.pain_cooldown = NIGHTMARE_PAIN_TICKS;
            }
            return unchanged;
        }
        let pain_sound = Some(self.kind.pain_sound());
        let pain = self.choose_pain(damage);
        // `T_Damage`, after `th_pain` has run: "nightmare mode monsters don't
        // go into pain frames often", `self.pain_finished = time + 5`. It is
        // charged whatever the monster's own pain function decided, so the
        // first flinch lands and the next five seconds of them do not.
        if nightmare {
            self.pain_cooldown = NIGHTMARE_PAIN_TICKS;
        }
        let Some(state) = pain else {
            // Quake still voices the wizard and shambler when the hit fails to
            // interrupt their animation.
            return match self.kind {
                MonsterKind::Wizard | MonsterKind::Shambler => MonsterDamageTransition {
                    sound_id: pain_sound,
                    frame: self.frame,
                    ..MonsterDamageTransition::default()
                },
                _ => unchanged,
            };
        };
        self.enter_state(state);
        MonsterDamageTransition {
            sound_id: pain_sound,
            killed: false,
            gibbed: false,
            reset_health: false,
            frame: self.frame,
        }
    }

    fn start_die(&mut self, remaining_health: i16) -> MonsterDamageTransition {
        // `Dog_JumpTouch`: `if (self.health <= 0) return`. A leap killed in
        // the air stops being the host's to fly.
        self.leap_ticks = 0;
        let gibs = matches!(self.kind, MonsterKind::Zombie)
            || remaining_health < self.kind.gib_health()
            || frame_range(self.kind, MonsterState::DeathA).is_none();
        if gibs {
            self.enter_state(MonsterState::Gib);
            return MonsterDamageTransition {
                sound_id: Some(self.kind.gib_sound()),
                killed: true,
                gibbed: true,
                reset_health: false,
                frame: self.frame,
            };
        }
        let use_b = frame_range(self.kind, MonsterState::DeathB).is_some()
            && self.random_fraction_q12() >= (Q12_ONE >> 1);
        let death = if use_b {
            MonsterState::DeathB
        } else {
            MonsterState::DeathA
        };
        self.enter_state(death);
        MonsterDamageTransition {
            sound_id: Some(self.kind.death_sound()),
            killed: true,
            gibbed: false,
            reset_health: false,
            frame: self.frame,
        }
    }

    /// The zombie's whole damage model: every survivable hit restores full
    /// health, small hits are ignored, big hits knock it down, and repeated
    /// hits inside three seconds also knock it down.
    fn zombie_pain(&mut self, damage: i16) -> MonsterDamageTransition {
        let quiet = MonsterDamageTransition {
            reset_health: true,
            frame: self.frame,
            ..MonsterDamageTransition::default()
        };
        if damage < 9 {
            return quiet;
        }
        if matches!(self.state, MonsterState::PainE) {
            return quiet;
        }
        if damage >= 25 {
            self.enter_state(MonsterState::PainE);
            return MonsterDamageTransition {
                sound_id: Some(ZOMBIE_PAIN_SOUND),
                reset_health: true,
                frame: self.frame,
                ..MonsterDamageTransition::default()
            };
        }
        if self.state.is_pain() {
            self.pain_cooldown = 3 * ONE_SECOND;
            return quiet;
        }
        if self.pain_cooldown != 0 {
            self.enter_state(MonsterState::PainE);
            return MonsterDamageTransition {
                sound_id: Some(ZOMBIE_PAIN_SOUND),
                reset_health: true,
                frame: self.frame,
                ..MonsterDamageTransition::default()
            };
        }
        let random = self.random_fraction_q12();
        let state = if random < Q12_ONE / 4 {
            MonsterState::PainA
        } else if random < Q12_ONE / 2 {
            MonsterState::PainB
        } else if random < Q12_ONE * 3 / 4 {
            MonsterState::PainC
        } else {
            MonsterState::PainD
        };
        self.enter_state(state);
        MonsterDamageTransition {
            sound_id: Some(ZOMBIE_PAIN_SOUND),
            reset_health: true,
            frame: self.frame,
            ..MonsterDamageTransition::default()
        }
    }

    /// Per-monster pain selection and refractory period. `None` means the hit
    /// did not interrupt the current animation.
    fn choose_pain(&mut self, damage: i16) -> Option<MonsterState> {
        let random = self.random_fraction_q12();
        match self.kind {
            MonsterKind::Soldier => Some(if random < Q12_ONE / 5 {
                self.pain_cooldown = 36;
                MonsterState::PainA
            } else if random < Q12_ONE * 3 / 5 {
                self.pain_cooldown = 66;
                MonsterState::PainB
            } else {
                self.pain_cooldown = 66;
                MonsterState::PainC
            }),
            MonsterKind::Dog => Some(if random > (Q12_ONE >> 1) {
                MonsterState::PainA
            } else {
                MonsterState::PainB
            }),
            MonsterKind::Ogre => Some(if random < Q12_ONE / 4 {
                self.pain_cooldown = ONE_SECOND;
                MonsterState::PainA
            } else if random < Q12_ONE / 2 {
                self.pain_cooldown = ONE_SECOND;
                MonsterState::PainB
            } else if random < Q12_ONE * 3 / 4 {
                self.pain_cooldown = ONE_SECOND;
                MonsterState::PainC
            } else if random < (Q12_ONE / 25) * 22 {
                self.pain_cooldown = 2 * ONE_SECOND;
                MonsterState::PainD
            } else {
                self.pain_cooldown = 2 * ONE_SECOND;
                MonsterState::PainE
            }),
            MonsterKind::Knight => {
                self.pain_cooldown = ONE_SECOND;
                Some(if random < (Q12_ONE / 20) * 17 {
                    MonsterState::PainA
                } else {
                    MonsterState::PainB
                })
            }
            MonsterKind::Demon => {
                self.pain_cooldown = ONE_SECOND;
                // `if (rand * 200 > damage) return` - a light hit is shrugged off.
                (scale_random(random, 200) <= i32::from(damage)).then_some(MonsterState::PainA)
            }
            MonsterKind::Wizard => {
                // `if ((rand() & 63) > damage) return`.
                (scale_random(random, 64) <= i32::from(damage)).then_some(MonsterState::PainA)
            }
            MonsterKind::Shambler => {
                // `if (400 * rand > damage) return`.
                if scale_random(random, 400) > i32::from(damage) {
                    return None;
                }
                self.pain_cooldown = 2 * ONE_SECOND;
                Some(MonsterState::PainA)
            }
            MonsterKind::Zombie | MonsterKind::Boss => None,
        }
    }

    fn think(&mut self, input: MonsterThinkInput) -> MonsterAction {
        let mut action = self.think_frame(input);
        if self.leaping() {
            // Between the launch frame and the landing the monster is a
            // projectile: the animation keeps running, but the move table and
            // `ai_face` no longer touch a body the host is flying.
            action.move_units = 0;
            action.face_target = false;
            action.face_goal = false;
        }
        action
    }

    fn think_frame(&mut self, input: MonsterThinkInput) -> MonsterAction {
        let mut action = MonsterAction {
            frame: self.frame,
            ..MonsterAction::default()
        };
        match self.state {
            MonsterState::Gib => return action,
            MonsterState::Crucified => {
                if self.frame == first_frame(self.kind, MonsterState::Crucified)
                    && self.random_fraction_q12() < Q12_ONE / 10
                {
                    action.sound_id = Some(ZOMBIE_CRUCIFIED_SOUND);
                    action.sound_idle = true;
                }
                self.advance_looping();
                action.frame = self.frame;
                return action;
            }
            MonsterState::Rise => {
                let first = first_frame(self.kind, MonsterState::Rise);
                if self.frame == first {
                    action.sound_id = Some(BOSS_OUT_SOUND);
                } else if self.frame == first + 1 {
                    action.sound_id = Some(BOSS_SIGHT_SOUND);
                }
                self.advance_finite(Some(MonsterState::Missile));
                action.frame = self.frame;
                return action;
            }
            MonsterState::DeathA | MonsterState::DeathB => {
                action.move_units = death_move(self.kind, self.state, self.frame);
                if let Some(sound) = death_sound_frame(self.kind, self.state, self.frame) {
                    action.sound_id = Some(sound);
                }
                action.drop_backpack = backpack_frame(self.kind, self.state, self.frame);
                action.corpse_finished = self.advance_finite(None);
                self.corpse_finished |= action.corpse_finished;
                action.frame = self.frame;
                return action;
            }
            state if state.is_pain() => {
                let (move_units, sound, hold) = self.pain_frame_event();
                action.move_units = move_units;
                action.sound_id = sound;
                self.hold_ticks = hold;
                let next = if self.kind.is_boss() {
                    Some(boss_pain_exit(state))
                } else {
                    Some(MonsterState::Run)
                };
                self.advance_finite(next);
                action.frame = self.frame;
                return action;
            }
            _ => {}
        }

        if !input.player_alive && !self.kind.is_boss() {
            self.active = false;
            self.enemy_index = NO_ENEMY_MONSTER;
            self.enter_state(MonsterState::Stand);
        }
        if !self.active {
            // Quake's ai_findtarget: never past RANGE_FAR, and inside
            // RANGE_MID the monster must be facing the player. Only inside
            // RANGE_NEAR does a recently fired weapon (`show_hostile`) wake a
            // monster that is looking the other way. It also returns FALSE for
            // a client holding `IT_INVISIBILITY`. A `sight_entity` that just
            // found the player wakes its neighbours too, unless they are
            // authored as an ambush.
            let sees_player = input.visible
                && input.distance < MONSTER_FAR_RANGE
                && (input.distance < MONSTER_MELEE_RANGE
                    || input.in_front
                    || (input.distance < MONSTER_NEAR_RANGE && input.player_hostile));
            let can_acquire = !self.kind.is_boss()
                && input.player_alive
                && !input.player_invisible
                && (sees_player || (input.pack_alert && !self.ambush));
            if can_acquire {
                self.active = true;
                self.enemy_index = NO_ENEMY_MONSTER;
                self.attack_cooldown = INITIAL_ATTACK_COOLDOWN;
                self.enter_state(MonsterState::Run);
                action.frame = self.frame;
                action.sound_id = Some(self.kind.sight_sound());
                action.activated = true;
                return action;
            }
            // `ai_stand` leaves for `th_walk` once `pause_time` has passed and
            // a `path_corner` goal exists; `ai_walk` follows it and drops back
            // to stand when the chain ends.
            if matches!(self.state, MonsterState::Stand)
                && !self.kind.is_boss()
                && input.goal.is_some()
                && self.pause_ticks == 0
            {
                self.enter_state(MonsterState::Walk);
            }
            if matches!(self.state, MonsterState::Walk) {
                if input.goal.is_none() {
                    self.enter_state(MonsterState::Stand);
                } else {
                    action.face_goal = true;
                    action.move_units = move_distance(self.kind, MonsterState::Walk, self.frame);
                }
            }
            if self.rest_idle_voice_due() {
                action.sound_id = Some(OGRE_IDLE_SOUND);
                action.sound_idle = true;
            }
            self.advance_looping();
            action.frame = self.frame;
            return action;
        }

        match self.state {
            MonsterState::Stand | MonsterState::Walk => {
                let next = if self.kind.is_boss() {
                    MonsterState::Missile
                } else {
                    MonsterState::Run
                };
                self.enter_state(next);
                action.frame = self.frame;
            }
            MonsterState::Run => {
                action.face_target = true;
                if input.visible && self.try_begin_attack(input) {
                    action.frame = self.frame;
                    if let Some(sound) = self.enter_attack_sound() {
                        action.sound_id = Some(sound);
                    }
                    return action;
                }
                action.move_units = move_distance(self.kind, MonsterState::Run, self.frame);
                if self.frame >= last_frame(self.kind, MonsterState::Run)
                    && self.random_fraction_q12() < idle_chance_q12(self.kind)
                {
                    // `wiz_idlesound` voices widle2 three times as often as widle1.
                    let idle = if self.kind == MonsterKind::Wizard
                        && self.random_fraction_q12() < Q12_ONE * 3 / 4
                    {
                        WIZARD_IDLE2_SOUND
                    } else {
                        self.kind.idle_sound()
                    };
                    action.sound_id = Some(idle);
                    action.sound_idle = true;
                }
                self.advance_looping();
                action.frame = self.frame;
            }
            state @ (MonsterState::Missile
            | MonsterState::Melee
            | MonsterState::MeleeB
            | MonsterState::MeleeC) => {
                action.face_target = face_during(self.kind, state);
                let (attack, sound, hold) = self.attack_frame_event(state, input);
                action.attack = attack;
                action.sound_id = sound;
                action.move_units = move_distance(self.kind, state, self.frame);
                self.hold_ticks = hold;
                if self.kind.is_boss() {
                    self.advance_looping();
                } else {
                    let next = self.attack_exit(state);
                    self.advance_finite(Some(next));
                }
                action.frame = self.frame;
            }
            _ => {}
        }
        action
    }

    /// Attack selection. This is the only genuinely per-monster decision in the
    /// run state and mirrors each `fn_check_attack` in the preserved runtime.
    fn try_begin_attack(&mut self, input: MonsterThinkInput) -> bool {
        // A leap that outlasts its own animation must not start another one
        // from the air; only the landing arms the next attack.
        if self.leaping() {
            return false;
        }
        // `SUB_AttackFinished`: `if (skill >= 3) self.attack_finished = time`.
        // Nightmare reloads finish the instant they are consulted, which also
        // covers the `SUB_AttackFinished(1)` that `HuntTarget` charges on
        // waking. Each branch below still writes its authored reload, and the
        // next think clears it again.
        if input.nightmare {
            self.attack_cooldown = 0;
        }
        match self.kind {
            MonsterKind::Dog if input.distance < MONSTER_MELEE_RANGE => {
                self.enter_state(MonsterState::Melee);
                true
            }
            MonsterKind::Dog if (80..150).contains(&input.distance) && input.leap_height_ok => {
                self.enter_state(MonsterState::Missile);
                true
            }
            MonsterKind::Dog => false,
            MonsterKind::Demon if input.distance < MONSTER_MELEE_RANGE => {
                self.enter_state(MonsterState::Melee);
                true
            }
            MonsterKind::Demon => {
                // demon_check_jump: never below 100 units, and beyond 200 only
                // one think in ten.
                if !input.leap_height_ok || input.distance < 100 {
                    return false;
                }
                if input.distance > 200 && self.random_fraction_q12() < Q12_ONE * 9 / 10 {
                    return false;
                }
                self.enter_state(MonsterState::Missile);
                true
            }
            MonsterKind::Knight if input.distance < MONSTER_MELEE_RANGE => {
                // fight.qc knight_attack: the standing swing inside 80 units,
                // the running attack beyond it.
                let state = if input.distance < 80 {
                    MonsterState::Melee
                } else {
                    MonsterState::MeleeB
                };
                self.enter_state(state);
                true
            }
            MonsterKind::Knight => false,
            MonsterKind::Shambler if input.distance < MONSTER_MELEE_RANGE => {
                // sham_melee: smash above 0.6, right swing above 0.3, else
                // left swing. The full-health smash override needs health the
                // runtime does not carry, so it is not modelled.
                let chance = self.random_fraction_q12();
                let state = if chance > Q12_ONE * 3 / 5 {
                    MonsterState::Melee
                } else if chance > Q12_ONE * 3 / 10 {
                    MonsterState::MeleeC
                } else {
                    MonsterState::MeleeB
                };
                self.enter_state(state);
                true
            }
            MonsterKind::Ogre if input.distance < MONSTER_MELEE_RANGE => {
                // ogre_melee: smash on a coin flip, otherwise swing.
                let state = if self.random_fraction_q12() > (Q12_ONE >> 1) {
                    MonsterState::MeleeB
                } else {
                    MonsterState::Melee
                };
                self.enter_state(state);
                true
            }
            MonsterKind::Shambler | MonsterKind::Ogre => {
                // Both cap their bolt or grenade at 600 units and reload for
                // two to four seconds.
                if self.attack_cooldown != 0 || input.distance > 600 {
                    return false;
                }
                self.attack_cooldown = 2 * ONE_SECOND
                    + ((u32::from(self.random_fraction_q12()) * u32::from(2 * ONE_SECOND)) >> 12)
                        as u16;
                self.enter_state(MonsterState::Missile);
                true
            }
            MonsterKind::Wizard => {
                if self.attack_cooldown != 0 || input.distance >= MONSTER_FAR_RANGE {
                    return false;
                }
                let chance = if input.distance < MONSTER_MELEE_RANGE {
                    Q12_ONE * 9 / 10
                } else if input.distance < MONSTER_NEAR_RANGE {
                    Q12_ONE * 3 / 5
                } else {
                    Q12_ONE / 5
                };
                if self.random_fraction_q12() >= chance {
                    return false;
                }
                self.attack_cooldown = 2 * ONE_SECOND;
                self.enter_state(MonsterState::Missile);
                true
            }
            MonsterKind::Zombie => {
                if self.attack_cooldown != 0 || input.distance >= MONSTER_FAR_RANGE {
                    return false;
                }
                self.attack_cooldown = ONE_SECOND;
                // One of three authored throw animations.
                let random = self.random_fraction_q12();
                let state = if random < Q12_ONE * 3 / 10 {
                    MonsterState::Missile
                } else if random < Q12_ONE * 3 / 5 {
                    MonsterState::MeleeB
                } else {
                    MonsterState::MeleeC
                };
                self.enter_state(state);
                true
            }
            MonsterKind::Soldier if self.attack_cooldown == 0 => {
                let chance = if input.distance < MONSTER_MELEE_RANGE {
                    Q12_ONE * 9 / 10
                } else if input.distance < MONSTER_NEAR_RANGE {
                    Q12_ONE * 2 / 5
                } else if input.distance < MONSTER_FAR_RANGE {
                    Q12_ONE / 20
                } else {
                    0
                };
                if self.random_fraction_q12() >= chance {
                    return false;
                }
                self.attack_cooldown =
                    60 + ((u32::from(self.random_fraction_q12()) * 60) >> 12) as u16;
                self.enter_state(MonsterState::Missile);
                true
            }
            MonsterKind::Soldier | MonsterKind::Boss => false,
        }
    }

    /// The ogre is the one monster with a distinct resting voice: `ogre_stand5`
    /// and `ogre_walk3` play `ogre/ogidle.wav` one time in five, while its run
    /// cycle uses `ogidle2`.
    fn rest_idle_voice_due(&mut self) -> bool {
        if !matches!(self.kind, MonsterKind::Ogre) {
            return false;
        }
        let offset = self.frame.saturating_sub(first_frame(self.kind, self.state));
        let authored = match self.state {
            MonsterState::Stand => offset == 4,
            MonsterState::Walk => offset == 2,
            _ => false,
        };
        authored && self.random_fraction_q12() < Q12_ONE / 5
    }

    /// State entered when an attack animation runs out. Everything returns to
    /// run except the shambler's swings: `sham_swingl9` and `sham_swingr9`
    /// chain into the opposite swing half the time.
    fn attack_exit(&mut self, state: MonsterState) -> MonsterState {
        if !matches!(self.kind, MonsterKind::Shambler)
            || self.frame < last_frame(self.kind, state)
        {
            return MonsterState::Run;
        }
        match state {
            MonsterState::MeleeB | MonsterState::MeleeC
                if self.random_fraction_q12() < (Q12_ONE >> 1) =>
            {
                if matches!(state, MonsterState::MeleeB) {
                    MonsterState::MeleeC
                } else {
                    MonsterState::MeleeB
                }
            }
            _ => MonsterState::Run,
        }
    }

    /// Sound played on the think that enters an attack state, where the
    /// original voices the decision rather than the first animation frame.
    fn enter_attack_sound(&mut self) -> Option<i16> {
        match (self.kind, self.state) {
            (MonsterKind::Demon, MonsterState::Missile) => Some(DEMON_JUMP_SOUND),
            _ => None,
        }
    }

    /// Per-frame attack events. The frame numbers are the authored ones; the
    /// damage rolls are the authored formulas.
    fn attack_frame_event(
        &mut self,
        state: MonsterState,
        input: MonsterThinkInput,
    ) -> (Option<MonsterAttack>, Option<i16>, u16) {
        let first = first_frame(self.kind, state);
        let offset = self.frame.saturating_sub(first);
        match (self.kind, state) {
            (MonsterKind::Soldier, MonsterState::Missile) if offset == 4 => (
                Some(MonsterAttack::SoldierShot {
                    spread: self.soldier_spread(),
                }),
                Some(SOLDIER_ATTACK_SOUND),
                0,
            ),
            (MonsterKind::Dog, MonsterState::Melee) if offset == 3 => (
                Some(MonsterAttack::Contact {
                    damage: self.scaled_three_random(8),
                    reach: MONSTER_MELEE_RANGE,
                }),
                Some(DOG_ATTACK_SOUND),
                0,
            ),
            (MonsterKind::Dog, MonsterState::Missile) if offset == 1 => {
                (Some(self.launch_leap()), None, 0)
            }
            (MonsterKind::Knight, MonsterState::Melee) if offset == 0 => {
                (None, Some(KNIGHT_SWORD_SOUND), 0)
            }
            (MonsterKind::Knight, MonsterState::Melee) if (5..=7).contains(&offset) => (
                Some(MonsterAttack::Contact {
                    damage: self.scaled_three_random(3),
                    reach: 60,
                }),
                None,
                0,
            ),
            (MonsterKind::Knight, MonsterState::MeleeB) if offset == 0 => {
                (None, Some(KNIGHT_SWORD2_SOUND), 0)
            }
            (MonsterKind::Knight, MonsterState::MeleeB) if (4..=8).contains(&offset) => (
                Some(MonsterAttack::Contact {
                    damage: self.scaled_three_random(3),
                    reach: 60,
                }),
                None,
                0,
            ),
            (MonsterKind::Ogre, MonsterState::Missile) if offset == 2 => (
                Some(MonsterAttack::Grenade { damage: 40 }),
                Some(GRENADE_LAUNCH_SOUND),
                0,
            ),
            (MonsterKind::Ogre, MonsterState::Melee) if offset == 0 => {
                (None, Some(OGRE_SAW_SOUND), 0)
            }
            (MonsterKind::Ogre, MonsterState::Melee) if (4..=10).contains(&offset) => (
                Some(MonsterAttack::Contact {
                    damage: self.scaled_three_random(4),
                    reach: MONSTER_MELEE_RANGE,
                }),
                None,
                0,
            ),
            (MonsterKind::Ogre, MonsterState::MeleeB) if offset == 0 => {
                (None, Some(OGRE_SAW_SOUND), 0)
            }
            (MonsterKind::Ogre, MonsterState::MeleeB)
                if matches!(offset, 5 | 6 | 7 | 8 | 10 | 11) =>
            {
                (
                    Some(MonsterAttack::Contact {
                        damage: self.scaled_three_random(4),
                        reach: MONSTER_MELEE_RANGE,
                    }),
                    None,
                    0,
                )
            }
            (MonsterKind::Demon, MonsterState::Melee) if offset == 4 || offset == 10 => (
                Some(MonsterAttack::Contact {
                    damage: 10 + scale_random(self.random_fraction_q12(), 5) as i16,
                    reach: MONSTER_MELEE_RANGE,
                }),
                Some(DEMON_HIT_SOUND),
                0,
            ),
            (MonsterKind::Demon, MonsterState::Missile) if offset == 3 => {
                (Some(self.launch_leap()), None, 0)
            }
            (MonsterKind::Demon, MonsterState::Missile) if offset == 9 => {
                // LEAP10 waits up to three seconds for a touch before retrying.
                (None, None, 3 * ONE_SECOND)
            }
            (MonsterKind::Shambler, MonsterState::Melee) if offset == 0 => {
                (None, Some(SHAMBLER_MELEE1_SOUND), 0)
            }
            (MonsterKind::Shambler, MonsterState::Melee) if offset == 9 => (
                // sham_smash10: (random() + random() + random()) * 40.
                Some(MonsterAttack::Contact {
                    damage: self.scaled_three_random(40),
                    reach: MONSTER_MELEE_RANGE,
                }),
                Some(SHAMBLER_SMACK_SOUND),
                0,
            ),
            (MonsterKind::Shambler, MonsterState::MeleeB) if offset == 0 => {
                (None, Some(SHAMBLER_MELEE1_SOUND), 0)
            }
            (MonsterKind::Shambler, MonsterState::MeleeC) if offset == 0 => {
                (None, Some(SHAMBLER_MELEE2_SOUND), 0)
            }
            (MonsterKind::Shambler, MonsterState::MeleeB | MonsterState::MeleeC) if offset == 6 => {
                (
                    Some(MonsterAttack::Contact {
                        damage: self.scaled_three_random(20),
                        reach: MONSTER_MELEE_RANGE,
                    }),
                    Some(SHAMBLER_SMACK_SOUND),
                    0,
                )
            }
            (MonsterKind::Shambler, MonsterState::Missile) if offset == 0 => {
                (None, Some(SHAMBLER_MAGIC_SOUND), 0)
            }
            (MonsterKind::Shambler, MonsterState::Missile) if offset == 2 => {
                // MAGIC3 holds for two extra frames while the hands charge.
                (None, None, 2 * MONSTER_THINK_TICKS)
            }
            (MonsterKind::Shambler, MonsterState::Missile)
                if matches!(offset, 5 | 8 | 9) && input.visible =>
            {
                (
                    Some(MonsterAttack::Lightning { damage: 10 }),
                    (offset == 5).then_some(SHAMBLER_BOOM_SOUND),
                    0,
                )
            }
            (MonsterKind::Wizard, MonsterState::Missile) if offset == 0 => {
                // Wiz_StartFast voices the wind-up and arms two spikes.
                (None, Some(WIZARD_ATTACK_SOUND), 0)
            }
            (MonsterKind::Wizard, MonsterState::Missile) if offset == 3 || offset == 8 => (
                // Wiz_FastFire: the left spike 0.3 s in, the right one 0.8 s in,
                // each voiced again on launch.
                Some(MonsterAttack::Spit {
                    damage: 9,
                    side: if offset == 3 { -14 } else { 14 },
                }),
                Some(WIZARD_ATTACK_SOUND),
                0,
            ),
            (MonsterKind::Zombie, MonsterState::Missile) if offset == 12 => (
                Some(MonsterAttack::ZombieGib {
                    damage: 10,
                    offset: [-10, -22, 30],
                }),
                Some(ZOMBIE_THROW_SOUND),
                0,
            ),
            (MonsterKind::Zombie, MonsterState::MeleeB) if offset == 12 => (
                Some(MonsterAttack::ZombieGib {
                    damage: 10,
                    offset: [-10, -24, 29],
                }),
                Some(ZOMBIE_THROW_SOUND),
                0,
            ),
            (MonsterKind::Zombie, MonsterState::MeleeC) if offset == 11 => (
                Some(MonsterAttack::ZombieGib {
                    damage: 10,
                    offset: [-12, -19, 29],
                }),
                Some(ZOMBIE_THROW_SOUND),
                0,
            ),
            (MonsterKind::Boss, MonsterState::Missile) if offset == 8 => (
                Some(MonsterAttack::LavaBall {
                    damage: self.lava_ball_damage(),
                    side: 100,
                }),
                Some(BOSS_THROW_SOUND),
                0,
            ),
            (MonsterKind::Boss, MonsterState::Missile) if offset == 19 => (
                Some(MonsterAttack::LavaBall {
                    damage: self.lava_ball_damage(),
                    side: -100,
                }),
                Some(BOSS_THROW_SOUND),
                0,
            ),
            _ => (None, None, 0),
        }
    }

    /// Per-frame pain recoil, sounds, and the zombie's five-second knockdown.
    fn pain_frame_event(&mut self) -> (i16, Option<i16>, u16) {
        let first = first_frame(self.kind, self.state);
        let offset = self.frame.saturating_sub(first);
        let move_units = pain_move(self.kind, self.state, offset);
        if matches!(self.kind, MonsterKind::Zombie) {
            match (self.state, offset) {
                (MonsterState::PainB, 8) => return (move_units, Some(ZOMBIE_FALL_SOUND), 0),
                (MonsterState::PainE, 9) => return (move_units, Some(ZOMBIE_FALL_SOUND), 0),
                (MonsterState::PainE, 10) => return (move_units, None, 5 * ONE_SECOND),
                (MonsterState::PainE, 11) => return (move_units, Some(ZOMBIE_IDLE_SOUND), 0),
                _ => {}
            }
        }
        if self.kind.is_boss() && offset == 0 {
            return (move_units, Some(BOSS_PAIN_SOUND), 0);
        }
        (move_units, None, 0)
    }

    /// The launch frame itself: the monster leaves the ground and the host
    /// takes its body over until something stops the arc.
    fn launch_leap(&mut self) -> MonsterAttack {
        let (forward, up) = leap_launch(self.kind);
        self.leap_ticks = 1;
        MonsterAttack::Leap { forward, up }
    }

    fn soldier_spread(&mut self) -> [[i16; 2]; 4] {
        let mut spread = [[0; 2]; 4];
        let mut index = 0usize;
        while index < spread.len() {
            spread[index] = [
                self.trace_random_q12() as i16,
                self.trace_random_q12() as i16,
            ];
            index += 1;
        }
        spread
    }

    /// `T_MissileTouch`: a lava ball is a rocket, `100 + random() * 20` on a
    /// direct hit plus 120 radius damage handled by the launcher.
    fn lava_ball_damage(&mut self) -> i16 {
        100 + scale_random(self.random_fraction_q12(), 20) as i16
    }

    /// Quake's `xmul32(xrand32() + xrand32() + xrand32(), scale)`.
    fn scaled_three_random(&mut self, scale: u32) -> i16 {
        let sum = u32::from(self.random_fraction_q12())
            + u32::from(self.random_fraction_q12())
            + u32::from(self.random_fraction_q12());
        ((sum * scale) >> 12) as i16
    }

    fn trace_random_q12(&mut self) -> i32 {
        2 * (i32::from(self.random_fraction_q12()) - i32::from(Q12_ONE >> 1))
    }

    fn random_fraction_q12(&mut self) -> u16 {
        self.random_state = self
            .random_state
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        ((self.random_state >> 1) as u16) & (Q12_ONE - 1)
    }

    fn enter_state(&mut self, state: MonsterState) {
        self.state = state;
        self.frame = first_frame(self.kind, state);
        self.hold_ticks = 0;
        self.corpse_finished = false;
    }

    fn advance_looping(&mut self) {
        let last = last_frame(self.kind, self.state);
        self.frame = if self.frame >= last {
            first_frame(self.kind, self.state)
        } else {
            self.frame + 1
        };
    }

    fn advance_finite(&mut self, next: Option<MonsterState>) -> bool {
        if self.frame < last_frame(self.kind, self.state) {
            self.frame += 1;
            false
        } else if let Some(next) = next {
            self.enter_state(next);
            true
        } else {
            true
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MonsterFrameRange {
    pub first: u16,
    pub last: u16,
}

/// Absolute alias-model frame ranges, transcribed from each preserved
/// `monster_*_states` table. `None` means the monster has no such state.
pub const fn frame_range(kind: MonsterKind, state: MonsterState) -> Option<MonsterFrameRange> {
    let (first, last) = match (kind, state) {
        // monster_army.c
        (MonsterKind::Soldier, MonsterState::Stand) => (0, 7),
        (MonsterKind::Soldier, MonsterState::Walk) => (79, 102),
        (MonsterKind::Soldier, MonsterState::Run) => (62, 69),
        (MonsterKind::Soldier, MonsterState::Missile) => (70, 78),
        (MonsterKind::Soldier, MonsterState::DeathA) => (8, 17),
        (MonsterKind::Soldier, MonsterState::DeathB) => (18, 28),
        (MonsterKind::Soldier, MonsterState::PainA) => (29, 34),
        (MonsterKind::Soldier, MonsterState::PainB) => (35, 48),
        (MonsterKind::Soldier, MonsterState::PainC) => (49, 61),
        // monster_dog.c
        (MonsterKind::Dog, MonsterState::Stand) => (69, 77),
        (MonsterKind::Dog, MonsterState::Walk) => (78, 85),
        (MonsterKind::Dog, MonsterState::Run) => (48, 59),
        (MonsterKind::Dog, MonsterState::Missile) => (60, 68),
        (MonsterKind::Dog, MonsterState::Melee) => (0, 7),
        (MonsterKind::Dog, MonsterState::DeathA) => (8, 16),
        (MonsterKind::Dog, MonsterState::DeathB) => (17, 25),
        (MonsterKind::Dog, MonsterState::PainA) => (26, 31),
        (MonsterKind::Dog, MonsterState::PainB) => (32, 47),
        // monster_ogre.c
        (MonsterKind::Ogre, MonsterState::Stand) => (0, 8),
        (MonsterKind::Ogre, MonsterState::Walk) => (9, 24),
        (MonsterKind::Ogre, MonsterState::Run) => (25, 32),
        (MonsterKind::Ogre, MonsterState::Melee) => (33, 46),
        (MonsterKind::Ogre, MonsterState::MeleeB) => (47, 60),
        (MonsterKind::Ogre, MonsterState::Missile) => (61, 66),
        (MonsterKind::Ogre, MonsterState::PainA) => (67, 71),
        (MonsterKind::Ogre, MonsterState::PainB) => (72, 74),
        (MonsterKind::Ogre, MonsterState::PainC) => (75, 80),
        (MonsterKind::Ogre, MonsterState::PainD) => (81, 96),
        (MonsterKind::Ogre, MonsterState::PainE) => (97, 111),
        (MonsterKind::Ogre, MonsterState::DeathA) => (112, 125),
        (MonsterKind::Ogre, MonsterState::DeathB) => (126, 135),
        // monster_knight.c
        (MonsterKind::Knight, MonsterState::Stand) => (0, 8),
        (MonsterKind::Knight, MonsterState::Walk) => (53, 66),
        (MonsterKind::Knight, MonsterState::Run) => (9, 16),
        (MonsterKind::Knight, MonsterState::MeleeB) => (17, 27),
        (MonsterKind::Knight, MonsterState::Melee) => (43, 52),
        (MonsterKind::Knight, MonsterState::PainA) => (28, 30),
        (MonsterKind::Knight, MonsterState::PainB) => (31, 41),
        (MonsterKind::Knight, MonsterState::DeathA) => (67, 76),
        (MonsterKind::Knight, MonsterState::DeathB) => (77, 87),
        // monster_wizard.c
        (MonsterKind::Wizard, MonsterState::Stand) => (0, 7),
        (MonsterKind::Wizard, MonsterState::Walk) => (0, 7),
        (MonsterKind::Wizard, MonsterState::Run) => (15, 28),
        (MonsterKind::Wizard, MonsterState::Missile) => (29, 38),
        (MonsterKind::Wizard, MonsterState::PainA) => (42, 45),
        (MonsterKind::Wizard, MonsterState::DeathA) => (46, 53),
        // monster_shambler.c
        (MonsterKind::Shambler, MonsterState::Stand) => (0, 16),
        (MonsterKind::Shambler, MonsterState::Walk) => (17, 28),
        (MonsterKind::Shambler, MonsterState::Run) => (29, 34),
        (MonsterKind::Shambler, MonsterState::Melee) => (35, 46),
        (MonsterKind::Shambler, MonsterState::MeleeB) => (56, 64),
        (MonsterKind::Shambler, MonsterState::MeleeC) => (47, 55),
        (MonsterKind::Shambler, MonsterState::Missile) => (65, 76),
        (MonsterKind::Shambler, MonsterState::PainA) => (77, 82),
        (MonsterKind::Shambler, MonsterState::DeathA) => (83, 93),
        // monster_demon1.c
        (MonsterKind::Demon, MonsterState::Stand) => (0, 12),
        (MonsterKind::Demon, MonsterState::Walk) => (13, 20),
        (MonsterKind::Demon, MonsterState::Run) => (21, 26),
        (MonsterKind::Demon, MonsterState::Missile) => (27, 36),
        (MonsterKind::Demon, MonsterState::Melee) => (54, 68),
        (MonsterKind::Demon, MonsterState::PainA) => (39, 44),
        (MonsterKind::Demon, MonsterState::DeathA) => (45, 53),
        // monster_zombie.c
        (MonsterKind::Zombie, MonsterState::Stand) => (0, 7),
        (MonsterKind::Zombie, MonsterState::Walk) => (15, 33),
        (MonsterKind::Zombie, MonsterState::Run) => (34, 51),
        (MonsterKind::Zombie, MonsterState::Missile) => (52, 64),
        (MonsterKind::Zombie, MonsterState::MeleeB) => (65, 78),
        (MonsterKind::Zombie, MonsterState::MeleeC) => (79, 90),
        (MonsterKind::Zombie, MonsterState::PainA) => (91, 102),
        (MonsterKind::Zombie, MonsterState::PainB) => (103, 130),
        (MonsterKind::Zombie, MonsterState::PainC) => (131, 148),
        (MonsterKind::Zombie, MonsterState::PainD) => (149, 161),
        (MonsterKind::Zombie, MonsterState::PainE) => (162, 191),
        (MonsterKind::Zombie, MonsterState::Crucified) => (192, 197),
        // monster_boss.c
        (MonsterKind::Boss, MonsterState::Rise) => (0, 16),
        (MonsterKind::Boss, MonsterState::Stand) => (17, 47),
        (MonsterKind::Boss, MonsterState::DeathA) => (48, 56),
        (MonsterKind::Boss, MonsterState::Missile) => (57, 79),
        (MonsterKind::Boss, MonsterState::PainA) => (80, 89),
        (MonsterKind::Boss, MonsterState::PainB) => (90, 95),
        (MonsterKind::Boss, MonsterState::PainC) => (96, 105),
        (_, MonsterState::Gib) => (0, 0),
        _ => return None,
    };
    Some(MonsterFrameRange { first, last })
}

/// The state a monster occupies before it is woken.
pub const fn initial_state(kind: MonsterKind, crucified: bool) -> MonsterState {
    if crucified {
        return MonsterState::Crucified;
    }
    match kind {
        // Chthon is invisible until his encounter trigger raises him.
        MonsterKind::Boss => MonsterState::Rise,
        _ => MonsterState::Stand,
    }
}

pub const fn first_frame(kind: MonsterKind, state: MonsterState) -> u16 {
    match frame_range(kind, state) {
        Some(range) => range.first,
        None => 0,
    }
}

pub const fn last_frame(kind: MonsterKind, state: MonsterState) -> u16 {
    match frame_range(kind, state) {
        Some(range) => range.last,
        None => 0,
    }
}

/// Highest authored frame index for a monster, so the host can prove every
/// state fits inside the cooked alias model.
pub const fn highest_frame(kind: MonsterKind) -> u16 {
    const STATES: [MonsterState; 17] = [
        MonsterState::Stand,
        MonsterState::Walk,
        MonsterState::Run,
        MonsterState::Missile,
        MonsterState::Melee,
        MonsterState::MeleeB,
        MonsterState::MeleeC,
        MonsterState::PainA,
        MonsterState::PainB,
        MonsterState::PainC,
        MonsterState::PainD,
        MonsterState::PainE,
        MonsterState::DeathA,
        MonsterState::DeathB,
        MonsterState::Gib,
        MonsterState::Rise,
        MonsterState::Crucified,
    ];
    let mut highest = 0u16;
    let mut index = 0usize;
    while index < STATES.len() {
        if let Some(range) = frame_range(kind, STATES[index]) {
            if range.last > highest {
                highest = range.last;
            }
        }
        index += 1;
    }
    highest
}

/// Per-frame move distance for walk, run, and the melee charge tables.
/// `self.velocity = v_forward * n + '0 0 n'` from `dog_leap1` and
/// `demon1_jump1`, units per second.
const fn leap_launch(kind: MonsterKind) -> (i16, i16) {
    match kind {
        MonsterKind::Demon => (600, 250),
        _ => (300, 200),
    }
}

pub fn move_distance(kind: MonsterKind, state: MonsterState, frame: u16) -> i16 {
    let Some(range) = frame_range(kind, state) else {
        return 0;
    };
    let offset = frame.saturating_sub(range.first) as usize;
    let table: &[i16] = match (kind, state) {
        (MonsterKind::Soldier, MonsterState::Run) => &[11, 15, 10, 10, 8, 15, 10, 8],
        (MonsterKind::Soldier, MonsterState::Walk) => &[
            1, 1, 1, 1, 2, 3, 4, 4, 2, 2, 2, 1, 0, 1, 1, 1, 3, 3, 3, 3, 2, 1, 1, 1,
        ],
        (MonsterKind::Dog, MonsterState::Run) => &[16, 32, 32, 20, 64, 32, 16, 32, 32, 20, 64, 32],
        (MonsterKind::Dog, MonsterState::Walk) => &[8, 8, 8, 8, 8, 8, 8, 8],
        (MonsterKind::Dog, MonsterState::Melee) => &[10, 10, 10, 10, 10, 10, 10, 10],
        (MonsterKind::Ogre, MonsterState::Run) => &[9, 12, 8, 22, 16, 4, 13, 24],
        (MonsterKind::Ogre, MonsterState::Walk) => {
            &[3, 2, 2, 2, 2, 5, 3, 2, 3, 1, 2, 3, 3, 3, 3, 4]
        }
        (MonsterKind::Ogre, MonsterState::Melee) => &[11, 1, 4, 13, 9, 0, 0, 0, 0, 0, 0, 3, 8, 9],
        (MonsterKind::Ogre, MonsterState::MeleeB) => &[6, 0, 0, 1, 4, 4, 4, 10, 13, 0, 2, 0, 4, 12],
        (MonsterKind::Knight, MonsterState::Run) => &[16, 20, 13, 7, 16, 20, 14, 6],
        (MonsterKind::Knight, MonsterState::Walk) => &[3, 2, 3, 4, 3, 3, 3, 4, 3, 3, 2, 3, 4, 3],
        (MonsterKind::Knight, MonsterState::Melee) => &[0, 7, 4, 0, 3, 4, 1, 3, 1, 5],
        (MonsterKind::Knight, MonsterState::MeleeB) => &[20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10],
        (MonsterKind::Demon, MonsterState::Run) => &[20, 15, 36, 20, 15, 36],
        (MonsterKind::Demon, MonsterState::Walk) => &[8, 6, 6, 7, 4, 6, 10, 10],
        (MonsterKind::Demon, MonsterState::Melee) => &[4, 0, 0, 1, 2, 1, 6, 8, 4, 2, 0, 5, 8, 4, 4],
        (MonsterKind::Wizard, MonsterState::Run) => &[16; 14],
        (MonsterKind::Wizard, MonsterState::Walk) => &[8; 8],
        (MonsterKind::Shambler, MonsterState::Run) => &[20, 24, 20, 20, 24, 20],
        (MonsterKind::Shambler, MonsterState::Walk) => &[10, 9, 9, 5, 6, 12, 8, 3, 13, 9, 7, 7],
        (MonsterKind::Shambler, MonsterState::Melee) => &[2, 6, 6, 5, 4, 1, 0, 0, 0, 0, 5, 4],
        (MonsterKind::Shambler, MonsterState::MeleeB) => &[5, 3, 7, 3, 7, 9, 5, 4, 8],
        (MonsterKind::Shambler, MonsterState::MeleeC) => &[1, 8, 14, 7, 3, 6, 6, 3, 11],
        (MonsterKind::Zombie, MonsterState::Run) => {
            &[1, 1, 0, 1, 2, 3, 4, 4, 2, 0, 0, 0, 2, 4, 6, 7, 3, 8]
        }
        (MonsterKind::Zombie, MonsterState::Walk) => {
            &[0, 2, 3, 2, 1, 0, 0, 0, 0, 0, 2, 2, 1, 0, 0, 0, 0, 0, 0]
        }
        _ => return 0,
    };
    if offset < table.len() {
        table[offset]
    } else {
        0
    }
}

/// Backward (`ai_pain`) and forward (`ai_painforward`) recoil per pain frame.
fn pain_move(kind: MonsterKind, state: MonsterState, offset: u16) -> i16 {
    let offset = offset as usize;
    let table: &[i16] = match (kind, state) {
        (MonsterKind::Knight, MonsterState::PainB) => &[0, 3, 0, 0, 2, 4, 2, 5, 5, 0, 0],
        (MonsterKind::Ogre, MonsterState::PainD) | (MonsterKind::Ogre, MonsterState::PainE) => {
            &[0, -10, -9, -4]
        }
        (MonsterKind::Zombie, MonsterState::PainA) => &[0, 3, 1, 0, -1, -3, -1],
        (MonsterKind::Zombie, MonsterState::PainB) => &[0, -2, -8, -6, -2],
        (MonsterKind::Zombie, MonsterState::PainC) => &[0, 0, -3, -1, 0, 0, 0, 0, 0, 0, 1, 1],
        (MonsterKind::Zombie, MonsterState::PainD) => &[0, 0, 0, 0, 0, 0, 0, 0, -1],
        (MonsterKind::Zombie, MonsterState::PainE) => &[
            0, -8, -5, -3, -1, -2, -1, -1, -2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 3,
            1, -1,
        ],
        _ => return 0,
    };
    if offset < table.len() {
        table[offset]
    } else {
        0
    }
}

/// Per-frame forward lurch inside a death sequence (`ogre_die_b`).
fn death_move(kind: MonsterKind, state: MonsterState, frame: u16) -> i16 {
    let Some(range) = frame_range(kind, state) else {
        return 0;
    };
    let offset = frame.saturating_sub(range.first) as usize;
    match (kind, state) {
        (MonsterKind::Ogre, MonsterState::DeathB) => {
            const TABLE: [i16; 10] = [0, 5, 0, 1, 3, 7, 25, 0, 0, 0];
            TABLE[offset.min(9)]
        }
        _ => 0,
    }
}

/// Death sequences that voice on a frame rather than on the killing blow.
fn death_sound_frame(kind: MonsterKind, state: MonsterState, frame: u16) -> Option<i16> {
    let range = frame_range(kind, state)?;
    let offset = frame.saturating_sub(range.first);
    match (kind, state, offset) {
        (MonsterKind::Boss, MonsterState::DeathA, 0) => Some(BOSS_DEATH_SOUND),
        (MonsterKind::Boss, MonsterState::DeathA, 7) => Some(BOSS_OUT_SOUND),
        _ => None,
    }
}

/// `DropBackpack` sits on the third frame of every soldier and ogre death
/// sequence (`army_die3`/`army_cdie3`: 5 shells, `ogre_die3`/`ogre_bdie3`:
/// 2 rockets). A gibbed monster never reaches it.
fn backpack_frame(kind: MonsterKind, state: MonsterState, frame: u16) -> Option<BackpackAmmo> {
    let range = frame_range(kind, state)?;
    if frame.saturating_sub(range.first) != 2 {
        return None;
    }
    match kind {
        MonsterKind::Soldier => Some(BackpackAmmo { shells: 5, rockets: 0 }),
        MonsterKind::Ogre => Some(BackpackAmmo { shells: 0, rockets: 2 }),
        _ => None,
    }
}

/// Chthon's shock chain: the first two shocks return to the throw cycle, the
/// last one falls through to the death sequence.
const fn boss_pain_exit(state: MonsterState) -> MonsterState {
    match state {
        MonsterState::PainC => MonsterState::DeathA,
        _ => MonsterState::Missile,
    }
}

/// Attack states that keep tracking the target while they play.
const fn face_during(kind: MonsterKind, state: MonsterState) -> bool {
    match (kind, state) {
        // The demon commits to its leap arc after the wind-up.
        (MonsterKind::Demon, MonsterState::Missile) => false,
        _ => true,
    }
}

const fn idle_chance_q12(kind: MonsterKind) -> u16 {
    match kind {
        MonsterKind::Ogre => Q12_ONE / 10,
        MonsterKind::Wizard => Q12_ONE / 20,
        // The shambler idles on `rand > 0.8`, the same one-in-five chance.
        _ => Q12_ONE / 5,
    }
}

/// `XMUL16(xrand32(), scale)`: scale a Q12 fraction into `0..scale`.
fn scale_random(random: u16, scale: u32) -> i32 {
    ((u32::from(random) * scale) >> 12) as i32
}

pub fn predicted_target(target: Vec3I32, velocity: Vec3I32) -> Vec3I32 {
    Vec3I32 {
        x: target.x.saturating_sub(velocity.x / 5),
        y: target.y.saturating_sub(velocity.y / 5),
        z: target.z.saturating_sub(velocity.z / 5),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERY_KIND: [MonsterKind; 9] = [
        MonsterKind::Soldier,
        MonsterKind::Dog,
        MonsterKind::Ogre,
        MonsterKind::Zombie,
        MonsterKind::Knight,
        MonsterKind::Wizard,
        MonsterKind::Shambler,
        MonsterKind::Demon,
        MonsterKind::Boss,
    ];

    fn visible(distance: i32) -> MonsterThinkInput {
        MonsterThinkInput {
            distance,
            visible: true,
            in_front: true,
            player_alive: true,
            leap_height_ok: true,
            ..MonsterThinkInput::default()
        }
    }

    fn next_think(runtime: &mut MonsterRuntime, input: MonsterThinkInput) -> MonsterAction {
        loop {
            if let Some(action) = runtime.advance_ticks(1, input) {
                return action;
            }
        }
    }

    fn wake(kind: MonsterKind, seed: u16, distance: i32) -> MonsterRuntime {
        let mut runtime = MonsterRuntime::new(kind, seed);
        let action = next_think(&mut runtime, visible(distance));
        assert!(action.activated, "{kind:?} did not acquire the player");
        assert_eq!(action.sound_id, Some(kind.sight_sound()));
        assert_eq!(runtime.state(), MonsterState::Run);
        runtime
    }

    /// Run until `predicate` accepts an action, or panic.
    fn run_until<F>(
        runtime: &mut MonsterRuntime,
        input: MonsterThinkInput,
        limit: usize,
        mut predicate: F,
    ) -> MonsterAction
    where
        F: FnMut(&MonsterRuntime, &MonsterAction) -> bool,
    {
        for _ in 0..limit {
            let action = next_think(runtime, input);
            if predicate(runtime, &action) {
                return action;
            }
        }
        panic!("condition never held for {:?}", runtime.kind());
    }

    #[test]
    fn preserved_frame_tables_cover_every_authored_monster() {
        assert_eq!(
            frame_range(MonsterKind::Soldier, MonsterState::Stand),
            Some(MonsterFrameRange { first: 0, last: 7 })
        );
        assert_eq!(
            frame_range(MonsterKind::Soldier, MonsterState::Missile),
            Some(MonsterFrameRange {
                first: 70,
                last: 78
            })
        );
        assert_eq!(
            frame_range(MonsterKind::Dog, MonsterState::Stand),
            Some(MonsterFrameRange {
                first: 69,
                last: 77
            })
        );
        assert_eq!(
            frame_range(MonsterKind::Ogre, MonsterState::MeleeB),
            Some(MonsterFrameRange {
                first: 47,
                last: 60
            })
        );
        assert_eq!(
            frame_range(MonsterKind::Knight, MonsterState::Melee),
            Some(MonsterFrameRange {
                first: 43,
                last: 52
            })
        );
        assert_eq!(
            frame_range(MonsterKind::Wizard, MonsterState::Missile),
            Some(MonsterFrameRange {
                first: 29,
                last: 38
            })
        );
        assert_eq!(
            frame_range(MonsterKind::Shambler, MonsterState::Missile),
            Some(MonsterFrameRange {
                first: 65,
                last: 76
            })
        );
        assert_eq!(
            frame_range(MonsterKind::Demon, MonsterState::Melee),
            Some(MonsterFrameRange {
                first: 54,
                last: 68
            })
        );
        assert_eq!(
            frame_range(MonsterKind::Zombie, MonsterState::Crucified),
            Some(MonsterFrameRange {
                first: 192,
                last: 197
            })
        );
        assert_eq!(
            frame_range(MonsterKind::Boss, MonsterState::Missile),
            Some(MonsterFrameRange {
                first: 57,
                last: 79
            })
        );
        // Absent states are absent, not silently zero-length.
        assert!(frame_range(MonsterKind::Soldier, MonsterState::Melee).is_none());
        assert!(frame_range(MonsterKind::Wizard, MonsterState::Melee).is_none());
        assert!(frame_range(MonsterKind::Zombie, MonsterState::Melee).is_none());
        assert!(frame_range(MonsterKind::Boss, MonsterState::Run).is_none());
    }

    #[test]
    fn every_state_range_is_ordered_and_bounded() {
        // Highest authored frame per monster. The host builder proves each of
        // these fits inside the real cooked alias model.
        const HIGHEST: [(MonsterKind, u16); 9] = [
            (MonsterKind::Soldier, 102),
            (MonsterKind::Dog, 85),
            (MonsterKind::Ogre, 135),
            (MonsterKind::Zombie, 197),
            (MonsterKind::Knight, 87),
            (MonsterKind::Wizard, 53),
            (MonsterKind::Shambler, 93),
            (MonsterKind::Demon, 68),
            (MonsterKind::Boss, 105),
        ];
        for (kind, highest) in HIGHEST {
            assert_eq!(highest_frame(kind), highest, "{kind:?} highest frame");
        }
        for kind in EVERY_KIND {
            for state in [
                MonsterState::Stand,
                MonsterState::Walk,
                MonsterState::Run,
                MonsterState::Missile,
                MonsterState::Melee,
                MonsterState::MeleeB,
                MonsterState::MeleeC,
                MonsterState::PainA,
                MonsterState::PainB,
                MonsterState::PainC,
                MonsterState::PainD,
                MonsterState::PainE,
                MonsterState::DeathA,
                MonsterState::DeathB,
                MonsterState::Rise,
                MonsterState::Crucified,
            ] {
                if let Some(range) = frame_range(kind, state) {
                    assert!(range.first <= range.last, "{kind:?} {state:?} is inverted");
                }
            }
        }
    }

    #[test]
    fn every_walking_monster_acquires_moves_and_returns_to_run() {
        for kind in EVERY_KIND {
            if kind.is_boss() {
                continue;
            }
            let mut runtime = wake(kind, 11, 300);
            let moved = run_until(&mut runtime, visible(300), 64, |_, action| {
                action.move_units != 0
            });
            assert!(moved.move_units > 0, "{kind:?} run cadence is zero");
            assert!(moved.face_target, "{kind:?} does not track the player");
        }
    }

    #[test]
    fn every_monster_reaches_an_attack_and_returns_to_run() {
        for (kind, distance) in [
            (MonsterKind::Soldier, 300),
            (MonsterKind::Dog, 60),
            (MonsterKind::Ogre, 60),
            (MonsterKind::Zombie, 300),
            (MonsterKind::Knight, 60),
            (MonsterKind::Wizard, 300),
            (MonsterKind::Shambler, 60),
            (MonsterKind::Demon, 60),
        ] {
            let mut runtime = wake(kind, 5, distance);
            let attack = run_until(&mut runtime, visible(distance), 400, |_, action| {
                action.attack.is_some()
            });
            assert!(attack.attack.is_some(), "{kind:?} never attacked");
            run_until(&mut runtime, visible(distance), 200, |runtime, _| {
                runtime.state() == MonsterState::Run
            });
        }
    }

    #[test]
    fn ranged_monsters_emit_their_own_projectile() {
        let mut ogre = wake(MonsterKind::Ogre, 3, 400);
        let grenade = run_until(&mut ogre, visible(400), 400, |_, action| {
            matches!(action.attack, Some(MonsterAttack::Grenade { .. }))
        });
        assert_eq!(grenade.attack, Some(MonsterAttack::Grenade { damage: 40 }));
        assert_eq!(grenade.sound_id, Some(GRENADE_LAUNCH_SOUND));

        let mut wizard = wake(MonsterKind::Wizard, 4, 300);
        let spit = run_until(&mut wizard, visible(300), 400, |_, action| {
            matches!(action.attack, Some(MonsterAttack::Spit { .. }))
        });
        assert_eq!(spit.attack, Some(MonsterAttack::Spit { damage: 9, side: -14 }));
        assert_eq!(spit.sound_id, Some(WIZARD_ATTACK_SOUND));
        // Wiz_StartFast arms two spikes; the second launches later in the
        // same attack, before the wizard returns to run.
        let second = run_until(&mut wizard, visible(300), 16, |runtime, action| {
            assert_eq!(runtime.state(), MonsterState::Missile);
            matches!(action.attack, Some(MonsterAttack::Spit { .. }))
        });
        assert_eq!(second.attack, Some(MonsterAttack::Spit { damage: 9, side: 14 }));

        let mut shambler = wake(MonsterKind::Shambler, 6, 400);
        let bolt = run_until(&mut shambler, visible(400), 400, |_, action| {
            matches!(action.attack, Some(MonsterAttack::Lightning { .. }))
        });
        assert_eq!(bolt.attack, Some(MonsterAttack::Lightning { damage: 10 }));

        let mut zombie = wake(MonsterKind::Zombie, 8, 400);
        let gib = run_until(&mut zombie, visible(400), 400, |_, action| {
            matches!(action.attack, Some(MonsterAttack::ZombieGib { .. }))
        });
        assert!(matches!(
            gib.attack,
            Some(MonsterAttack::ZombieGib { damage: 10, .. })
        ));
        assert_eq!(gib.sound_id, Some(ZOMBIE_THROW_SOUND));
    }

    #[test]
    fn melee_damage_rolls_stay_inside_their_authored_bounds() {
        for (kind, distance, max_damage, reach) in [
            (MonsterKind::Knight, 60, 9, 60),
            (MonsterKind::Ogre, 60, 12, MONSTER_MELEE_RANGE),
            // The smash rolls up to 120, the claws up to 60.
            (MonsterKind::Shambler, 60, 120, MONSTER_MELEE_RANGE),
            (MonsterKind::Demon, 60, 15, MONSTER_MELEE_RANGE),
            (MonsterKind::Dog, 60, 24, MONSTER_MELEE_RANGE),
        ] {
            let mut runtime = wake(kind, 13, distance);
            for _ in 0..12 {
                let action = run_until(&mut runtime, visible(distance), 400, |_, action| {
                    matches!(action.attack, Some(MonsterAttack::Contact { .. }))
                });
                let Some(MonsterAttack::Contact { damage, reach: got }) = action.attack else {
                    unreachable!()
                };
                assert!(
                    (0..=max_damage).contains(&damage),
                    "{kind:?} rolled {damage}"
                );
                assert_eq!(got, reach, "{kind:?} melee reach");
            }
        }
    }

    /// Which melee states a monster enters over many fights from `distance`.
    fn melee_states_seen(kind: MonsterKind, distance: i32) -> [bool; 3] {
        let mut runtime = wake(kind, 17, distance);
        let mut seen = [false; 3];
        for _ in 0..40 {
            run_until(&mut runtime, visible(distance), 400, |runtime, _| {
                runtime.state().is_melee()
            });
            match runtime.state() {
                MonsterState::Melee => seen[0] = true,
                MonsterState::MeleeB => seen[1] = true,
                _ => seen[2] = true,
            }
            run_until(&mut runtime, visible(distance), 200, |runtime, _| {
                runtime.state() == MonsterState::Run
            });
        }
        seen
    }

    #[test]
    fn melee_variants_are_picked_like_the_original() {
        // knight_attack: standing swing inside 80 units, running attack beyond.
        assert_eq!(
            melee_states_seen(MonsterKind::Knight, 60),
            [true, false, false]
        );
        assert_eq!(
            melee_states_seen(MonsterKind::Knight, 90),
            [false, true, false]
        );
        // ogre_melee: coin flip between swing and smash.
        assert_eq!(melee_states_seen(MonsterKind::Ogre, 60), [true, true, false]);
        // sham_melee: smash, right swing, or left swing.
        assert_eq!(
            melee_states_seen(MonsterKind::Shambler, 60),
            [true, true, true]
        );
    }

    #[test]
    fn the_demon_leaps_and_the_dog_bites() {
        let mut demon = wake(MonsterKind::Demon, 2, 160);
        let leap = run_until(&mut demon, visible(160), 400, |_, action| {
            matches!(action.attack, Some(MonsterAttack::Leap { .. }))
        });
        let Some(MonsterAttack::Leap { forward, up }) = leap.attack else {
            unreachable!()
        };
        // demon1_jump4: `self.velocity = v_forward * 600 + '0 0 250'`.
        assert_eq!((forward, up), (600, 250));
        assert!(demon.leaping());
        assert_eq!(demon.advance_leap(1), Some(MonsterLeap { forward: 600, up: 250 - 13 }));

        let mut dog = wake(MonsterKind::Dog, 0, 80);
        let bite = run_until(&mut dog, visible(80), 64, |_, action| {
            matches!(action.attack, Some(MonsterAttack::Contact { .. }))
        });
        assert_eq!(bite.sound_id, Some(DOG_ATTACK_SOUND));
    }

    #[test]
    fn a_leap_flies_a_ballistic_arc() {
        // dog_leap2: `self.velocity = v_forward * 300 + '0 0 200'`.
        let mut dog = wake(MonsterKind::Dog, 0, 120);
        let leap = run_until(&mut dog, visible(120), 400, |_, action| {
            matches!(action.attack, Some(MonsterAttack::Leap { .. }))
        });
        assert_eq!(
            leap.attack,
            Some(MonsterAttack::Leap {
                forward: 300,
                up: 200
            })
        );
        assert!(dog.leaping());
        // The move table and `ai_face` let go of a body the host now flies.
        assert_eq!(leap.move_units, 0);
        assert!(!leap.face_target);

        // Gravity comes off the arc a tick at a time, so the apex is 200
        // units of climb later.
        assert_eq!(
            dog.advance_leap(1),
            Some(MonsterLeap {
                forward: 300,
                up: 187
            })
        );

        // `if (vlen(self.velocity) > 300)`: it bites while the arc still
        // climbs, and nothing at the apex where the speed is exactly 300.
        assert!(matches!(dog.leap_touch_damage(), Some(10..=19)));
        let mut apex = dog;
        while apex.advance_leap(1).is_some_and(|leap| leap.up > 0) {}
        assert_eq!(
            apex.advance_leap(0),
            Some(MonsterLeap {
                forward: 300,
                up: 0
            })
        );
        assert_eq!(apex.leap_touch_damage(), None);

        // `Dog_JumpTouch`: ground under the box ends the leap in the run
        // cycle, and a box hanging over nothing throws itself again.
        let mut hanging = apex;
        hanging.land_leap(false);
        assert!(!hanging.leaping());
        assert_eq!(hanging.state(), MonsterState::Missile);
        apex.land_leap(true);
        assert!(!apex.leaping());
        assert_eq!(apex.state(), MonsterState::Run);
    }

    #[test]
    fn a_forced_jump_uses_authored_velocity_without_changing_ai_state() {
        let mut ogre = wake(MonsterKind::Ogre, 4, 300);
        let state = ogre.state();
        ogre.begin_forced_jump();
        assert!(ogre.forced_jump());
        assert_eq!(
            ogre.advance_leap(1),
            Some(MonsterLeap {
                forward: 200,
                up: 200 - 13,
            })
        );
        ogre.land_leap(true);
        assert!(!ogre.leaping());
        assert!(!ogre.forced_jump());
        assert_eq!(ogre.state(), state);
    }

    #[test]
    fn nightmare_monsters_flinch_once_and_then_stop() {
        // `T_Damage`: `if (skill == 3) self.pain_finished = time + 5`. The
        // knight normally flinches once a second, so a second hit two seconds
        // later lands on skill 3 while the ordinary knight staggers again.
        let elapse = |runtime: &mut MonsterRuntime, ticks: u16| {
            for _ in 0..ticks {
                runtime.advance_ticks(1, visible(200));
            }
        };
        let mut ordinary = MonsterRuntime::new(MonsterKind::Knight, 2);
        assert!(ordinary.take_damage(10, 65, false).sound_id.is_some());
        elapse(&mut ordinary, 2 * ONE_SECOND);
        assert!(ordinary.take_damage(10, 55, false).sound_id.is_some());

        let mut nightmare = MonsterRuntime::new(MonsterKind::Knight, 2);
        assert!(
            nightmare.take_damage(10, 65, true).sound_id.is_some(),
            "the first nightmare hit still flinches, as `th_pain` runs first"
        );
        for _ in 0..4 {
            elapse(&mut nightmare, ONE_SECOND);
            assert_eq!(nightmare.take_damage(10, 55, true).sound_id, None);
        }
        assert!(!nightmare.state().is_pain());
    }

    #[test]
    fn nightmare_monsters_ignore_the_attack_reload() {
        // `SUB_AttackFinished`: `if (skill >= 3) self.attack_finished = time`.
        // A woken soldier normally waits out `SUB_AttackFinished(1)` before
        // its first shot and reloads for one to two seconds after it.
        let mut ordinary = wake(MonsterKind::Soldier, 3, 200);
        let mut attacks = 0;
        for _ in 0..12 {
            if next_think(&mut ordinary, visible(200)).attack.is_some() {
                attacks += 1;
            }
        }

        let mut nightmare = wake(MonsterKind::Soldier, 3, 200);
        let mut nightmare_attacks = 0;
        for _ in 0..12 {
            let input = MonsterThinkInput {
                nightmare: true,
                ..visible(200)
            };
            if next_think(&mut nightmare, input).attack.is_some() {
                nightmare_attacks += 1;
            }
        }
        assert!(
            nightmare_attacks > attacks,
            "nightmare soldier fired {nightmare_attacks} times, easy soldier {attacks}"
        );
    }

    #[test]
    fn pain_thresholds_match_each_monster() {
        // The shambler shrugs off small hits and flinches on a big one.
        let mut shambler = MonsterRuntime::new(MonsterKind::Shambler, 1);
        let mut shrugged = false;
        for _ in 0..24 {
            let transition = shambler.take_damage(1, 599, false);
            if !shambler.state().is_pain() {
                shrugged = true;
                assert_eq!(transition.sound_id, Some(SHAMBLER_PAIN_SOUND));
            }
        }
        assert!(shrugged, "a one-point hit always staggered the shambler");
        let mut shambler = MonsterRuntime::new(MonsterKind::Shambler, 1);
        shambler.take_damage(400, 200, false);
        assert_eq!(shambler.state(), MonsterState::PainA);

        // The knight always flinches, but only once per second.
        let mut knight = MonsterRuntime::new(MonsterKind::Knight, 2);
        let first = knight.take_damage(10, 65, false);
        assert_eq!(first.sound_id, Some(KNIGHT_PAIN_SOUND));
        assert!(knight.state().is_pain());
        let frame = knight.frame();
        let second = knight.take_damage(10, 55, false);
        assert_eq!(second.sound_id, None);
        assert_eq!(knight.frame(), frame);

        // The ogre reaches its long PAIN_D and PAIN_E animations.
        let mut seen_long = false;
        for seed in 0..48u16 {
            let mut ogre = MonsterRuntime::new(MonsterKind::Ogre, seed);
            ogre.take_damage(20, 180, false);
            seen_long |= matches!(ogre.state(), MonsterState::PainD | MonsterState::PainE);
        }
        assert!(seen_long, "the ogre never used its long pain animations");
    }

    #[test]
    fn gib_thresholds_match_each_authored_start_die() {
        for (kind, threshold) in [
            (MonsterKind::Soldier, -35),
            (MonsterKind::Dog, -35),
            (MonsterKind::Knight, -40),
            (MonsterKind::Wizard, -40),
            (MonsterKind::Ogre, -80),
            (MonsterKind::Demon, -80),
            (MonsterKind::Shambler, -60),
        ] {
            assert_eq!(kind.gib_health(), threshold, "{kind:?} gib threshold");
            // Exactly at the threshold the monster still plays its death
            // sequence; one point below it gibs.
            let mut at = MonsterRuntime::new(kind, 3);
            let transition = at.take_damage(200, threshold, false);
            assert!(transition.killed && !transition.gibbed, "{kind:?} at");
            let mut below = MonsterRuntime::new(kind, 3);
            let transition = below.take_damage(200, threshold - 1, false);
            assert!(transition.killed && transition.gibbed, "{kind:?} below");
        }
    }

    #[test]
    fn gibbed_monsters_select_their_original_head_models() {
        assert_eq!(MonsterKind::Soldier.gib_head_model_id(), Some(0x2d));
        assert_eq!(MonsterKind::Dog.gib_head_model_id(), Some(0x2c));
        assert_eq!(MonsterKind::Ogre.gib_head_model_id(), Some(0x31));
        assert_eq!(MonsterKind::Zombie.gib_head_model_id(), Some(0x36));
        assert_eq!(MonsterKind::Knight.gib_head_model_id(), Some(0x2f));
        assert_eq!(MonsterKind::Wizard.gib_head_model_id(), Some(0x35));
        assert_eq!(MonsterKind::Shambler.gib_head_model_id(), Some(0x34));
        assert_eq!(MonsterKind::Demon.gib_head_model_id(), Some(0x2b));
        assert_eq!(MonsterKind::Boss.gib_head_model_id(), None);
    }

    #[test]
    fn death_sequences_finish_and_gib_below_the_threshold() {
        for kind in EVERY_KIND {
            if kind.is_boss() {
                continue;
            }
            let mut runtime = MonsterRuntime::new(kind, 21);
            let transition = runtime.take_damage(500, -1_000, false);
            assert!(transition.killed, "{kind:?} survived a lethal hit");
            assert!(transition.gibbed, "{kind:?} did not gib below threshold");
            let expected = if matches!(kind, MonsterKind::Zombie) {
                ZOMBIE_GIB_SOUND
            } else {
                MONSTER_GIB_SOUND
            };
            assert_eq!(transition.sound_id, Some(expected), "{kind:?} gib voice");
            assert_eq!(runtime.state(), MonsterState::Gib);
            assert!(!runtime.body_solid(), "{kind:?} gibs still block");
        }
        for kind in EVERY_KIND {
            if kind.is_boss() || matches!(kind, MonsterKind::Zombie) {
                continue;
            }
            let mut runtime = MonsterRuntime::new(kind, 21);
            let transition = runtime.take_damage(10, 0, false);
            assert!(transition.killed, "{kind:?} survived a lethal hit");
            assert!(!transition.gibbed, "{kind:?} gibbed at zero health");
            assert_eq!(transition.sound_id, Some(kind.death_sound()));
            assert!(runtime.state().is_death());
            for _ in 0..240 {
                let _ = next_think(&mut runtime, MonsterThinkInput::default());
                if runtime.corpse_finished() {
                    break;
                }
            }
            assert!(runtime.corpse_finished(), "{kind:?} corpse never finished");
            assert!(!runtime.body_solid(), "{kind:?} corpse still blocks");
        }
    }

    #[test]
    fn the_zombie_only_dies_to_a_gib_level_blow() {
        let mut zombie = MonsterRuntime::new(MonsterKind::Zombie, 4);
        // Small hits are ignored outright but still restore health.
        let ignored = zombie.take_damage(4, 56, false);
        assert!(!ignored.killed);
        assert!(ignored.reset_health);
        assert_eq!(zombie.state(), MonsterState::Stand);

        // A shotgun-sized hit knocks it into a fast pain animation.
        let hit = zombie.take_damage(12, 48, false);
        assert!(hit.reset_health);
        assert!(zombie.state().is_pain());
        assert!(!hit.killed);

        // A big hit drops it, and it stops blocking while it is down.
        let mut zombie = MonsterRuntime::new(MonsterKind::Zombie, 4);
        zombie.take_damage(30, 30, false);
        assert_eq!(zombie.state(), MonsterState::PainE);
        let down = first_frame(MonsterKind::Zombie, MonsterState::PainE) + 9;
        let mut saw_down = false;
        for _ in 0..900 {
            let _ = next_think(&mut zombie, visible(200));
            if zombie.frame() >= down && zombie.frame() <= down + 2 {
                saw_down |= !zombie.body_solid();
            }
            if zombie.state() == MonsterState::Run {
                break;
            }
        }
        assert!(saw_down, "the knocked down zombie never went non-solid");
        assert_eq!(zombie.state(), MonsterState::Run, "the zombie never rose");

        // Only a blow that takes it past zero kills it, and that always gibs.
        let mut zombie = MonsterRuntime::new(MonsterKind::Zombie, 4);
        let killed = zombie.take_damage(120, -60, false);
        assert!(killed.killed && killed.gibbed);
        assert_eq!(zombie.state(), MonsterState::Gib);
    }

    #[test]
    fn a_crucified_zombie_hangs_and_never_wakes() {
        let mut zombie = MonsterRuntime::new_crucified(9);
        assert!(zombie.crucified());
        assert!(!zombie.body_solid());
        let range = frame_range(MonsterKind::Zombie, MonsterState::Crucified).unwrap();
        for _ in 0..64 {
            let action = next_think(&mut zombie, visible(64));
            assert!(!action.activated);
            assert!((range.first..=range.last).contains(&action.frame));
        }
        assert!(zombie.crucified());
        // Damage cannot start a fight with decoration.
        let transition = zombie.take_damage(200, -100, false);
        assert!(!transition.killed);
    }

    #[test]
    fn chthon_ignores_damage_and_dies_only_to_the_shock_chain() {
        let mut boss = MonsterRuntime::new(MonsterKind::Boss, 1);
        assert!(!boss.active());
        // Sight never wakes him and weapons never hurt him.
        for _ in 0..32 {
            let action = next_think(&mut boss, visible(200));
            assert!(!action.activated);
        }
        assert!(boss.take_damage(500, -1_000, false).killed == false);
        assert!(!boss.dead());
        assert!(boss.apply_shock().is_none());

        boss.awaken(0);
        assert_eq!(boss.boss_shocks(), BOSS_EASY_SHOCKS);
        assert_eq!(boss.state(), MonsterState::Rise);
        run_until(&mut boss, visible(400), 64, |runtime, _| {
            runtime.state() == MonsterState::Missile
        });
        let lava = run_until(&mut boss, visible(400), 128, |_, action| {
            matches!(action.attack, Some(MonsterAttack::LavaBall { .. }))
        });
        let Some(MonsterAttack::LavaBall { damage, side }) = lava.attack else {
            unreachable!()
        };
        assert!((100..120).contains(&damage), "lava ball rolled {damage}");
        assert_eq!(side, 100);
        assert_eq!(lava.sound_id, Some(BOSS_THROW_SOUND));

        let shock = boss.apply_shock().expect("the first shock lands");
        assert_eq!(shock.sound_id, Some(BOSS_PAIN_SOUND));
        assert_eq!(boss.boss_shocks(), 0);
        assert_eq!(boss.state(), MonsterState::PainC);
        run_until(&mut boss, visible(400), 64, |runtime, _| runtime.dead());
        assert_eq!(boss.state(), MonsterState::DeathA);
        run_until(&mut boss, visible(400), 64, |runtime, _| {
            runtime.corpse_finished()
        });
    }

    #[test]
    fn chthon_on_normal_skill_needs_three_shocks() {
        let mut boss = MonsterRuntime::new(MonsterKind::Boss, 1);
        boss.awaken(1);
        assert_eq!(boss.boss_shocks(), BOSS_HARD_SHOCKS);
        assert_eq!(boss.apply_shock().is_some(), true);
        assert_eq!(boss.state(), MonsterState::PainA);
        assert_eq!(boss.apply_shock().is_some(), true);
        assert_eq!(boss.state(), MonsterState::PainB);
        assert_eq!(boss.apply_shock().is_some(), true);
        assert_eq!(boss.state(), MonsterState::PainC);
        run_until(&mut boss, visible(400), 64, |runtime, _| runtime.dead());
        assert!(boss.apply_shock().is_none());
    }

    #[test]
    fn idle_voices_are_flagged_for_attenuation() {
        let mut zombie = MonsterRuntime::new_crucified(2);
        let mut idle = None;
        for _ in 0..400 {
            let action = next_think(&mut zombie, visible(64));
            if action.sound_id == Some(ZOMBIE_CRUCIFIED_SOUND) {
                idle = Some(action.sound_idle);
                break;
            }
        }
        assert_eq!(idle, Some(true), "the crucified idle voice is ATTN_IDLE");

        let mut ogre = wake(MonsterKind::Ogre, 7, 700);
        let sight = MonsterRuntime::new(MonsterKind::Ogre, 7);
        let _ = sight;
        let mut saw_idle = false;
        for _ in 0..400 {
            let action = next_think(&mut ogre, visible(700));
            if action.sound_id == Some(OGRE_IDLE2_SOUND) {
                saw_idle = action.sound_idle;
                break;
            }
        }
        assert!(saw_idle, "the ogre run idle voice is ATTN_IDLE");
    }

    #[test]
    fn acquisition_requires_visibility_and_front_or_hostility() {
        let mut runtime = MonsterRuntime::new(MonsterKind::Soldier, 0);
        let hidden = MonsterThinkInput {
            distance: 96,
            player_alive: true,
            ..MonsterThinkInput::default()
        };
        assert!(!next_think(&mut runtime, hidden).activated);
        let behind = MonsterThinkInput {
            distance: 200,
            visible: true,
            player_alive: true,
            ..MonsterThinkInput::default()
        };
        assert!(!next_think(&mut runtime, behind).activated);
        // A fired weapon only reaches a monster looking the other way inside
        // RANGE_NEAR; past it the monster must be facing the player.
        let far_hostile = MonsterThinkInput {
            distance: 700,
            player_hostile: true,
            ..behind
        };
        assert!(!next_think(&mut runtime, far_hostile).activated);
        let hostile = MonsterThinkInput {
            player_hostile: true,
            ..behind
        };
        let action = next_think(&mut runtime, hostile);
        assert!(action.activated);
        assert_eq!(action.sound_id, Some(SOLDIER_SIGHT_SOUND));
        assert_eq!(runtime.state(), MonsterState::Run);
    }

    /// `walkmonster_start_go`: a monster with a `path_corner` target walks
    /// toward it; without one it stands forever. Reaching a corner with
    /// `wait` parks it until `pause_time`, then it walks to the next goal.
    #[test]
    fn a_path_corner_goal_makes_a_resting_monster_walk() {
        let goal = Some(Vec3I32 { x: 100 << 12, y: 0, z: 0 });
        let hidden = MonsterThinkInput {
            distance: 2_000,
            player_alive: true,
            ..MonsterThinkInput::default()
        };
        let mut idle = MonsterRuntime::new(MonsterKind::Ogre, 4);
        for _ in 0..8 {
            let action = next_think(&mut idle, hidden);
            assert_eq!(idle.state(), MonsterState::Stand);
            assert!(!action.face_goal && action.move_units == 0);
        }
        for kind in EVERY_KIND {
            if kind.is_boss() {
                continue;
            }
            let mut runtime = MonsterRuntime::new(kind, 4);
            let patrol = MonsterThinkInput { goal, ..hidden };
            let action = run_until(&mut runtime, patrol, 8, |runtime, action| {
                runtime.state() == MonsterState::Walk && action.move_units != 0
            });
            assert!(action.face_goal, "{kind:?} walk does not face its corner");
            assert!(!action.face_target && !action.activated);
            assert!(!runtime.active());
            // A patrolling monster still acquires the player normally.
            let woke = next_think(&mut runtime, MonsterThinkInput { goal, ..visible(300) });
            assert!(woke.activated, "{kind:?} did not wake while patrolling");
            assert_eq!(runtime.state(), MonsterState::Run);
        }
        // Arrival with an authored wait: stand for the wait, then walk on.
        let mut runtime = MonsterRuntime::new(MonsterKind::Soldier, 4);
        let patrol = MonsterThinkInput { goal, ..hidden };
        run_until(&mut runtime, patrol, 8, |runtime, _| {
            runtime.state() == MonsterState::Walk
        });
        runtime.arrive_at_goal(2 * MONSTER_THINK_TICKS);
        assert_eq!(runtime.state(), MonsterState::Stand);
        assert_eq!(next_think(&mut runtime, patrol).move_units, 0);
        assert_eq!(runtime.state(), MonsterState::Stand);
        run_until(&mut runtime, patrol, 4, |runtime, _| {
            runtime.state() == MonsterState::Walk
        });
        // Arrival with no wait keeps walking; losing the goal stops it.
        runtime.arrive_at_goal(0);
        assert_eq!(runtime.state(), MonsterState::Walk);
        next_think(&mut runtime, hidden);
        assert_eq!(runtime.state(), MonsterState::Stand);
        assert_eq!(next_think(&mut runtime, hidden).move_units, 0);
    }

    /// `T_Damage` from another monster: `self.enemy = attacker; FoundTarget`.
    /// The hunt starts from rest without a sight check, and `monster_use`
    /// wakes a targeted monster the same way with the player as enemy.
    #[test]
    fn set_enemy_wakes_from_rest_and_pack_alerts_respect_ambush() {
        let mut ogre = MonsterRuntime::new(MonsterKind::Ogre, 2);
        assert!(ogre.set_enemy(MonsterEnemy::Monster(7)));
        assert!(ogre.active());
        assert_eq!(ogre.enemy(), MonsterEnemy::Monster(7));
        assert_eq!(ogre.state(), MonsterState::Run);
        // Already hunting: a swap is not a second wake.
        assert!(!ogre.set_enemy(MonsterEnemy::Player));
        assert_eq!(ogre.enemy(), MonsterEnemy::Player);
        let action = next_think(&mut ogre, visible(300));
        assert!(action.face_target && !action.activated);

        let hidden = MonsterThinkInput {
            distance: 900,
            player_alive: true,
            ..MonsterThinkInput::default()
        };
        let alerted = MonsterThinkInput {
            pack_alert: true,
            ..hidden
        };
        let mut knight = MonsterRuntime::new(MonsterKind::Knight, 3);
        assert!(!next_think(&mut knight, hidden).activated);
        let action = next_think(&mut knight, alerted);
        assert!(action.activated);
        assert_eq!(action.sound_id, Some(KNIGHT_SIGHT_SOUND));
        assert_eq!(knight.enemy(), MonsterEnemy::Player);
        let mut ambusher = MonsterRuntime::new(MonsterKind::Knight, 3);
        ambusher.set_ambush(true);
        assert!(!next_think(&mut ambusher, alerted).activated);
        assert!(!ambusher.active());
        // Damage still wakes an ambusher, as it always did.
        assert!(ambusher.set_enemy(MonsterEnemy::Monster(1)));
    }

    /// `FindTarget` returns FALSE for `client.items & IT_INVISIBILITY`, so
    /// the Ring of Shadows blocks acquisition only. A monster that already
    /// holds the player as its enemy keeps hunting.
    #[test]
    fn the_ring_of_shadows_blocks_acquisition_but_not_an_existing_hunt() {
        let visible_and_close = MonsterThinkInput {
            distance: 96,
            visible: true,
            in_front: true,
            player_alive: true,
            ..MonsterThinkInput::default()
        };
        let shadowed = MonsterThinkInput {
            player_invisible: true,
            ..visible_and_close
        };

        let mut hidden = MonsterRuntime::new(MonsterKind::Soldier, 0);
        for _ in 0..16 {
            assert!(!next_think(&mut hidden, shadowed).activated);
        }
        assert_eq!(hidden.state(), MonsterState::Stand);

        let mut hunting = MonsterRuntime::new(MonsterKind::Soldier, 0);
        assert!(next_think(&mut hunting, visible_and_close).activated);
        assert_eq!(hunting.state(), MonsterState::Run);
        next_think(&mut hunting, shadowed);
        assert_ne!(hunting.state(), MonsterState::Stand);
    }

    #[test]
    fn soldier_attack_sequence_is_reproducible() {
        fn sequence() -> ([[i16; 2]; 4], u16) {
            let mut runtime = MonsterRuntime::new(MonsterKind::Soldier, 21);
            assert!(next_think(&mut runtime, visible(200)).activated);
            for _ in 0..128 {
                let action = next_think(&mut runtime, visible(200));
                if let Some(MonsterAttack::SoldierShot { spread }) = action.attack {
                    assert_eq!(action.sound_id, Some(SOLDIER_ATTACK_SOUND));
                    return (spread, action.frame);
                }
            }
            panic!("soldier did not fire");
        }
        assert_eq!(sequence(), sequence());
    }

    #[test]
    fn runtime_state_is_small_and_allocation_free() {
        // The host keeps one of these inside every render entity, and PS1
        // RAM is spoken for: 24 bytes is the budget.
        assert!(core::mem::size_of::<MonsterRuntime>() <= 24);
        assert!(core::mem::size_of::<MonsterAction>() <= 40);
    }
}
