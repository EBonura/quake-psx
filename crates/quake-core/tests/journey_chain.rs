//! The per-map journeys form one chain.
//!
//! Each per-map journey (`tests/e1mN.toml`) starts from a memory card the game
//! itself saved at that map's spawn while the previous journey's route was
//! replayed. The journeys are replayed one by one, so nothing in the gate ties
//! one journey's start to the last one's finish: a card saved from a different
//! timeline (more health, fewer rockets, a weapon missed) would still pass every
//! journey on its own. This test closes that gap. It reads every start card with
//! the game's own save layout (`game/src/savegame.rs`: header, player, weapon
//! state) through quake-core's save reader, and compares what the player carries
//! with the previous journey's closing checkpoint, which the gate pins from the
//! guest's journey state block on every replay.

use quake_core::combat::{AmmoKind, WeaponState};
use quake_core::movement::MovementState;
use quake_core::save::{Reader, Snapshot};
use psx_mc::{Card, RamCard};

/// The game's slot 1 file (`savegame.rs` `FILES[0]`); the journeys save and load slot 1.
const SLOT_1: &str = "BESLES-00000QPSX0001";
/// `savegame.rs` `STAGING_BYTES`: no save is larger.
const STAGING_BYTES: usize = 6 * 1024;

/// One link: the journey whose start card is checked, and the journey and closing
/// checkpoint its card must agree with.
struct Link {
    journey: &'static str,
    previous: &'static str,
    checkpoint: &'static str,
}

const LINKS: &[Link] = &[
    Link { journey: "e1m1", previous: "e1-chain", checkpoint: "e1m1-loaded" },
    Link { journey: "e1m2", previous: "e1m1", checkpoint: "e1m2-loaded" },
    Link { journey: "e1m3", previous: "e1m2", checkpoint: "e1m3-loaded" },
    Link { journey: "e1m4", previous: "e1m3", checkpoint: "e1m4-loaded" },
    Link { journey: "e1m5", previous: "e1m4", checkpoint: "e1m5-loaded" },
    Link { journey: "e1m6", previous: "e1m5", checkpoint: "e1m6-loaded" },
    Link { journey: "e1m7", previous: "e1m6", checkpoint: "e1m7-loaded" },
];

/// What a save says the player carries, decoded the way `savegame::restore` reads it.
#[derive(Debug)]
struct Carried {
    map: u32,
    skill: u32,
    health: i32,
    armor: u32,
    ammo: [u32; 4],
    weapons: u32,
    keys: u32,
}

fn repo_path(relative: &str) -> String {
    format!("{}/../../{relative}", env!("CARGO_MANIFEST_DIR"))
}

/// Slot 1's payload from a 128 KiB card image.
fn slot_payload(image: &[u8]) -> Vec<u8> {
    let device = RamCard::from_image(image).expect("a 128 KiB card image");
    let mut card = Card::new(device);
    let mut staging = vec![0u8; STAGING_BYTES];
    let len = card.read(SLOT_1, &mut staging).expect("slot 1 holds a save");
    staging.truncate(len);
    staging
}

/// `savegame::encode`'s layout: map, skill, runes, level ticks, then
/// `Player::save_state` (movement, dead flag, three view angles), then the weapon
/// state, whose inventory is what the player carries. The scene that follows is not
/// needed here.
fn decode(payload: &[u8]) -> Carried {
    let mut r = Reader::open(payload).expect("the save's magic, version and checksum");
    let map = u32::from(r.u8());
    let skill = u32::from(r.u8());
    let _runes = r.u8();
    let _ticks = r.u32();
    let _movement = <MovementState as Snapshot>::load(&mut r);
    let _dead = r.bool();
    for _ in 0..3 {
        let _angle = r.i16();
    }
    let weapon = <WeaponState as Snapshot>::load(&mut r);
    assert!(!r.is_bad(), "the save's player and weapon records decode");
    let inventory = weapon.inventory();
    Carried {
        map,
        skill,
        health: i32::from(inventory.health()),
        armor: u32::from(inventory.armor()),
        ammo: [
            u32::from(inventory.ammo(AmmoKind::Shells)),
            u32::from(inventory.ammo(AmmoKind::Nails)),
            u32::from(inventory.ammo(AmmoKind::Rockets)),
            u32::from(inventory.ammo(AmmoKind::Cells)),
        ],
        weapons: u32::from(inventory.owned_weapons()),
        keys: u32::from(inventory.keys()),
    }
}

