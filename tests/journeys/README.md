# Quake journeys

Normal-play proofs for Episode 1: a recorded controller tape is played through
the real title, START and map flow, with live enemies at the default easy
skill. No god mode, noclip, give, warp or debug spawn, and no controller code
runs during the replay. `RECEIPTS.md` lists the proven edges and their hashes.

**Proofs run on the shipping game code at a fixed 2 ticks per frame, not on the
shipping BIN.** The reason is below: the game polls the pad per frame, so a
tape only reproduces where frames consume the same ticks, and the shipping
clock makes that vary with frame cost.

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

