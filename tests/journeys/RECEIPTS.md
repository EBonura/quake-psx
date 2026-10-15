# Quake Episode 1 normal-play journeys

Evidence is a recorded controller tape played back through the real title,
START and map flow, with live enemies at the default easy skill. No god mode,
noclip, give, warp or debug spawn, and no controller code runs during the
replay: the pad tape is the only input. The probe words are read-only.

## How a tape is made and why it needs a fixed-cadence image

The game polls the pad once per rendered frame and applies that sample to the
1 to 4 ticks owed since the last frame. A poll-bound tape therefore means "this
input for however many ticks the frame consumed", and replays identically only
where frames consume the same ticks. Measured: a tape recorded on a telemetry
image replayed on the shipping disc (library BIN ba526452) ends in the wrong
place, because shipping frames take 1 to 3 ticks, the authoring image 2 to 3.

So both images advance a fixed two ticks per frame (`perf-fixed-ticks-30hz`):

* authoring image (`cargo quake-build e1m1-normal-build`): journey controllers
  steer, the pad round trip (`journey_pad.rs`) turns each frame into the pad
  sample that produces it and logs it as `PADTAPE` on the debug log. The
  controllers are only a tape authoring tool.
* replay image (`cargo quake-build e1m1-replay-build`): the same code with the
  controllers inert (`journey-replay`). The pad drives the game.

Tape from the log: `--guest-debug-log` output, one `PADTAPE n menu buttons
rx ry lx ly` line per frame; the tape is a poll 0 neutral sample followed by
every frame's sample, as PXITAPE2 (header, count, first poll 0, six bytes per
sample), trailing idle frames trimmed to 120.

Run a journey against its replay image:

    cargo quake-build e1m1-replay-build
    psoxide-gate run tests/journeys/e1m1-exit-tape.toml --disc $PWD/build-psoxide-e1m1-replay/quake-psx.cue --no-hw

The `[addr]` table is bound to the build; after a guest change set
`E1M1_PROBE` to `quake_psx::e1m1_chain_regression::PROBE` in the new
`quake-psx.map`. The images need `QUAKE_PSX_JOURNEY_STACK_RESERVE=0xB000`
(measured stack depth 32.6 KiB of 45 KiB) to fit the extra controller code in
the heap. Shipping never sets it.

## Edges

| Edge | Journey | Result | Replay BIN sha256 | Tape sha256 |
| --- | --- | --- | --- | --- |
| Title, START, E1M1 to the authored exit, E1M2 loaded | e1m1-exit-tape.toml | PASS 6/6, 6702 ticks, run twice with identical state | fbca23947b536b88b358c4ca4255ea16cbea407427b869cbc5d7bc6f6f10feec | 0291e4749cfd24d888a82efb7b2e80d78354e54e61b91aecd6d759651f9ee362 |

Mechanism word 0x7FBF is every E1M1 bit except the doorway crossing, which only
the authoring controller witnesses. Health on arrival is read from the probe
(1..200 asserted), and the card is unchanged.

## Not yet proven: E1M2 onward

`e1-chain-build` links the START, E1M1 and E1M2 controllers so one run carries
the inventory across the E1M1 exit (it needs `QUAKE_PSX_JOURNEY_STACK_RESERVE=0xA000`).
E1M1 replays exactly, then E1M2 fails in the authoring run:

* The E1M2 route was authored for a monsterless run. The first ogre sits 500
  units from the spawn and chainsaws the controller to near death when it
  stands in melee range; a version that kites (back off, circle, fire when
  aligned) survives it and takes the double shotgun, but the run then leaves
  the authored route and drowns in the water room (stage 0 times out at 2000
  frames, the controller does not swim up).
* Whatever the controller does in E1M2 changes the monster timeline, so each
  fix moves where the next failure is. The controllers need a recovery layer
  (swim to the surface, re-plan from the current position, pick up health and
  armour) before a tape for E1M2 can be recorded.
