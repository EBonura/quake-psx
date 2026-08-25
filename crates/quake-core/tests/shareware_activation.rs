//! What the cooked shareware episode actually authors for the two activation
//! paths that were dead, measured against the cooked lumps rather than
//! against a list of entity indices that a re-cook could renumber.
//!
//! Two production defects motivated this. `key_touch` ends in
//! `SUB_UseTargets` like every other item touch function, but the pickup loop
//! only did that for `item_sigil`, so four authored key chains never fired. A
//! `func_button` with `health` spawns with `th_die` and no touch function at
//! all, but shootable buttons took no damage and were wrongly openable by
//! walking into them.

use quake_core::mover::{
    button_admits_touch, button_is_shootable, mover_admits_use, ShootableButton,
};
use quake_formats::{LumpKind, MapEntity, PsbIndex, RecordSlice, SliceReader};

const CLASS_FUNC_BUTTON: u8 = 0x0b;
/// `item_artifact_*` through `item_spikes`: every class the pickup loop and
/// `item_sigil` cover.
const PICKUP_CLASSES: core::ops::RangeInclusive<u8> = 0x1d..=0x28;
const CLASS_ITEM_KEY1: u8 = 0x23;
const CLASS_ITEM_KEY2: u8 = 0x24;
const CLASS_ITEM_SIGIL: u8 = 0x27;

const MAPS: [&str; 9] = [
    "start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8",
];

fn map(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/../../id1psx/maps/{name}.psb",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
}

fn entities(bytes: &[u8]) -> RecordSlice<'_, MapEntity> {
    let mut reader = SliceReader::new(bytes);
    let index = PsbIndex::read(&mut reader).expect("psb index");
    let range = index.lump(LumpKind::Entities);
    RecordSlice::<MapEntity>::new(&bytes[range.offset as usize..range.end() as usize])
        .expect("entities")
}

fn each_entity(name: &str, mut visit: impl FnMut(usize, MapEntity)) {
    let bytes = map(name);
    let entities = entities(&bytes);
    for index in 0..entities.len() {
        visit(index, entities.get(index).expect("entity"));
    }
}

/// Identified by authored properties, not by index: a brush-model
/// `func_button` carrying health. Every one of them is health 1, each fires a
/// target, and each is shot rather than touched.
#[test]
fn every_authored_shootable_button_is_shot_open_and_fires_a_target() {
    let mut found: Vec<(&str, usize, i16, u16, u16)> = Vec::new();
    for name in MAPS {
        each_entity(name, |index, entity| {
            if !button_is_shootable(entity.class_name, entity.health) || entity.model >= 0 {
                return;
            }
            found.push((
                name,
                index,
                entity.health,
                entity.target,
                entity.target_name,
            ));
        });
    }

    let maps: Vec<&str> = found.iter().map(|(name, ..)| *name).collect();
    assert_eq!(
        maps,
        vec!["e1m2", "e1m3", "e1m4", "e1m4"],
        "the shareware episode authors exactly four shootable buttons: {found:?}"
    );
    for (name, index, health, target, target_name) in &found {
        assert_eq!(*health, 1, "{name} #{index} is authored health 1");
        assert_ne!(
            *target, 0,
            "{name} #{index} must fire something or shooting it is pointless"
        );
        assert!(
            !button_admits_touch(CLASS_FUNC_BUTTON, *health),
            "{name} #{index} must not open by walking into it"
        );
        // Every one of these is UNNAMED, which is the case that made the
        // USE arm reach them: `target_name == 0` was enough to open any
        // mover directly, so all four opened on USE and never needed to be
        // shot at all.
        assert_eq!(*target_name, 0, "{name} #{index} is authored unnamed");
        assert!(
            !mover_admits_use(CLASS_FUNC_BUTTON, *health, *target_name),
            "{name} #{index} must not open on USE either"
        );
        // One point of any weapon kills an authored health 1 and the button
        // hands its health back, so a re-usable one can be shot again.
        let mut button =
            ShootableButton::from_entity(CLASS_FUNC_BUTTON, *health).expect("shootable");
        assert!(
            button.take_damage(1),
            "{name} #{index} must die to one point"
        );
        assert_eq!(button.health(), *health);
        assert!(!button.is_live());
        button.rearm();
        assert!(button.is_live(), "{name} #{index} recovers on its return");
    }
}

