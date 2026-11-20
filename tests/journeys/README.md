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
   --map build-psoxide-journey/quake-psx.map --out <dir> --mode play --frames 7000 \
   --maps id1psx/maps --routesim <quake-routesim binary>)
```

The bot writes `<dir>/chain.toml` (kept as tests/e1-chain.toml (and tests/e1-shipping.toml) (the gate takes repo-relative disc paths only from a journey directly under tests/), tapes under tests/journeys/e1-chain/tapes, disc and map read from build/journey) and `<dir>/tapes/seg-NN-to-<map>.pxtape`: one tape per
map segment, cut at the frame that first shows the next map, and an exact checkpoint after
each (map, skill, position, health, armour, ammunition, weapons, keys, kills and secrets).
The chain file finds the state block by name in that build's link map (`symbols`), waits for the
game's first published frame, and plays every tape with `sync = "poll"` and `tape_rebase`, so a
change in boot, splash or load time moves nothing; regenerate the chain when the game's play
changes (a tape is the pad, not the state).

```sh
psoxide-gate run tests/journeys/e1-chain.toml
```

The bot's route files are `tools/journey-bot/routes/*.txt` (waypoints in cooked units,
movers named by cooked entity index) and its combat policy is `src/fight.rs`. A replan
shells out to `tools/routesim path` (breadth-first walkable route over the cooked hull).

## Status

See `COVERAGE.md`. Chained so far: `e1-chain` (fixed step; `--frames 7000` records it) Start (menu, Easy hall, E1 slipgate) to E1M1 to E1M2 to
the E1M3 load, replayed on the real and mister hardware targets; `e1-shipping` (record it with `--frames 2400
--journey-name quake-e1-shipping --build-dir build-psoxide-journey-shipping`) Start to the E1M2 load, on real only. Route legs that only walk are written as
`@go x y` (a dry path planned by `tools/routesim` from wherever the player stands, every
grid node kept); the bot also detours a few steps for health, ammunition and weapons it
needs, as a player does.

## Per-map journeys (each map from its own save)

`e1m1`, `e1m2` and `e1m3` (`tests/e1mN.toml`) start at the title menu with a memory card the gate boots
from (`card_fixture`, read only, bound to its sha256), press LOAD GAME and slot 1 with the pad, and play the
one map to the next map's load. The card is a save the game wrote itself: the bot plays the chain to the
map's first moments, opens the pause menu, presses SAVE GAME and slot 1 through the real menu code, and
dumps the card. No state is written, no map is forced. A change to the route or the bot for one map cannot
move another map's recording.

```sh
# 1. the card (runs the route to map N; N = 1, 2, 3)
cargo build --release --manifest-path tools/journey-bot/Cargo.toml
quake-journey-bot --disc build-psoxide-journey/quake-psx.cue --map build-psoxide-journey/quake-psx.map \
  --out <dir> --mode play --frames 9000 --maps id1psx/maps --routesim <quake-routesim> \
  --card-at N --card-out <dir>/eNm?-start.mcd
# 2. the journey from the card (seed picks the dodge rhythm; a seed that fails is simply another attempt)
quake-journey-bot ... --card-in <card> --load-map N --single --repo-journey e1mN \
  --journey-name quake-e1mN --seed S
# 3. copy <dir>/chain.toml to tests/e1mN.toml, <dir>/tapes/* to tests/journeys/e1mN/tapes/ and the card to
#    tests/journeys/e1mN/start.mcd, then: psoxide-gate run tests/e1mN.toml
```

A route file may be cut into phases with `@checkpoint` lines (E1M3 has seven). `--cp-dir <dir>` writes a
checkpoint at each one and `--cp <checkpoint.psx> <map>` continues from it by replaying the checkpoint's own
tape from the card to the same publish (not by loading the saved machine state: a machine restored from a
checkpoint was seen to leave the recording by the time the replay reached the next map, see
quake-e1/NOTES.md), then plays on with a new `--seed`. Every checkpoint carries the whole tape, so the
finished recording is one pad tape from the card. A phase that fails is retried from its checkpoint;
only the pad the bot held is recorded.
