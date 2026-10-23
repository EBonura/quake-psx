//! What the runtime does with each class, key and spawnflag, as citations
//! into the source. `verify` checks that every cited symbol still exists, so
//! a rename shows up as a failing citation instead of a stale claim.

use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Handling {
    /// The runtime implements the original behaviour for what E1 authors.
    Full,
    /// Implemented with a known difference (see the differences table).
    Partial,
    /// Present in the data, ignored at runtime.
    Ignored,
    /// Cooker drops it; the runtime never sees it.
    Dropped,
}

impl Handling {
    pub fn label(self) -> &'static str {
        match self {
            Handling::Full => "full",
            Handling::Partial => "partial",
            Handling::Ignored => "IGNORED",
            Handling::Dropped => "dropped by cook",
        }
    }
}

pub struct Citation {
    pub file: &'static str,
    pub symbol: &'static str,
}

pub struct ClassRule {
    pub classes: &'static [&'static str],
    pub handling: Handling,
    pub note: &'static str,
    pub cite: Citation,
}

const fn c(file: &'static str, symbol: &'static str) -> Citation {
    Citation { file, symbol }
}

const MON: &[&str] = &[
    "monster_army",
    "monster_dog",
    "monster_ogre",
    "monster_zombie",
    "monster_knight",
    "monster_wizard",
    "monster_shambler",
    "monster_demon1",
    "monster_boss",
];

