# RAM budget

One command prints where main RAM goes and how much of the heap is left:

```sh
cargo quake-build ram              # builds the shipping disc, boots it, prints the report
cargo quake-build ram --map PATH   # the same report from an existing link map (no build)
cargo quake-build ram --map PATH --items   # every placed input section, largest first
```

The first lines are the numbers a change has to defend:

```text
heap_free=29540 bytes (floor 8192)            # ship-boot, after boot and a sampled gameplay period
stack_depth=39824 bytes of 53248 reserved     # the emulator's own high-water over that run
heap region 1131924 B                         # link time: STACK_INIT - STACK_RESERVE - __bss_end
image 846184 B = .text 499596 + .data 46936 + .bss 299652
```

Below them come the same breakdown per crate, per module and per symbol for
`.text`, `.data` (read-only data included), `.bss` and the overlay images.
`ship-boot` still fails the build under its 8,192 byte floor.

## What `heap_free` means

The guest heap is psx-rt's bump allocator: it only moves forward and never
frees. The heap runs from `__heap_start` (the end of `.bss`) to
`STACK_INIT - STACK_RESERVE` (`0x801f2f00`, see `game/build.rs`). Every pool is
allocated once, at its full capacity, in `quake::run` and `ResidentMap::new`;
nothing allocates during play. `heap_free` is therefore

```text
heap region (a link-time number, moves with .text/.data/.bss)
  - heap used (a boot-time number, the sum of the pool sizes below)
```

So a byte of code or static data costs a byte of `heap_free` exactly like a byte
of pool. The census below is the allocator's real sequence for the shipping
disc (cursor trace of 30 allocations, 1,102,384 bytes at the tip).

| allocation | bytes | what |
| --- | ---: | --- |
| resident map arena | 866,128 | largest map (`e1m3`, 866,122) rounded to 8 |
| visible face list | 47,700 | 1,325 faces x 36 (E1M1 PVS census) |
| render entities | 36,192 | 312 x 116 (census 310) |
| collision planes | 35,376 | 2,948 x 12 (census 2,948) |
| stream scratch / overlay window | 30,720 | CD streaming; also the monster/door code window |
| liquid source tiles | 16,384 | 4 x 64x64 |
| menu packets (two frames) | 20,416 | 2 x 232 x 44 |
| HUD packets (two frames) | 8,448 | 2 x 96 x 44 |
| weapon icon strips | 6,144 | inv and inv2 |
| everything else (movers, triggers, audio, centerprint, graphics, ...) | about 35,000 | |

Outside the heap: `.bss` is 270,336 bytes of GPU packet arenas (two 132 KiB
frames), 16,384 of liquid warp output; `.data` holds the 16,384 byte ordering
tables and the 4,692 byte boot logo.

## Ledger (ship-boot `heap_free`, floor 8,192)

| step | commit | free | gain | evidence |
| --- | --- | ---: | ---: | --- |
| checkpoint | 20481e3 | 9,912 | | `ram`, same as the installed build |
| `EntityScene::load` out of line | 664e02e | 12,328 | +2,416 | text 16,952 -> 14,272: inlined into the overlay admission closure it had lost its size optimisation |
| death bubbles in Q16 integers | ce9e35e | 15,720 | +3,392 | the f32 walk linked the soft-float helpers (`__addsf3`, `__mulsf3`, compares, conversions) |
| pools at the cooked maxima | 15cdf03 | 27,444 | +11,724 | `check` census: 310 render entities, 57 movers, 28 triggers, 15 teleports, 4 trains, 15 spouts, 2,948 planes, 81 textures; arena margin 1,878 |
| loading error line without `alloc::format!` | a89f582 | 29,540 | +2,096 | `alloc::fmt::format`, the String writer and `UpperHex` leave the image |

No code path got slower: `e1m1-chain-bench` 31.305 fps before and after, work
instructions 2,102,584,876 -> 2,100,539,490 (-0.10 percent), the hardware frame
dumped at 5 different pad polls is byte-identical to the checkpoint's (3 polls
on the monster route), every gate that passed on the checkpoint still passes,
and the ones that failed there (map, survival, e1m2-e1m3, episode1,
visual-parity, combat) fail with the same message, the same position or the
same hash.

