# Quake journeys

Normal-play proofs for Episode 1: a recorded pad tape is replayed through the real
title menu, the START hall and each map, with live monsters at Easy skill. No god mode,
noclip, give, warp or debug spawn. The tape is the only input.

## How a tape is made

`cargo quake-build journey-build` builds `build-psoxide-journey/`: the game at the fixed 30 Hz
step (`perf-fixed-ticks-30hz`, so a tape replays on every hardware model) plus the read-only state block;
`journey-build-shipping` builds `build-psoxide-journey-shipping/` with the shipping game's own variable step
(replays exactly only on the hardware model it was recorded on: `real`). The state block is in `game/src/journey.rs` (feature `journey-telemetry`). The
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
   --map build-psoxide-journey/quake-psx.map --out <dir> --mode play --frames 2400 \
   --maps id1psx/maps --routesim <quake-routesim binary>)
```

The bot writes `<dir>/chain.toml` and `<dir>/tapes/seg-NN-to-<map>.pxtape`: one tape per
map segment, cut at the frame that first shows the next map, and an exact checkpoint after
each (map, skill, position, health, armour, ammunition, weapons, keys, kills and secrets).
The chain file finds the state block by name in that build's link map (`symbols`), waits for the
game's first published frame, and plays every tape with `sync = "poll"` and `tape_rebase`, so a
change in boot, splash or load time moves nothing; regenerate the chain when the game's play
changes (a tape is the pad, not the state).

```sh
psoxide-gate run tests/journeys/e1-chain/chain.toml --disc $PWD/build-psoxide-journey/quake-psx.cue
```

The bot's route files are `tools/journey-bot/routes/*.txt` (waypoints in cooked units,
movers named by cooked entity index) and its combat policy is `src/fight.rs`. A replan
shells out to `tools/routesim path` (breadth-first walkable route over the cooked hull).

## Status

See `COVERAGE.md`. Chained so far: Start (menu, Easy hall, E1 slipgate) to E1M1 to the E1M2 load (`--frames 2400` records exactly
that), `e1-chain` replayed on the real and mister hardware targets and `e1-shipping` (record it with
`--journey-name quake-e1-shipping --build-dir build-psoxide-journey-shipping`) on real only. Route legs that only walk are written as
`@go x y` (a dry path planned by `tools/routesim` from wherever the player stands, every
grid node kept); the bot also detours a few steps for health, ammunition and weapons it
needs, as a player does.