pub const CLASS_RULES: &[ClassRule] = &[
    ClassRule { classes: &["worldspawn"], handling: Handling::Partial,
        note: "worldtype and sounds are cooked; worldtype picks key wording and sound variants, sounds (CD track) is not played (XA ambient loops instead); the level title is carried in the runtime",
        cite: c("crates/quake-core/src/level.rs", "LEVEL_TITLES") },
    ClassRule { classes: &["info_player_start", "info_player_start2"], handling: Handling::Full,
        note: "cooked into the reserved player slot; start2 is the return from E1M7",
        cite: c("game/src/player.rs", "info_player_start2") },
    ClassRule { classes: &["info_intermission"], handling: Handling::Full,
        note: "up to four spots kept, camera from mangle",
        cite: c("game/src/entity.rs", "intermission_spots") },
    ClassRule { classes: &["info_player_deathmatch", "info_player_coop", "info_null"], handling: Handling::Dropped,
        note: "no multiplayer; info_null is removed by the original at spawn",
        cite: c("crates/quake-cook/src/entities.rs", "info_player_deathmatch") },
    ClassRule { classes: &["info_teleport_destination", "trigger_teleport"], handling: Handling::Full,
        note: "first matching destination, 27 unit rise, player-only and silent flags",
        cite: c("crates/quake-core/src/teleport.rs", "resolve_destination") },
    ClassRule { classes: &["trigger_changelevel"], handling: Handling::Full,
        note: "map name cooked as a string, no-intermission flag, target fired on touch",
        cite: c("game/src/entity.rs", "fn change_level") },
    ClassRule { classes: &["trigger_once", "trigger_multiple"], handling: Handling::Partial,
        note: "wait, delay, message, killtarget, health (shootable) honoured; sounds 3 silent; facing (angle) not modelled",
        cite: c("crates/quake-core/src/trigger.rs", "struct MultiTrigger") },
    ClassRule { classes: &["trigger_secret"], handling: Handling::Full,
        note: "total_secrets at load, found on touch, secret.wav",
        cite: c("crates/quake-core/src/secrets.rs", "struct SecretCounter") },
    ClassRule { classes: &["trigger_counter"], handling: Handling::Full,
        note: "count (default 2), countdown messages, nomessage flag",
        cite: c("crates/quake-core/src/targets.rs", "CLASS_TRIGGER_COUNTER") },
    ClassRule { classes: &["trigger_relay"], handling: Handling::Full,
        note: "delay and message honoured by the target graph",
        cite: c("crates/quake-core/src/targets.rs", "CLASS_TRIGGER_RELAY") },
    ClassRule { classes: &["trigger_setskill", "trigger_onlyregistered"], handling: Handling::Full,
        note: "Start map skill doors; shareware answer for the registered doors",
        cite: c("game/src/entity.rs", "CLASS_TRIGGER_SETSKILL") },
    ClassRule { classes: &["trigger_monsterjump"], handling: Handling::Full,
        note: "demon and monster jump volumes",
        cite: c("game/src/entity.rs", "CLASS_TRIGGER_MONSTERJUMP") },
    ClassRule { classes: &["func_door"], handling: Handling::Partial,
        note: "speed, wait, lip, dmg, start-open, toggle, key and dont-link flags honoured; move and stop sounds follow the world type instead of the authored sounds",
        cite: c("crates/quake-core/src/mover.rs", "pub fn from_entity") },
    ClassRule { classes: &["func_button"], handling: Handling::Partial,
        note: "speed, wait, lip, health (shootable) honoured; the press sound follows the world type instead of the authored sounds",
        cite: c("crates/quake-core/src/mover.rs", "button_admits_touch") },
    ClassRule { classes: &["func_plat"], handling: Handling::Full,
        note: "speed, height, dmg, low-trigger flag honoured; the sound pair follows the world type; every E1 plat authors the selector its world type plays (checked per map)",
        cite: c("crates/quake-core/src/mover.rs", "PLAT_LOW_TRIGGER") },
    ClassRule { classes: &["func_door_secret"], handling: Handling::Full,
        note: "open-once, first-left/down, shoot flags; sounds 1 selects the medieval set",
        cite: c("crates/quake-core/src/mover.rs", "fn secret_legs") },
    ClassRule { classes: &["func_train", "path_corner"], handling: Handling::Full,
        note: "speed, dmg (default 2), per-corner wait, looping chain",
        cite: c("crates/quake-core/src/train.rs", "pub struct QuakeTrain") },
    ClassRule { classes: &["func_wall", "func_illusionary"], handling: Handling::Full,
        note: "solid and non-solid brush models",
        cite: c("game/src/entity.rs", "fn brush_model_is_visible") },
    ClassRule { classes: &["func_bossgate", "func_episodegate"], handling: Handling::Full,
        note: "rune gating (shareware: boss gate stays shut)",
        cite: c("game/src/entity.rs", "CLASS_FUNC_EPISODEGATE") },
    ClassRule { classes: &["event_lightning"], handling: Handling::Full,
        note: "E1M7 electrode chain through the implicit lightning target",
        cite: c("game/src/entity.rs", "fn use_event_lightning") },
    ClassRule { classes: &["misc_explobox", "misc_explobox2"], handling: Handling::Full,
        note: "20 health barrel, explosion on death",
        cite: c("game/src/entity.rs", "fn explobox_profile") },
    ClassRule { classes: &["misc_fireball"], handling: Handling::Full,
        note: "speed honoured, random launch delay",
        cite: c("game/src/entity.rs", "struct FireballEmitter") },
    ClassRule { classes: &["trap_spikeshooter"], handling: Handling::Full,
        note: "superspike flag; laser flag not authored in E1",
        cite: c("crates/quake-core/src/traps.rs", "spikeshooter_damage") },
    ClassRule { classes: MON, handling: Handling::Full,
        note: "AI, attacks, pain, death, patrol along path_corner, ambush flag, wake on use; crucified zombie is wall decoration",
        cite: c("crates/quake-core/src/monster.rs", "fn from_class_name") },
    ClassRule { classes: &[
        "item_armor1", "item_armor2", "item_armorInv", "item_artifact_envirosuit",
        "item_artifact_invisibility", "item_artifact_invulnerability", "item_artifact_super_damage",
        "item_cells", "item_health", "item_key1", "item_key2", "item_rockets", "item_shells",
        "item_spikes", "weapon_grenadelauncher", "weapon_lightning", "weapon_nailgun",
        "weapon_rocketlauncher", "weapon_supernailgun", "weapon_supershotgun",
    ], handling: Handling::Full,
        note: "amounts, big/rotten/mega flags and ammo grants match the original touch functions",
        cite: c("crates/quake-core/src/combat.rs", "fn pickup_for_entity") },
    ClassRule { classes: &["item_sigil"], handling: Handling::Full,
        note: "rune bits carried across maps, fires its targets",
        cite: c("game/src/entity.rs", "CLASS_ITEM_SIGIL") },
    ClassRule { classes: &["ambient_comp_hum", "ambient_drip", "ambient_drone", "ambient_suck_wind", "ambient_swamp1"], handling: Handling::Full,
        note: "static positional source, original sample and volume",
        cite: c("game/src/audio.rs", "fn ambient_sound") },
    ClassRule { classes: &["ambient_swamp2"], handling: Handling::Partial,
        note: "plays swamp1.wav (swamp2 is not cooked)",
        cite: c("game/src/audio.rs", "fn ambient_sound") },
    ClassRule { classes: &["air_bubbles"], handling: Handling::Ignored,
        note: "cooked as a record but no spawn path: render_spawn has no arm, no other code reads class 0x02",
        cite: c("game/src/entity.rs", "fn render_spawn") },
    ClassRule { classes: &[
        "light", "light_fluoro", "light_fluorospark", "light_flame_large_yellow",
        "light_flame_small_white", "light_flame_small_yellow", "light_torch_small_walltorch",
    ], handling: Handling::Full,
        note: "baked into lightmaps with their light styles; switchable lights keep a style channel; flames and torches draw their model",
        cite: c("crates/quake-core/src/lightstyle.rs", "SPAWNFLAG_LIGHT_START_OFF") },
];