### Pools are pinned by the cooker

The pool constants are mirrored in `host/quake-build/main.rs`
(`GUEST_MAX_*`, `MAX_RENDER_ENTITIES`, `RESIDENT_MAP_ARENA_BYTES`). A recook that
needs one more slot fails `check`/`assets` before a disc exists, so exact sizes
cannot silently drop authored content. `cargo quake-build check` prints the
worst cases.

The map-code overlay executes from the stream scratch byte buffer and needs a
word-aligned address. It used to get one because the pool sizes in front of it
were multiples of four; the buffer is now allocated straight after the arena
and a const assert pins the multiples (`ship-boot` halted on the first
unaligned layout, so a mistake here fails loudly).

## Stack

`STACK_RESERVE` is 0xD000 (53,248). The shipping `quake::run` frame is
31,944 bytes (state, 17 KB, plus inlined-call temporaries; the weapon, spawn and
player paths are inlined into it). Measured below it (emulator
`--stack-profile-log`, telemetry builds of the same code):

| route | depth (entry sp) | run frame | below run |
| --- | ---: | ---: | ---: |
| e1m1-chain-bench | 29,848 | 21,896 | 7,712 |
| e1m1-monster-route-bench | 31,672 | 22,696 | 8,736 |
| monster-regress / bestiary-regress | 32,792 / 32,528 | | |
| ship-boot (Start map) | 39,824 | 31,944 | 7,640 |

`tools/stackan.rs` gives the static upper bound from the disassembled ELF (45,472
bytes at the tip, counting level-load and gameplay chains as if they ran
together; usage in its header). The SDK's `stack-guard` cannot bound `run`,
which calls through a register inside the GPU handoff.

