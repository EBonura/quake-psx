# Comicon runtime validation

Move the PSoXide pin from `c2c4b90d` to `b1ee0fd5`, bringing in the shared
packet-linking and depth-slot improvements already accepted by PSoXide.
No game content, renderer settings, packet budgets or simulation logic change.

The canonical fixed E1M1 chain passes twice with identical results on each
SDK: 60 waypoints, all mechanisms, and the E1M2 transition. Both produce
1,682 presentations, display `0x621bf7ee03f427a4` and VRAM
`0x6c23b5e6511bc16e`. Owner-camera visual parity also passes.

Full-level bus cycles change from 2,264,959,278 to 2,260,389,289; measured
FPS from 25.136 to 25.187. This is below the documented 0.122-FPS layout
noise band and is not evidence of a significant FPS increase. The shared
OT linker removes five instructions per linked packet; this work sits in
`gpu_end_frame`, excluded by the existing work-instruction summary. The
remaining work count is essentially unchanged (1,788,228,563 versus
1,788,224,481).

Raw baseline/candidate build, replay and PC-attribution evidence is under
`/tmp/astra-perf-20260904/quake-*`. The guest SDK revisions are recorded
separately from the headless frontend binaries. No original-hardware FPS
claim follows from these emulator runs.