pub struct FlagRule {
    pub classes: &'static [&'static str],
    pub bit: u32,
    pub meaning: &'static str,
    pub handling: Handling,
    pub cite: Citation,
}

pub const FLAG_RULES: &[FlagRule] = &[
    FlagRule {
        classes: &["func_door"],
        bit: 1,
        meaning: "start open",
        handling: Handling::Full,
        cite: c("crates/quake-core/src/door.rs", "DOOR_START_OPEN"),
    },
    FlagRule {
        classes: &["func_door"],
        bit: 4,
        meaning: "don't link",
        handling: Handling::Full,
        cite: c("crates/quake-core/src/door.rs", "DOOR_DONT_LINK"),
    },
    FlagRule {
        classes: &["func_door"],
        bit: 8,
        meaning: "gold key",
        handling: Handling::Full,
        cite: c("crates/quake-core/src/door.rs", "fn door_key_bit"),
    },
    FlagRule {
        classes: &["func_door"],
        bit: 16,
        meaning: "silver key",
        handling: Handling::Full,
        cite: c("crates/quake-core/src/door.rs", "fn door_key_bit"),
    },
    FlagRule {
        classes: &["func_door"],
        bit: 32,
        meaning: "toggle",
        handling: Handling::Full,
        cite: c("crates/quake-core/src/door.rs", "DOOR_TOGGLE"),
    },
    FlagRule {
        classes: &["func_door_secret"],
        bit: 1,
        meaning: "open once",
        handling: Handling::Full,
        cite: c("crates/quake-core/src/door.rs", "SECRET_OPEN_ONCE"),
    },
    FlagRule {
        classes: &["func_door_secret"],
        bit: 2,
        meaning: "first left",
        handling: Handling::Full,
        cite: c("crates/quake-core/src/door.rs", "SECRET_1ST_LEFT"),
    },
    FlagRule {
        classes: &["func_plat"],
        bit: 1,
        meaning: "low trigger",
        handling: Handling::Full,
        cite: c("crates/quake-core/src/mover.rs", "PLAT_LOW_TRIGGER"),
    },
    FlagRule {
        classes: &[
            "item_cells",
            "item_rockets",
            "item_shells",
            "item_spikes",
            "item_health",
        ],
        bit: 1,
        meaning: "big box / rotten health",
        handling: Handling::Full,
        cite: c("crates/quake-core/src/combat.rs", "fn pickup_for_entity"),
    },
    FlagRule {
        classes: &["item_health"],
        bit: 2,
        meaning: "megahealth",
        handling: Handling::Full,
        cite: c("crates/quake-core/src/combat.rs", "fn pickup_for_entity"),
    },
    FlagRule {
        classes: &["item_sigil", "func_episodegate"],
        bit: 15,
        meaning: "episode rune bits",
        handling: Handling::Full,
        cite: c("game/src/entity.rs", "RUNE_MASK"),
    },
    FlagRule {
        classes: &["light"],
        bit: 1,
        meaning: "start off",
        handling: Handling::Full,
        cite: c(
            "crates/quake-core/src/lightstyle.rs",
            "SPAWNFLAG_LIGHT_START_OFF",
        ),
    },
    FlagRule {
        classes: &[
            "monster_army",
            "monster_dog",
            "monster_knight",
            "monster_ogre",
            "monster_shambler",
            "monster_wizard",
            "monster_demon1",
            "monster_zombie",
        ],
        bit: 1,
        meaning: "ambush (zombie: crucified)",
        handling: Handling::Full,
        cite: c("game/src/entity.rs", "SPAWNFLAG_MONSTER_AMBUSH"),
    },
    FlagRule {
        classes: &["trap_spikeshooter"],
        bit: 1,
        meaning: "superspike",
        handling: Handling::Full,
        cite: c("crates/quake-core/src/traps.rs", "SPAWNFLAG_SUPERSPIKE"),
    },
    FlagRule {
        classes: &["trigger_changelevel"],
        bit: 1,
        meaning: "no intermission",
        handling: Handling::Full,
        cite: c(
            "crates/quake-core/src/targets.rs",
            "CHANGELEVEL_NO_INTERMISSION",
        ),
    },
    FlagRule {
        classes: &["trigger_counter"],
        bit: 1,
        meaning: "no message",
        handling: Handling::Full,
        cite: c("crates/quake-core/src/targets.rs", "COUNTER_NO_MESSAGE"),
    },
    FlagRule {
        classes: &["trigger_teleport"],
        bit: 1,
        meaning: "player only",
        handling: Handling::Full,
        cite: c(
            "crates/quake-core/src/teleport.rs",
            "SPAWNFLAG_TELEPORT_PLAYER_ONLY",
        ),
    },
    FlagRule {
        classes: &["trigger_teleport"],
        bit: 2,
        meaning: "silent",
        handling: Handling::Full,
        cite: c(
            "crates/quake-core/src/teleport.rs",
            "SPAWNFLAG_TELEPORT_SILENT",
        ),
    },
];

