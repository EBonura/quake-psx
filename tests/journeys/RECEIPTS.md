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
