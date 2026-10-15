# Quake Episode 1 normal-play journeys

Evidence is a recorded controller tape played back through the real title,
START and map flow, with live enemies at the default easy skill. No god mode,
noclip, give, warp or debug spawn, and no controller code runs during the
replay: the pad tape is the only input. The probe words are read-only.

See README.md for the method (fixed 2 ticks per frame, authoring and replay images).

## Edges

| Edge | Journey | Result | Replay BIN sha256 | Tape sha256 |
| --- | --- | --- | --- | --- |
| Title, START, E1M1 to the authored exit, E1M2 loaded | e1m1-exit-tape.toml | PASS 6/6, 6702 ticks, run twice with identical state | fbca23947b536b88b358c4ca4255ea16cbea407427b869cbc5d7bc6f6f10feec | 0291e4749cfd24d888a82efb7b2e80d78354e54e61b91aecd6d759651f9ee362 |

Mechanism word 0x7FBF is every E1M1 bit except the doorway crossing, which only
the authoring controller witnesses. Health on arrival is read from the probe
(1..200 asserted), and the card is unchanged.

## Not yet proven: E1M2 onward

`e1-chain-build` links the START, E1M1 and E1M2 controllers so one run carries
the inventory across the E1M1 exit. It needs `QUAKE_PSX_JOURNEY_STACK_RESERVE`
(0x9000 now; the 32.6 KiB stack depth measured at 0xB000 was in E1M1 only, so
re-measure by scanning a RAM dump for the lowest nonzero word below STACK_INIT
before trusting a value). The journey code itself is only about 8 KiB; the
images are over the heap because the telemetry and monster-think code the
journey features switch on is large, so the proper fix is a replay image
without telemetry (needs the journey features restructured so they do not
imply `emulator-telemetry`).

E1M1 replays exactly, then E1M2 fails in the authoring run, about two hours of
controller work in:

* The E1M2 waypoints were authored for a monsterless run. The first ogre is
  500 units from the spawn. A kiting fight policy survives it and takes the
  double shotgun, and `journey_combat::recovery` now swims toward the last dry
  ground and detours to health, armour, shells and weapons.
* The route walker still loses the bridge after the fight: stage 0 times out
  at 2000 frames wandering a dark side corridor, then the player dies. Fixing
  that means real path re-planning from the current position (routesim has a
  breadth-first path search that would have to run in the guest or be baked
  into the route), not another patch.

Suggested instead: record E1M2 onward by hand. Play the replay image
(`e1m1-replay-build` with the chain features) in the GUI emulator with input
recording, transcribe the tape to poll-bound PXITAPE2 with the frontend's
`--input-tape-transcribe`, and add it to the journey after the E1M1 tape.