/// Cooker/runtime treatment of every authored key, independent of class.
pub struct KeyRule {
    pub key: &'static str,
    pub cooked_as: &'static str,
    pub runtime: &'static str,
}

pub const KEY_RULES: &[KeyRule] = &[
    KeyRule {
        key: "classname",
        cooked_as: "class id",
        runtime: "dispatch",
    },
    KeyRule {
        key: "origin",
        cooked_as: "origin (Q12)",
        runtime: "all placed classes",
    },
    KeyRule {
        key: "angle",
        cooked_as: "angles.y (-1 and -2 kept raw)",
        runtime: "facing, mover direction",
    },
    KeyRule {
        key: "angles",
        cooked_as: "angles",
        runtime: "facing",
    },
    KeyRule {
        key: "mangle",
        cooked_as: "angles",
        runtime: "intermission camera",
    },
    KeyRule {
        key: "model",
        cooked_as: "model (negative = brush submodel)",
        runtime: "brush and alias models",
    },
    KeyRule {
        key: "spawnflags",
        cooked_as: "spawn_flags (u16, keeps the skill bits)",
        runtime: "skill filter and per-class flags",
    },
    KeyRule {
        key: "sounds",
        cooked_as: "noise (i8)",
        runtime: "mover/trigger selectors; worldspawn CD track unused",
    },
    KeyRule {
        key: "count",
        cooked_as: "count",
        runtime: "trigger_counter",
    },
    KeyRule {
        key: "lip",
        cooked_as: "count (shared with count)",
        runtime: "door, plat, button travel",
    },
    KeyRule {
        key: "style",
        cooked_as: "count, lights only",
        runtime: "light style channel; other classes drop it (the original ignores it)",
    },
    KeyRule {
        key: "dmg",
        cooked_as: "damage",
        runtime: "door, plat, train crush",
    },
    KeyRule {
        key: "speed",
        cooked_as: "speed",
        runtime: "movers, trains, fireballs",
    },
    KeyRule {
        key: "height",
        cooked_as: "height",
        runtime: "plat travel",
    },
    KeyRule {
        key: "health",
        cooked_as: "health",
        runtime: "shootable buttons and triggers",
    },
    KeyRule {
        key: "wait",
        cooked_as: "wait (Q12 seconds)",
        runtime: "movers, triggers, train corners",
    },
    KeyRule {
        key: "delay",
        cooked_as: "delay (Q12 seconds)",
        runtime: "target graph",
    },
    KeyRule {
        key: "message",
        cooked_as: "string table (worldspawn: dropped)",
        runtime: "centerprints; titles carried in level.rs",
    },
    KeyRule {
        key: "map",
        cooked_as: "string table (upper case)",
        runtime: "changelevel destination",
    },
    KeyRule {
        key: "target",
        cooked_as: "target id",
        runtime: "target graph, patrol, teleport",
    },
    KeyRule {
        key: "killtarget",
        cooked_as: "kill_target id",
        runtime: "target graph",
    },
    KeyRule {
        key: "targetname",
        cooked_as: "target_name id",
        runtime: "target graph",
    },
    KeyRule {
        key: "worldtype",
        cooked_as: "world type (worldspawn)",
        runtime: "key wording, sound variants",
    },
    KeyRule {
        key: "wad",
        cooked_as: "dropped",
        runtime: "textures come from the cooked BSP",
    },
    KeyRule {
        key: "light",
        cooked_as: "dropped",
        runtime: "lightmaps are baked",
    },
];

/// Check every citation against the source tree. Returns the broken ones.
pub fn verify(repo: &Path) -> Vec<String> {
    let mut seen: Vec<(&str, &str)> = Vec::new();
    let mut all: Vec<&Citation> = CLASS_RULES.iter().map(|r| &r.cite).collect();
    all.extend(FLAG_RULES.iter().map(|r| &r.cite));
    let mut broken = Vec::new();
    for cite in all {
        let key = (cite.file, cite.symbol);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        match std::fs::read_to_string(repo.join(cite.file)) {
            Ok(text) if text.contains(cite.symbol) => {}
            Ok(_) => broken.push(format!("{}: `{}` not found", cite.file, cite.symbol)),
            Err(e) => broken.push(format!("{}: {e}", cite.file)),
        }
    }
    broken
}

pub fn citations_checked() -> usize {
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for cite in CLASS_RULES
        .iter()
        .map(|r| &r.cite)
        .chain(FLAG_RULES.iter().map(|r| &r.cite))
    {
        let key = (cite.file, cite.symbol);
        if !seen.contains(&key) {
            seen.push(key);
        }
    }
    seen.len()
}
