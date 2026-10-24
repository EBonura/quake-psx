# Quake journeys

Normal-play proofs for Episode 1: a recorded pad tape is replayed through the real
title menu, the START hall and each map, with live monsters at Easy skill. No god mode,
noclip, give, warp or debug spawn. The tape is the only input.

## How a tape is made

`cargo quake-build journey-build` builds `build-psoxide-journey/`: the shipping game plus
the read-only state block in `game/src/journey.rs` (feature `journey-telemetry`). The
guest publishes the block at the top of every frame and never reads it back, so it cannot
change play. `tools/journey-bot` boots that disc in the emulator, steps it exactly the way
`psoxide-gate` does, and stops each time a new block is published, just before the frame's
pad poll. It reads the block, decides the pad sample (sticks and buttons, the way a player
holds them), and records it. The recorded poll-bound tape replays through the gate to the
same machine state, because the pad is the only input and the emulator is deterministic.

```sh
cargo quake-build journey-build
cargo build --release --manifest-path tools/routesim/Cargo.toml
cargo build --release --manifest-path tools/journey-bot/Cargo.toml
(cd <repo> && quake-journey-bot --disc build-psoxide-journey/quake-psx.cue \
   --map build-psoxide-journey/quake-psx.map --out <dir> --mode play --frames 2300 \
   --maps id1psx/maps --routesim <routesim binary>)
```

The bot writes `<dir>/chain.toml` and `<dir>/tapes/seg-NN-to-<map>.pxtape`: one tape per
map segment, cut at the frame that first shows the next map, and an exact checkpoint after
each (map, skill, position, health, armour, ammunition, weapons, keys, kills and secrets).
The `[addr]` entry names the state block in that build's link map, so regenerate the
journey after any guest change.

```sh
psoxide-gate run tests/journeys/e1-chain/chain.toml --disc $PWD/build-psoxide-journey/quake-psx.cue
```

The bot's route files are `tools/journey-bot/routes/*.txt` (waypoints in cooked units,
movers named by cooked entity index) and its combat policy is `src/fight.rs`. A replan
shells out to `tools/routesim path` (breadth-first walkable route over the cooked hull).

## Status

See `COVERAGE.md`. Chained so far: Start (menu, Easy hall, E1 slipgate) to E1M1 to the
E1M2 load, replayed on CPU and with the hardware renderer comparison.
