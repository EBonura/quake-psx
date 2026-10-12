# Quake Episode 1 normal-play journeys

Each journey drives the real title, START and the map's own controller input
(`InputFrame`, the same struct the pad produces) with live enemies at the
default easy skill. No god mode, noclip, give, warp or debug spawn: the
controllers read game state to steer and fire, and the guest simulation is the
authority for every mover, trigger, monster and map load. The probe words are
read-only observations.

Build the disc, then run the journey against it with the retained emulator
frontend (CPU renderer; `--disc` takes the absolute path):

    cargo quake-build e1m1-normal-build
    psoxide-gate run tests/journeys/e1m1-exit.toml --disc $PWD/build-psoxide-e1m1-normal-journey/quake-psx.cue --no-hw

The journey `[addr]` table is bound to the build its link map came from;
after any guest change set `E1M1_PROBE` to the address of `quake_psx::e1m1_chain_regression::PROBE` in the new `quake-psx.map`.

## Edges

| Edge | Journey | Result | Disc BIN sha256 | Ticks |
| --- | --- | --- | --- | --- |
| Title, START, E1M1 to the authored exit, E1M2 loaded | e1m1-exit.toml | PASS 8/8 | e7e450cfbbc667e2819412eebf1dd15f5c57541357af7f281f7c714c53eee430 | 6110 |

`receipts/e1m1-exit.trajectory.txt` samples the run (health, position, route
step, mechanism bits). Health reaches 200 from the map's mega health, and the
E1M2 arrival health of 100 is the original `SetChangeParms` cap.

## Not yet proven

E1M2 and later edges have no receipt. `e1-chain-journey` (`cargo quake-build e1-chain-build`)
links the START, E1M1 and E1M2 controllers into one image so a single run
carries the inventory across the E1M1 exit. It currently fails in E1M1:

* The image is 14 KiB larger than the E1M1 image, whose heap has 8 KiB to
  spare, so it panics at boot in the allocator. Building it with
  `QUAKE_PSX_JOURNEY_STACK_RESERVE=0xB000` (measured stack depth 32.6 KiB)
  boots.
* The E1M1 route's survival is not robust: the same controller survives to the
  exit in the E1M1 image and dies in the chain image. The only difference is
  guest code size and frame timing. The route has no health or armour
  pickups and the controller fights from a standstill, so grunt fire during
  the long loop around the bridge room drains it (about 8 health per second
  of exposure).
