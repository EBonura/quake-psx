# Reclaiming the chain-bench cost of fix-b and fix-c (work in progress)

Goal set for `integ/quake-2026-10-16`: `e1m1-chain-bench` work instructions at
or under the checkpoint's 2,102,584,876 and fps at or over 31.305, with the same
final display hash (`0x0ef4917b9161ac78`). Deferred to a later performance pass.
This branch holds the one uncommitted candidate and the measurements so far.
Nothing here is gated or installed; the installed build is `integ` at `e05f786`.

## What is on this branch (untested on the bench)

- `MovementState::update_ticks_with_gravity` and `EntityScene::collision`
  `#[inline(always)]` (variant j1 below).
- A water-probe cache in `MovementState`: `refresh_water` walks the BSP three
  times (feet, middle, eye), and `water_origin` records the origin it last
  resolved with every sample answered, so a player standing still skips the
  walks. Contents are fixed for a map and a new `Player` is built per level, so
  the answer cannot change at the same origin. quake-core tests pass (391 lib),
  the guest has not been built with it and no bench has run.

## Measurements (`e1m1-chain-bench`, work = builder's `full_level_work_instructions`)

| variant | change | work instructions | fps | final display hash | notes |
| --- | --- | ---: | ---: | --- | --- |
| checkpoint 20481e3 | | 2,102,584,876 | 31.305 | 0x0ef4917b9161ac78 | reference; 955 selection rebuilds |
| L0 tip 89f875e | RAM work only | 2,100,539,490 | 31.305 | | |
| m1 (f551f19) | L0 + fix-b | 2,099,885,545 | 31.221 | 0x7f6a1745... | 31.178 on the camera-log run |
| j0 (integ e05f786) | + fix-c, divides out | 2,110,123,754 | 31.105 | 0x0ef4917b9161ac78 | 31.126 on the camera-log run; the installed build |
| j1 | j0 + `inline(always)` on `update_ticks_with_gravity`, `collision` | 2,108,633,479 | 31.105 | 0x7f6a1745... | -1.5M; `update_one` and `NearCandidates::gather` are then the outlined ones |
| h1 | j0 without the one-unit spawn lift | 2,108,013,564 | 31.136 | 0x0ef4917b9161ac78 | -2.1M |
| h2 | j0 without stair smoothing | 2,109,991,804 | 31.168 | 0x0ef4917b9161ac78 | -0.1M |

Monster route: checkpoint 28.909 fps, integ 28.641. Not comparable: with fix-b
the monsters fight differently and the scripted player dies at another place, so
the frozen end-of-route camera and the work differ. A fair comparison needs a
route that survives.

Heap (ship-boot free): integ 13,124 B (floor 8,192). j1 adds 376 B of text in
the bench build; the water cache adds one `Option<Vec3I32>` (16 B) to
`MovementState`. Neither was built as a shipping image.

## Where the 0.6% goes

Instruction counts per function (builder work counts `wait_arena_free` and the
other GPU waits, which move with frame timing; subtracting them, CPU work is
checkpoint 2,051.7M, m1 2,055.0M, j0 2,059.8M, +0.40%):

- `quake::run` -9.9M, `update_ticks_with_gravity` +6.8M and `EntityScene::collision`
  +4.9M: the player update used to be inlined into `run`; it is now two outlined
  functions (net about +1.8M). With `inline(always)` the same weight moves one
  level down (`update_one` +6.8M, `NearCandidates::gather` +4.7M).
- `select_frame_faces_blocked_in_place` +4.4M: it is called 1,132 times against
  955. The selection cache needs an identical camera. A per-frame camera log on
  m1 and j0 shows the same 1,340 frames with a moved origin, and 87 extra frames
  where only the angles change: fix-c gives world damage a direction, so the
  damage kick (`ViewFeel::damage`) now moves pitch and roll where it used to only
  arm a timer. That is Quake-correct behaviour, not code cost. fix-b alone has
  none of it. The spawn lift (h1) and stair smoothing (h2) are not the cause.
- the rest is spread: `memcpy` +0.7M, `push_text` +0.7M, `aabb_overlaps` +0.6M,
  `trace_in_place` +0.5M, `push_centerprint_text` +0.3M.

fps moves with the sum: the bench is GPU and vsync bound on most frames, and a
0.4% CPU increase shifts a few frames over a field boundary.

## Best lead

The player is stationary for most of the route (the camera is unchanged on about
62% of frames), so per-tick work that is a pure function of the origin is wasted.
`refresh_water` is 24.7M instructions (1%) in every build, and the water-probe
cache on this branch should remove most of it, more than the 8M to recover. Build
it, run the chain bench and the gates (nothing about the answer changes, so the
display hash and the monster/combat/bestiary hashes must stay put). Next:

1. `inline(always)` on `update_one` and `NearCandidates::gather` as well (about
   -1.5M more, weigh the code size against the 13 KB of heap).
2. `next_pickup_hull_touch` (37.6M, 1.5%) and `animate_lights` (15.5M) are also
   per-tick work on a stationary player; the same trick may apply.
3. `sound_hash_extend` (12.5M at load, size-optimised, 64-bit FNV per byte)
   counts in the work number; an unrolled speed-optimised loop saves a few
   million and shortens loads.

Method notes: snapshots with their own builder (git archive of a commit, cooked
data copied in); first action of a snapshot cooks, so start the second stream a
minute later; `tools/pcdiff.py`-style per-function diff of the `--pc-line-log`
histograms against the checkpoint's map and log is what found the movers above.