/// The values a journey's checkpoint asserts on the journey state block, by note.
fn checkpoint_values(journey: &str, checkpoint: &str) -> Vec<(String, u32)> {
    let path = repo_path(&format!("tests/{journey}.toml"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let start = text
        .find(&format!("checkpoint = \"{checkpoint}\""))
        .unwrap_or_else(|| panic!("{path} has no checkpoint {checkpoint}"));
    let mut values = Vec::new();
    let mut value: Option<u32> = None;
    for line in text[start..].lines().skip(1) {
        let line = line.trim();
        if line == "[[step]]" {
            break;
        }
        if let Some(v) = line.strip_prefix("value = ") {
            value = Some(v.parse().unwrap_or_else(|_| panic!("{path}: value {v}")));
        } else if let Some(note) = line.strip_prefix("note = ") {
            let note = note.trim_matches('"').to_string();
            values.push((note, value.take().expect("a value before each note")));
        }
    }
    values
}

fn value_of(values: &[(String, u32)], note: &str) -> u32 {
    values
        .iter()
        .find(|(n, _)| n == note)
        .map(|(_, v)| *v)
        .unwrap_or_else(|| panic!("the checkpoint asserts no {note}"))
}

#[test]
fn every_start_card_carries_what_the_previous_journey_closed_with() {
    let mut broken = Vec::new();
    for link in LINKS {
        let card_path = repo_path(&format!("tests/journeys/{}/start.mcd", link.journey));
        let image = std::fs::read(&card_path).unwrap_or_else(|e| panic!("{card_path}: {e}"));
        let carried = decode(&slot_payload(&image));
        let closed = checkpoint_values(link.previous, link.checkpoint);
        let at = |note: &str| value_of(&closed, note);
        let name = format!("{} card vs {} {}", link.journey, link.previous, link.checkpoint);
        let pairs = [
            ("map", carried.map),
            ("skill", carried.skill),
            ("health", carried.health as u32),
            ("armor", carried.armor),
            ("shells", carried.ammo[0]),
            ("nails", carried.ammo[1]),
            ("rockets", carried.ammo[2]),
            ("cells", carried.ammo[3]),
            ("weapons", carried.weapons),
            ("keys", carried.keys),
        ];
        for (note, card) in pairs {
            if card != at(note) {
                broken.push(format!("{name}: {note} {card} on the card, {} at the checkpoint", at(note)));
            }
        }
    }
    assert!(broken.is_empty(), "the chain is broken:\n{}", broken.join("\n"));
}

/// The decoder reads what the encoder wrote: a round trip through quake-core's own
/// writer with the same field order as `savegame::encode`.
#[test]
fn the_decoder_follows_the_game_layout() {
    let weapon = WeaponState::new();
    let movement = MovementState::new(Default::default());
    let mut buf = [0u8; 1024];
    let len = quake_core::save::frame(&mut buf, |w| {
        w.u8(6);
        w.u8(0);
        w.u8(0);
        w.u32(1234);
        movement.save(w);
        w.bool(false);
        for angle in [1i16, 2, 3] {
            w.i16(angle);
        }
        weapon.save(w);
    })
    .expect("fits");
    let carried = decode(&buf[..len]);
    let inventory = weapon.inventory();
    assert_eq!(carried.map, 6);
    assert_eq!(carried.health, i32::from(inventory.health()));
    assert_eq!(carried.weapons, u32::from(inventory.owned_weapons()));
    assert_eq!(carried.ammo[0], u32::from(inventory.ammo(AmmoKind::Shells)));
}

/// Route-authoring aid: `QUAKE_CARD=<card.mcd> cargo test --test journey_chain -- --ignored`
/// prints what a card's slot 1 says the player carries.
#[test]
#[ignore]
fn print_a_card() {
    let path = std::env::var("QUAKE_CARD").expect("QUAKE_CARD=<card.mcd>");
    let image = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    println!("{path}: {:?}", decode(&slot_payload(&image)));
}