The deepest realistic chain is `run` -> monster admission -> `update_monsters`
-> `monster_move_step` -> `trace_point`: 31,944 + 0xf0 + about 9.5 KB = about
41.7 KB, times 1.25 = 52.1 KB against 53,248. The reserve is already at the
watermark plus a quarter; it cannot give anything back until `run`'s frame
shrinks. Experiments on the frame (all measured with the ELF's `addiu sp`):

- `#[inline(never)]` on 15 weapon/projectile methods: -1,096 B of frame, +4.8 KB
  of text (the outlined bodies lose the size optimisation `run` gives them).
- `#[inline(never)]` on the four large `new()` constructors: no change.
- `load_level` returning into the player in place: no change (exactly 31,944).
- Boxing the five big state objects: -8,528 B of frame but +15.4 KB of heap.
  The frame is 15 KB of state plus 17 KB of temporaries of the inlined update
  paths; moving state out of the frame only moves bytes.

Another way down is splitting the per-frame body of `run` into functions whose
frames overlap, which is a refactor of the loop and not done here.

`ship-boot` now fails if the stack it measures exceeds four fifths of the
reserve.

## What was tried and not taken

- **GPU packet arenas** (270,336 B in `.bss`): the map route reaches 31,453 of
  33,792 words per frame (`map-regress`), and past
  `COARSE_BUDGET_REMAINING_WORDS` the world switches to coarser tessellation,
  so a smaller arena trades visible quality for bytes. Left alone.
- **PGO**: no profile helps (see the PGO section).
- **UI packet vectors** (menu 20,416, HUD 8,448): already trimmed by hand to
  measured maxima with 4 spare packets; sharing them with the GPU arena would
  need a lifetime change in the renderer.
- **Boot logo** (4,692 B of `.data`, shown once): moving it to a CD chunk
  reorders the first disc reads that `Music::probe` keeps seek-free.
- **64-bit divides** (`u64_div_rem` and two wrappers, 1.4 KB) come from
  `audio::spatial_volumes`, `apply_target_actions` and `load_level`; a 32-bit
  rewrite of the pan math is exact but touches three callers.

## PGO

Quake ships no sample profile. The SDK pipeline (`psoxide-pgo`: twin ELF with
`-Cdebuginfo=1 -Zdebug-info-for-profiling`, `--pc-sample-log` replay, convert,
`-Zprofile-sample-use`) was run by hand through `QUAKE_PSX_EXTRA_RUSTFLAGS`
against the tip, with the control built under the same flags (the profiling
flags alone grow the monster overlay package by 564 bytes, past its 15,360 byte
half, so every run below used a 33,280 byte window; that change is not in the
branch). Chain bench = `e1m1-chain-bench`; text is the shipping compile.

| profile | text | vs control | chain fps | chain work |
| --- | ---: | ---: | ---: | ---: |
| none (control) | 504,180 | | 31.252 | 2,100,512,217 |
| E1M1 monster route, default | 498,932 | -5,248 | not built | the overlay package grows to 19.9 KB and no longer fits its window |
| same, `-profile-sample-accurate` | 441,012 | -63,168 | 28.865 | +5.8% |
| same, accurate + hot-callsite 500 | 441,012 | -63,168 | 28.757 | +6.1% |
| 4 routes merged (monster route, chain, arsenal, combat), accurate | 522,108 | +17,928 | 28.194 | +9.4% |
| the same pruned to 99% of gameplay samples (102 functions), accurate | 504,632 | +452 | 28.289 | +20.7% |

No variant is better than the control on both axes. The single-route profile
shows how much code the route never runs (63 KB, 12.5 percent of `.text`), but
the code the route does not run is not cold in the game: the chain route
reaches code the monster route did not. Widening the training set made the
image bigger (profile-guided inlining of the hot callees) and slower, and
pruning the merged profile made the work worse. This matches the SDK's own note
that PGO is a per-game `choose` decision with `off` always a candidate. Not
adopted. If it is revisited, it needs the SDK's `choose` ranking on several
tapes, a builder step that builds the twin and rebinds a committed portable
profile, and a way to keep the overlay images out of the profile.

## Integration (fix-b and fix-c merged)

`integ/quake-2026-10-16` merges the weapons and monsters pass (without the
Chthon lead-aim commit, which kills the scripted player in E1M7) and the
survival, view and message rows (without the player motor rewrite, which
stalled on the E1M1 ramp; the corpse runs the old motor through
`MovementState::die`/`update_dead` and a monster-blind `trace_line`).

| step | ship-boot free |
| --- | ---: |
| RAM branch tip | 29,540 |
| + fix-b | 17,428 |
| + fix-c survival rows | 14,148 |
| + 64-bit divides out of the guest (below) | 13,124 |

The two 64-bit divides in game code are gone: the lightning beam shortening
(`electrode::shorten_q12`) and the sound pan (`spatial::pan_q12`) use the SDK's
64 by 32 divide, each tested against the wide expression. One `__udivdi3` call
remains in `load_level`; it comes from pinned engine code (psx-bsp), so
`u64_div_rem` (1,276 bytes) stays until the engine moves.

Manual save/load (`wip/quake-save-load-2026-10-15`, overlay packages 6 to 8)
costs 13,424 bytes of resident heap region in its current shape (`.text` +12.5 KB,
`.data` +0.9 KB), measured by linking it against its own base. About 6.9 KB of that is
`psx_mc::with_card` instantiated in resident code instead of the card image
(its closures were not matched by the `quake_card` section patterns), and
`overlay-compose` stops at `undeclared typed cold call`. With `with_card` in the
card image the resident cost is about 6.5 KB; against 13,124 free that leaves
about 6.6 KB, which is 1.6 KB short of the 8,192 floor, so it also needs either
that much more code size or the floor lowered (below).

Policy levers that need a decision, not code:

- the `ship-boot` floor (8,192, `SHIP_BOOT_MIN_HEAP_FREE`): nothing allocates
  after boot, so the floor protects against a future allocation, not a measured
  need; 4,096 would be +4,096 bytes.