/// The complement: every other authored `func_button` is a touch/use button
/// and takes no damage. E1M1's are the ones the survival and chain routes
/// drive with ordinary input, including the lift button.
#[test]
fn every_other_authored_button_stays_a_touch_button() {
    let mut touch_buttons = 0usize;
    let mut e1m1_lift_button = None;
    for name in MAPS {
        each_entity(name, |index, entity| {
            if entity.class_name != CLASS_FUNC_BUTTON || entity.model >= 0 {
                return;
            }
            if button_is_shootable(entity.class_name, entity.health) {
                return;
            }
            assert!(
                button_admits_touch(entity.class_name, entity.health),
                "{name} #{index} is neither shootable nor touchable"
            );
            assert!(
                ShootableButton::from_entity(entity.class_name, entity.health).is_none(),
                "{name} #{index} must take no damage"
            );
            touch_buttons += 1;
            // E1M1's ordinary buttons, the ones the chain and survival routes
            // drive with touch and use. A brush entity keeps its geometry in
            // its model rather than its origin, so it is named here by what
            // it does, not by where it is; the lift's actual descent is
            // proved on the guest by survival-regress, once per map load.
            if name == "e1m1" && entity.target != 0 {
                e1m1_lift_button = Some((index, entity.target));
            }
        });
    }
    assert!(
        touch_buttons >= 8,
        "the episode authors plenty of ordinary buttons, found {touch_buttons}"
    );
    let (index, target) = e1m1_lift_button.expect("E1M1 authors touch buttons that fire");
    assert_ne!(target, 0, "E1M1 #{index} fires its target");
}

/// Exactly which pickups author a chain. The pickup loop fires targets for
/// any consumed pickup that authors a target or a killtarget, which is the
/// original's own rule; on this data that is the keys and E1M7's sigil, and
/// nothing at all on Start or E1M1.
#[test]
fn only_the_keys_and_the_sigil_author_a_pickup_chain() {
    let mut chains: Vec<(&str, u8, u16, u16)> = Vec::new();
    for name in MAPS {
        each_entity(name, |_, entity| {
            let is_pickup = PICKUP_CLASSES.contains(&entity.class_name);
            if !is_pickup || (entity.target == 0 && entity.kill_target == 0) {
                return;
            }
            chains.push((name, entity.class_name, entity.target, entity.kill_target));
        });
    }

    let summary: Vec<(&str, u8)> = chains
        .iter()
        .map(|(name, class, ..)| (*name, *class))
        .collect();
    assert_eq!(
        summary,
        vec![
            ("e1m2", CLASS_ITEM_KEY1),
            ("e1m6", CLASS_ITEM_KEY2),
            ("e1m6", CLASS_ITEM_KEY1),
            ("e1m7", CLASS_ITEM_SIGIL),
            ("e1m8", CLASS_ITEM_KEY1),
        ],
        "the authored pickup chains changed: {chains:?}"
    );
    for (name, class, target, kill_target) in &chains {
        assert!(
            *target != 0 || *kill_target != 0,
            "{name} class {class:#04x} must carry something to fire"
        );
    }
}

/// Start and E1M1 contain no pickup target chains.
#[test]
fn start_and_e1m1_pickups_fire_nothing() {
    for name in ["start", "e1m1"] {
        each_entity(name, |index, entity| {
            if !PICKUP_CLASSES.contains(&entity.class_name) {
                return;
            }
            assert_eq!(
                (entity.target, entity.kill_target),
                (0, 0),
                "{name} #{index} would newly fire a chain"
            );
        });
    }
}
