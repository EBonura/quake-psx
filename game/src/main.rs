//! Quake for PlayStation 1 on the PSoXide SDK.

#![feature(optimize_attribute)]
#![no_std]
#![no_main]

extern crate alloc;
extern crate psx_rt;

#[cfg(feature = "arsenal-regression")]
mod arsenal_regression;
mod asset;
mod audio;
#[cfg(feature = "bestiary-regression")]
mod bestiary_regression;
mod bonnie;
#[cfg(feature = "combat-regression")]
mod combat_regression;
#[cfg(feature = "e1m1-chain-regression")]
mod e1m1_chain_regression;
#[cfg(feature = "e1m2-e1m3-route-regression")]
mod e1m2_e1m3_route_regression;
mod entity;
#[cfg(feature = "episode1-route-regression")]
mod episode1_regression;
mod input;
mod input_policy;
mod intro;
#[cfg(feature = "monster-regression")]
mod monster_regression;
#[cfg(feature = "monsterjump-regression")]
mod monsterjump_regression;
mod music;
mod platform;
mod player;
mod pusher;
mod quake;
#[cfg(feature = "episode1-regression")]
mod regression;
mod renderer;
#[cfg(feature = "start-route-regression")]
mod start_route_regression;
#[cfg(feature = "survival-regression")]
mod survival_regression;
#[cfg(feature = "systems-regression")]
mod systems_regression;
#[cfg(feature = "visual-parity-regression")]
mod visual_parity_regression;

#[cfg(all(
    feature = "renderer-topology-cache",
    feature = "renderer-indexed-projection"
))]
compile_error!("renderer topology-cache and indexed-projection experiments are mutually exclusive");

#[cfg(all(
    feature = "visual-parity-regression",
    any(
        feature = "episode1-regression",
        feature = "episode1-route-regression",
        feature = "ambient-regression",
        feature = "combat-regression",
        feature = "arsenal-regression",
        feature = "monster-regression",
        feature = "bestiary-regression",
        feature = "start-route-regression",
        feature = "e1m1-chain-regression",
        feature = "e1m2-e1m3-route-regression",
        feature = "systems-regression",
        feature = "survival-regression",
        feature = "hardware-regression"
    )
))]
compile_error!("visual parity cannot be combined with a gameplay regression feature");

#[cfg(all(
    feature = "monsterjump-regression",
    any(
        feature = "episode1-regression",
        feature = "episode1-route-regression",
        feature = "ambient-regression",
        feature = "combat-regression",
        feature = "arsenal-regression",
        feature = "monster-regression",
        feature = "bestiary-regression",
        feature = "start-route-regression",
        feature = "e1m1-chain-regression",
        feature = "e1m2-e1m3-route-regression",
        feature = "systems-regression",
        feature = "survival-regression",
        feature = "visual-parity-regression",
        feature = "hardware-regression"
    )
))]
compile_error!("monster-jump regression cannot be combined with another regression feature");

#[cfg(any(
    all(feature = "combat-regression", feature = "episode1-regression"),
    all(feature = "combat-regression", feature = "arsenal-regression"),
    all(feature = "episode1-regression", feature = "arsenal-regression"),
    all(feature = "monster-regression", feature = "combat-regression"),
    all(feature = "monster-regression", feature = "episode1-regression"),
    all(feature = "monster-regression", feature = "arsenal-regression"),
    all(feature = "start-route-regression", feature = "combat-regression"),
    all(feature = "start-route-regression", feature = "episode1-regression"),
    all(feature = "start-route-regression", feature = "arsenal-regression"),
    all(feature = "start-route-regression", feature = "monster-regression"),
    all(feature = "start-route-regression", feature = "ambient-regression"),
    all(feature = "start-route-regression", feature = "hardware-regression"),
    all(feature = "e1m1-chain-regression", feature = "combat-regression"),
    all(feature = "e1m1-chain-regression", feature = "episode1-regression"),
    all(feature = "e1m1-chain-regression", feature = "arsenal-regression"),
    all(feature = "e1m1-chain-regression", feature = "monster-regression"),
    all(feature = "e1m1-chain-regression", feature = "start-route-regression"),
    all(feature = "e1m1-chain-regression", feature = "ambient-regression"),
    all(feature = "e1m1-chain-regression", feature = "hardware-regression"),
    all(feature = "e1m2-e1m3-route-regression", feature = "combat-regression"),
    all(
        feature = "e1m2-e1m3-route-regression",
        feature = "episode1-regression"
    ),
    all(feature = "e1m2-e1m3-route-regression", feature = "arsenal-regression"),
    all(feature = "e1m2-e1m3-route-regression", feature = "monster-regression"),
    all(
        feature = "e1m2-e1m3-route-regression",
        feature = "start-route-regression"
    ),
    all(
        feature = "e1m2-e1m3-route-regression",
        feature = "e1m1-chain-regression"
    ),
    all(feature = "e1m2-e1m3-route-regression", feature = "ambient-regression"),
    all(
        feature = "e1m2-e1m3-route-regression",
        feature = "hardware-regression"
    ),
    all(feature = "survival-regression", feature = "combat-regression"),
    all(feature = "survival-regression", feature = "episode1-regression"),
    all(feature = "survival-regression", feature = "arsenal-regression"),
    all(feature = "survival-regression", feature = "monster-regression"),
    all(feature = "survival-regression", feature = "start-route-regression"),
    all(feature = "survival-regression", feature = "e1m1-chain-regression"),
    all(feature = "survival-regression", feature = "ambient-regression"),
    all(feature = "survival-regression", feature = "hardware-regression"),
    all(feature = "episode1-route-regression", feature = "combat-regression"),
    all(feature = "episode1-route-regression", feature = "episode1-regression"),
    all(feature = "episode1-route-regression", feature = "arsenal-regression"),
    all(feature = "episode1-route-regression", feature = "monster-regression"),
    all(
        feature = "episode1-route-regression",
        feature = "start-route-regression"
    ),
    all(
        feature = "episode1-route-regression",
        feature = "e1m1-chain-regression"
    ),
    all(feature = "episode1-route-regression", feature = "bestiary-regression"),
    all(feature = "episode1-route-regression", feature = "systems-regression"),
    all(feature = "episode1-route-regression", feature = "survival-regression"),
    all(feature = "episode1-route-regression", feature = "ambient-regression"),
    all(feature = "episode1-route-regression", feature = "hardware-regression")
))]
compile_error!("gameplay regression features are mutually exclusive");

#[no_mangle]
fn main() {
    psx_rt::tty::println("quake-psx: all-Rust PSoXide boot");
    quake::run()
}
