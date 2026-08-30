# Rendering

The Quake renderer targets the original PlayStation at 320x240. It keeps
Quake's BSP visibility model and adapts the visible surfaces to PSoXide's GTE,
GPU packet and ordering-table APIs.

## Frame path

A normal frame follows this path:

1. locate the camera leaf in the cooked BSP;
2. read the leaf's potentially visible set;
3. collect visible world and brush-model faces;
4. transform, clip and project vertices with the GTE;
5. submit textured polygons to a depth-sorted ordering table;
6. draw alias models, sprites, particles, the weapon and HUD;
7. submit the finished list while the next frame is prepared.

World records are converted by the cooker so the PS1 does not parse desktop
Quake structures at runtime. Frequently used face, vertex, texture and
collision records are stored in compact arrays.

## PlayStation-specific choices

### Texture palettes

Quake textures are cooked into PS1 texture pages and palettes. The game uses a
shared gamma-adjusted palette so dark areas remain readable on typical
PlayStation output. Texture conversion happens on the host; the console does
not perform per-pixel colour conversion.

### Affine subdivision

The PlayStation GPU uses affine texture mapping. Large polygons are subdivided
only when their projected size and texture error make the distortion obvious.
Global fine subdivision was rejected because it increased packet count and GPU
work throughout every map.

### Sky

Quake sky textures contain an opaque background and a masked foreground.
The cooker separates those layers. The renderer projects both as a moving
view-ray background with different scroll rates, preserving the characteristic
Quake sky without drawing distant world geometry.

### Water

Water surfaces perturb their texture coordinates over time. Water warp affects
the camera while the viewpoint is submerged. An optional translucent-water
mode uses the PS1's semi-transparent blend modes and a limited view through the
first water boundary.

### Sprites and models

Alias models use cooked frames and texture atlases. Sprites support the
original parallel, facing-upright, parallel-upright, oriented and
parallel-oriented modes. The first-person weapon is submitted after the world
so its depth ties do not expose gaps in nearby geometry.

## Performance work

The current renderer includes:

- BSP potentially-visible-set culling;
- cached visible-face order for stable leaves;
- compact cooked surface records;
- indexed world vertices;
- early screen-outcode rejection;
- GTE average-depth instructions instead of software 64-bit depth math;
- batched world submission;
- two packet arenas and asynchronous ordering-table submission;
- fixed packet and scratch buffers with overflow counters;
- shared clipping and projection helpers from PSoXide.

The latest accepted feature-gated renderer build measured 23.856 fps on the
complete fixed-step E1M1 route in PSoXide, up from the 23.656 fps accepted
predecessor, the 23.432 fps working baseline and the original 21.857 fps
renderer baseline.

The goal remains 30 fps. Emulator timing is useful for comparing revisions, but
only a real console can provide the final result.

### Measured E1M1 attribution

The canonical fixed-step route now brackets renderer construction and the
final tagged-packet-to-OT insertion pass in regression builds. Across 2,134
profiled frames and 2,133 presentation intervals:

- renderer construction consumed 1,616,839,116 bus cycles, 48.9% of the
  measured level interval;
- final OT insertion consumed 69,228,347 cycles, 2.09%;
- a 30 fps cadence would require removing about 897 million cycles, 27.1% of
  the interval or roughly 420,600 cycles per presentation interval.

Deleting OT insertion entirely would therefore supply only 7.7% of the
required saving. A Quake II-style constant-time packet-range splice can still
be useful for proven order-stable ranges, but it is not the main E1M1 gap.

A temporary, more detailed PSoXide-only probe split renderer time as follows:

| Stage | Share of total interval | Share of renderer |
| --- | ---: | ---: |
| world surface pass | 22.47% | 45.9% |
| per-frame face selection and near classification | 9.50% | 19.5% |
| liquid resample and deferred upload | 5.38% | 11.0% |
| entity/model instances | 4.53% | 9.3% |
| PVS/cache upkeep | 2.25% | 4.6% |
| view weapon | 1.86% | 3.8% |
| sky | 1.01% | 2.1% |
| effects and residual 2D/setup work | 1.83% | 3.8% |

Inside the world-surface block, scheduled GTE projection, adaptive
subdivision, and packet emission were 66.0% of its cost (14.83% of the whole
interval). Retained-face materialisation, clipping, and loop work were the
remaining 34.0% (7.64%). This makes cooked visibility/selection and bounded
projection reuse higher-priority experiments than OT relinking alone.

An out-of-band PC/cycle profile of the accepted selection-plus-16-face-block
image sharpens that attribution without adding guest instrumentation. Over the
gameplay interval, `submit_classic_affine_batch` accounted for 16.09% of PC
samples, `select_frame_faces_blocked` 7.21%, the encompassing frame renderer
6.84%, collision tracing 6.38%, liquid warp/update 4.75%, retained-surface
materialisation 3.93%, alias-model submission 2.78%, and scoped-window fan
submission 1.76%. `gpu_end_frame` accounted for 17.04%, including GPU/DMA
waits rather than CPU construction alone.

The same run spent 43.15% of modeled bus cycles issuing instructions and
40.01% on ordinary RAM-load stalls; I-cache refill was another 11.57% and
stack RAM loads alone were 9.44%. Of 1.398 billion issued instructions, 21.66%
accessed RAM, 17.48% were NOPs, and 13.34% were conditional branches. The
accepted `submit_classic_affine_batch` machine-code body is 16,684 bytes,
over four times the PS1's 4 KiB instruction cache. These measurements explain
why exact-output writer variants can lose despite doing fewer arithmetic
operations: hot code size and dependent RAM traffic are first-order costs.

The following exact PSoXide experiments were rejected after complete-route
measurement:

| Experiment | FPS | Reason |
| --- | ---: | --- |
| exact resident topology plan | 18.051 | plan verification and resident patch path overwhelmed saved invariant stores |
| outlined once/twice subdivision | 21.566 / 21.417 | calls and changed code layout outweighed the smaller caller |
| level-zero preflight emitter | 20.855 | proving the fast case before emission duplicated too much traversal |
| exact same-camera static world reuse | 22.059 | two-pool key recurrence was too sparse; tag restoration added a second packet scan |
| hoisted indexed-world view | 22.375 | a logically free decode hoist still regressed through MIPS code layout/register allocation |
| speculative level-zero writer | 20.640 | subdivision rollbacks and cold re-entry overwhelmed the one-pass common case |
| OTZ-only Quake selector | 22.450 | LTO had already removed nearly all of the general error policy |
| GTE AVSZ3 cached-depth keys | 22.542 | exact, but GTE command/interlock latency lost to the MIPS scale |
| conservative 64-to-16-face hierarchy | 22.198 | exact, but admitted super-blocks added more GTE work than they skipped |
| full persistent subdivision slabs | 18.691 | exact hot/cold writer; hashing and noncontiguous packet insertion dominated saved stores |
| shared adaptive GT3/GT4 emitters | 20.990 | exact; per-packet aggregate calls and spills overwhelmed the smaller executable |
| shared complete L1/L2 lattice kernels | 21.559 | exact; one ordinary Rust call per adaptive root still lost register locality |
| shared generic L2 lattice only | 21.265 | exact; also outlined the separate scoped-window L2 writer |
| shared ordinary-world L2 lattice only | 21.258 | exact; its 11.6 KiB callee plus 7.2 KiB caller exceeded the 16.4 KiB inlined submitter |

A larger exact transfer reverses ownership of 2D polygon clipping. PVS,
backface and conservative 3D frustum admission remain unchanged, as do every
adaptive midpoint and packet attribute. Once a source surface is admitted,
the PS1 GPU draw area clips offscreen children instead of the CPU running a
four-edge reject for every generated GT3/GT4. Applying this first to lattice
children reached 23.170 fps; applying it to all compact ordinary polygons
reached 23.367 fps with both canonical hashes in two deterministic full routes.
That is +3.45% over the prior 22.587 selector and +6.91% over the original
21.857 renderer. More importantly, the hot `submit_classic_affine_batch` body
shrinks from `0x3fec` (16,364 bytes) to `0x1b98` (7,064 bytes), moving it much
closer to Quake II's small non-calling kernels. This feature-gated path is the
new performance candidate; wider-map and GPU-command validation remain its
shipping gate. Removing the projected whole-surface scan as well shrinks the
submitter only another 180 bytes and regresses to 23.255 fps, despite exact
hashes. The source-surface scan remains selected.

Suppressing duplicate underdraw along adjacent same-level fan roots reached
22.660 fps, but changed both image hashes and gained only 0.32% over the
accepted build. It remains a diagnostic result, not an accepted renderer.

One byte-exact direct packet-writer experiment was rejected. It reproduced the
SDK packet stream and scratch records over 3,000 randomized batches and kept
both E1M1 image hashes exact, but measured only 21.434 fps. Its larger hot code
raised instruction-cache refill stalls by 37.4 million cycles and total route
time by 65.1 million cycles. The experiment is not present in the runtime.

The liquid stage already updates only selected tiles, once per exact 20 Hz
Quake turbulence phase, and double-buffers its atlas uploads. A visual-exact
four-texel MIPS resampler schedule halved the remaining inner-loop branches,
but retained only 570,955 bus cycles and moved 21.857 to 21.861 fps. The
0.004 fps difference is noise, so the experiment is rejected and the existing
two-texel schedule remains. Dense random source reads and the required VRAM
transfer, rather than loop control, dominate this stage.

A cost-aware follow-up reserved level-two subdivision roots while leaving
level one on the compact dynamic writer. The first three controls all
subtracted the complete 48 KiB modelling budget from the 128 KiB dynamic GPU
arena, even when the 26-slot variant used only 19,448 resident bytes. That
left 80 KiB for ordinary packets; the first gameplay image already dropped
the view weapon before any cache hit could occur. Their 21.063/21.467/21.507
fps results and changed hashes therefore did not isolate resident ordering or
GPU state. Charging the arena for the actual 26 slabs restores 108,840 dynamic
bytes, above the measured 108,488-byte route high-water, and returns both
canonical hashes. It reaches only 21.442 fps, so the level-two-only cache is
still rejected on performance. The corrected conclusion is narrower: bound
resident storage by the slab bytes actually instantiated, and do not infer a
submission-order dependency from a packet-overflow image.

Four exact code-shape controls then tested whether ordinary adaptive expansion
could at least be made compact. Sharing per-packet emitters reached 20.990 fps;
sharing both complete lattice bodies reached 21.559; sharing the generic L2
body reached 21.265; and sharing only the ordinary-world L2 body reached
21.258. The accepted link map's inlined `submit_classic_affine_batch` is
`0x3fec` bytes. The writer-specialized alternative needs an `0x2d34`-byte L2
callee plus an `0x1bfc`-byte caller, so it grows the active pair from 16,364 to
18,736 bytes while forcing writer/root state through the normal MIPS ABI. A
useful compact successor therefore needs a purpose-built register ABI and
fixed packet schedule, matching the recovered Quake II leaf kernels; Rust
outlining alone is closed.

The retail comparison is now instruction-exact. Quake II's
`GlobDrawSubGt4_` occupies only 2,056 bytes, reserves 116 stack bytes, and
makes no calls. Its 804-byte all-visible prefix projects 21 fixed lattice
points with seven RTPT commands, scatters 60 SXY results, copies four source
corners, and fills all 64 coordinate fields of sixteen resident 52-byte GT4
packets exactly once before writing an unrolled tag chain. The failed
Quake-PSX outline is therefore more than nine times larger in active text and
over three times deeper in nested stack. The next serious implementation must
be a small non-calling MIPS scatter/link island fed by cooked packet offsets;
another Rust trait or aggregate-array boundary will not reach the retail code
shape.

A subsequent fixed-schedule control kept Quake-PSX's exact L2 midpoint,
projection, OTZ, packet-order and underdraw rules inside the hot submitter, but
replaced its four-GT3/six-GT4 base expansion with a compact descriptor loop.
The MIPS body shrank from `0x1b98` to `0x1880` bytes (-11.2%) and both
canonical hashes remained exact across two complete routes. Performance fell
from 23.432 to 22.893 fps (3,155,519,282 gameplay bus cycles). The additional
descriptor and point-table RAM loads cost more than the saved instruction
refills. This closes a table-interpreted fixed schedule: the Quake II transfer
must preserve its unrolled register/packet scatter, not merely its small code
footprint.

### Renderer census

A diagnostic-only `renderer-census` build now measures retained-renderer
structure without changing the normal build. Two full fixed-step E1M1 runs in
PSoXide produced 3,795 identical per-frame census rows and matching gameplay,
VRAM and display hashes. Its 14.147 fps result is intentionally invalid for
timing: the image performs extra passes and writes one guest debug line per
frame.

The measured route contained 2,938,276 PVS-face visits. Backface rejection
removed 31.32%, frustum rejection removed 35.91%, and 32.78% reached the world
pass. Only 0.44% of selected faces reached the near path. Five structural
experiments were bounded from those exact decisions:

| Candidate | Census bound | Result |
| --- | ---: | --- |
| consecutive same-plane facing reuse | 51.61% fewer plane-distance tests | rejected in PSoXide A/B |
| conservative 16-face union AABBs | 17.38% fewer net GTE AABB calls | accepted with exact-key selection reuse |
| selected-only, per-batch shared projection | 27.10% fewer eligible transforms | runtime hash rejected; cooker-authored remap remains open |
| previous-face-only shared projection | 16.68% fewer eligible transforms | compact runtime remap rejected in PSoXide A/B |
| exact-key selected-face temporal reuse | 77.23% identical-list upper bound | accepted behind `renderer-selection-cache` |

The same-plane cache retained exact VRAM and display hashes, but its first
layout moved the canonical route from 21.857 to 21.774 fps. Repeating the
experiment on top of exact-key selection reuse measured 22.093 versus
22.128 fps; combining it with the accepted block gate measured 22.599 versus
22.587 fps. Both deltas are far inside the documented 0.122 fps layout band.
A PVS-cached run-length redesign removed the per-face key comparison entirely
but reached only 21.997 versus 22.128 fps: the extra descriptor stream and
nested loop were worse by 0.131 fps. The whole family remains rejected;
avoiding 51.61% of plane-distance calculations does not repay the replacement
control and memory traffic in this register-limited MIPS loop.

The 16-face frustum candidate was reimplemented as a PVS-generation cache:
each successful visible-face rebuild stores one 12-byte conservative union for
every 16 consecutive faces, and the frame selector tests that union before the
authoritative per-face policy, facing, and AABB checks. On top of the accepted
exact selection cache it improved 22.128 to 22.587 fps, saving 66,263,035 bus
cycles over 2,134 presentations (+2.07%). It retained the canonical gameplay,
VRAM (`0x09a7f019bb9a5e7c`) and display (`0x9bac66f3bec0e66b`) hashes. A
32-face sizing A/B reached only 22.522 fps, so 16 remains the selected size.

The feature also cleared broader PSoXide validation: exact fixed-camera world,
HUD, display, packet, and texture-window/reset parity; two deterministic
E1M2/E1M3 traversals through E1M4 with all 86 target edges and hashes
`0xb43b84dba8258f74`/`0xdd1e0c9c06d994cf`; and a telemetry-free release boot
that remained live with 54,048 bytes heap free against the 8,192-byte floor.
The extra resident allocation is bounded to 83 blocks, or 996 bytes. The
implementation remains feature-gated for original-hardware validation.

The temporal candidate preserves `frame_face_indices` only when the complete
camera, map-generation/PVS/portal key, and active water plane exactly match the
previous rendered frame. A miss runs the established selector and near pass;
a hit also skips the four-plane GTE setup. It makes no motion tolerance or
visibility approximation. On the canonical E1M1 route it improved 21.857 to
22.128 fps, saved 40,558,116 bus cycles, and retained the exact gameplay,
VRAM (`0x09a7f019bb9a5e7c`) and display (`0x9bac66f3bec0e66b`) hashes across two
runs. The +0.271 fps gain is 2.22 times the measured layout-noise band.

Broader PSoXide validation also passed: the pinned E1M1 owner camera retained
its exact world, HUD and display hashes and all 10,736 texture-window/reset
pairs; two 5,687-frame E1M2/E1M3 routes reached E1M4 with all 86 target edges
and exact cross-run VRAM/display hashes; and a release-style boot remained live
with 57,092 bytes of heap free against the 8,192-byte floor. The implementation
remains feature-gated until original-hardware timing and visual confirmation,
but it is the first census-derived runtime candidate to clear the PSoXide
performance, determinism and visual gates.

The projection bound is deliberately not a whole-PVS or whole-map cache. It
groups only ordinary selected surfaces within the existing 39-corner,
13-surface batch limits, excludes near-clipped and special surfaces, and found
4,288,997 eligible corners but only 3,126,684 unique positions. Any
implementation must retain current material attributes, subdivision, packet
order and fallback paths, then pass the same byte-exact visual and route A/B.

A bounded runtime implementation used a 64-slot generation-tagged hash per
batch, projected only unique positions, and scattered cached screen/depth to
duplicate corners. It retained exact gameplay, VRAM and display hashes, but
fell from 21.857 to 18.641 fps and added 570,095,895 bus cycles (17.25%). The
experiment is rejected and is not present in the runtime. This rules out
discovering the remap on the R3000A; a future attempt must have the cooker
author compact batch-local indices so the guest consumes sequential records
without hashing.

A second runtime implementation removed the hash and retained only a compact
previous-face map. The census showed that this subset could reuse 715,310
projections, or 16.68% of eligible corners and 61.54% of all shared-position
hits. It also retained exact gameplay, VRAM and display hashes, but measured
18.716 fps and added 554,671,917 bus cycles versus the 21.857 fps baseline.
It is rejected and is not present in the runtime. Even this bounded lookup,
scatter and alternate submission path costs substantially more on the R3000A
than the scheduled GTE transforms it avoids. Projection reuse should therefore
be expressed as a cooker-authored sequential draw representation, not
rediscovered or scattered per frame.

### GP0 comparison with Quake II PSX

The `e1m1-gpu-census` action captures PSoXide's emulator-owned GP0 counters
for the accepted selection-cache plus 16-face block build. The analyzer derives
the gameplay interval from the same CD-session boundaries as the canonical FPS
metric. Across 2,133 complete E1M1 presentations it observed:

| Metric per present | Quake-PSX mean | P50 | P95 | Maximum | Quake II movement mean |
| --- | ---: | ---: | ---: | ---: | ---: |
| GP0 commands | 1,113.45 | 1,173 | 2,208 | 3,089 | 559.94 |
| draws | 983.18 | 1,007 | 2,053 | 2,609 | 521.71 |
| textured triangles | 774.43 | 725 | 1,680 | 2,057 | 0 |
| textured quads | 204.83 | 175 | 382 | 620 | 510.38 |
| texture-window commands | 124.61 | 116 | 536 | 930 | 5.20 |
| GPU cycles (emulator estimate) | 611,881 | 588,946 | 1,125,505 | 1,280,614 | — |

Quake II renders true 512x240 and held an exact two-field cadence throughout
its retail capture, yet its movement stream has about half as many GP0
commands, no textured triangles, and roughly one twenty-fourth as many texture
window commands as Quake-PSX at 320x240. Quake-PSX's mean hardware-triangle
equivalent is about 1,184 per present (`GT3 + 2*GT4`), versus about 1,021 for
Quake II, so raw visible triangle count alone is not the explanation. Packet
shape and state traffic are material differences.

The Quake-PSX GPU estimate attributes 53.84% of cycles to textured triangles,
44.79% to textured quads, 1.06% to the framebuffer fill, and 0.31% to other
work. The fill is cleanup-scale. The more important architectural targets are:

- move ordinary static topology toward cooker-authored GT4 packet objects,
  while preserving the current exact-order and near/subdivision fallbacks;
- stop rebuilding invariant command, colour, UV, CLUT and TPAGE words on an
  exact resident-topology hit;
- investigate a cooker representation for tiled surfaces that reduces scoped
  GP0(E2) selector/reset pairs without changing texture repetition or OT order.

The relaxed GT4 pairing experiment demonstrates the GPU ceiling but is not an
accepted implementation. Allowing neighbouring OT keys reduced textured
triangles by about 26.6%, increased quads by 51%, reduced commands by 10.8%,
draw words by 7.2%, and estimated GPU cycles by 4.0%. It changed the canonical
frame hashes and its 22.183 fps result was inside CPU timing noise, so the
ordering change is rejected. A valid cooker path must obtain the packet-shape
benefit without merging distinct depth decisions.

A stronger fixed-fan ceiling removes runtime subdivision from ordinary world
fans completely. Its compact 2,060-byte submitter emits adjacent GT4 pairs
plus an optional GT3 and completed two deterministic routes at 24.353 fps:
+11.42% over the original renderer and +7.82% over the accepted selector
build. Mean GP0 commands fell from 1,113.45 to 702.24, draws from 983.18 to
571.96, and the emulator GPU estimate from 611,881 to 506,003 cycles. This is
the first material proof that Quake II's fixed packet shape transfers to the
destination CPU/GPU balance.

It is deliberately not a visual candidate. The canonical hashes changed to
VRAM `0x4d3f7ffbcafd3a44` and display `0x648a0e9cf9f191a8`; a captured E1M1
view loses a near wall because all original polygon corners lie outside the
viewport even though the polygon crosses it. Reference subdivision creates
interior vertices that survive the established pairwise screen-reject rule.
The speedup therefore includes missing subdivision work and geometry.

Repeating that ceiling on the complete accepted August 2026 renderer stack
measured 24.628 fps versus its 23.825 fps stable baseline. Removing all
ordinary-world adaptive subdivision therefore saves only about 3.3% of frame
cycles after the other accepted improvements. Even this deliberately
incorrect topology remains more than five frames per second short of 30 fps.
Subdivision is no longer the primary architectural target; the larger gap is
work admitted before submission, especially broad BSP visibility candidates
that still incur selection, materialization, and packet costs.

A quality ladder which retains the complete level-two lattice but flattens
level-one roots restored that near-wall coverage and measured 23.149 fps
deterministically (+5.91% over original, +2.49% over accepted). Its submitter
is 12,900 bytes; mean commands/draws/GPU cycles were
785.73/655.46/530,792. It still changes the image
(`0x1b5289dc0903d058`/`0x37a924b4cdf18152`) and visibly increases affine warp,
so it too is evidence rather than a shipping option. Together the two points
isolate the target: retain fixed base packets, but make subdivision leaves
persistent and cheap as Quake II does.

Reproduce the current census with:

```sh
cargo run --release -- e1m1-gpu-census \
  --psoxide ../PSoXide/target/release/frontend
python3 tools/analyze_psoxide_gpu.py \
  captures/e1m1-gpu-census/gpu.csv \
  captures/e1m1-gpu-census/route.csv \
  captures/e1m1-gpu-census/cd.csv
```

GP0 counts are direct observations. PSoXide's GPU cycle fields remain emulator
estimates and cannot establish original-silicon headroom.

### Quake II transfer compatibility

`tools/quake2-transfer-census.rs` now tests the retail static-brush shape
against all nine cooked Quake Episode 1 maps before another runtime format is
committed. It derives exact camera-leaf visibility signatures from the source
BSP and cooked PVS, joins only faces sharing a complete boundary edge, and
enforces Quake II's recovered 32-face/255-position brush caps.

The literal transfer is not viable. Episode 1 has 39,079 PVS-addressable world
faces and 9,044 exact visibility classes. Preserving source order produces
21,558 ranges averaging only 1.81 faces; joining shared-edge components reduces
that to 10,688 ranges averaging 3.66 faces, but 96.5-98.4% of faces would move
from their current order across the individual maps. Treating those ranges as
unconditionally direct `AddPrims` chains would therefore change established OT
semantics. They are useful projection/template objects, not proof that sorting
can be removed.

Memory also rules out a full-PVS resident-template table. Pairing adjacent fan
triangles into 52-byte GT4 packets with a 40-byte GT3 remainder gives these
per-view immutable base-packet footprints:

| map | p50 | p95 | maximum |
| --- | ---: | ---: | ---: |
| Start | 40 KiB | 73 KiB | 93 KiB |
| E1M1 | 45 KiB | 82 KiB | 105 KiB |
| E1M2 | 37 KiB | 59 KiB | 87 KiB |
| E1M3 | 36 KiB | 58 KiB | 68 KiB |
| E1M4 | 40 KiB | 68 KiB | 84 KiB |
| E1M5 | 27 KiB | 58 KiB | 74 KiB |
| E1M6 | 40 KiB | 67 KiB | 99 KiB |
| E1M7 | 48 KiB | 69 KiB | 73 KiB |
| E1M8 | 56 KiB | 91 KiB | 110 KiB |

Those are pre-facing/pre-frustum base templates alone. Pinning a maximum view
would consume most of the existing 128 KiB per-frame packet arena before near
clipping, affine subdivision, entities, particles, HUD, and the fixed 8 KiB
overflow reserve. A bounded hot prefix plus exact dynamic tail is required.

A second census then rejected a **map-global** hot prefix inside that arena.
Ranking exact-visibility connected objects by camera-leaf frequency gives only
15.8% packet-byte coverage for a 32 KiB E1M1 prefix. Its optimistic full-PVS
static high-water reaches 118 KiB, leaving about 1 KiB before the fixed reserve;
E1M8 reaches 128 KiB and already overflows one view. Unselected global packets
consume the prefix without reducing that frame's dynamic tail. Residency must
therefore be leaf/PVS-keyed, or use independently budgeted memory; it cannot be
a permanent map-wide allocation carved from the current arena.

The QRC2 selected-stream measurement makes a leaf-local candidate much more
specific. Across 3,795 deterministic E1M1 frames, 140 visibility rebuilds mean
one stable key lasted 27.1 frames on average. Selected ordinary base topology
was 20,172/27,216/31,732 bytes P50/P95/max. Faces with baked UV and lighting,
ordinary materials, no near clip, and no intersecting dynamic light supplied
67,591,848 of 71,369,860 candidate bytes (94.71%), with a
19,924/27,216/29,824-byte P50/P95/max. No candidate was invalidated by a
dynamic light on this route.

Actual complete packet-arena use was 54,228/70,636/97,336 bytes P50/P95/max,
with 5,191,531 packets, 6,383,314 hardware triangles, and no overflow frame.
If every eligible base byte replaced an existing dynamic byte one for one, a
32 KiB leaf-local prefix would bound at 72,688/77,432/111,060 bytes and clear
the 120 KiB safe limit on every measured frame. That is deliberately labelled
an optimistic bound: adaptive subdivision expands the stream, so final packet
provenance must prove which bytes are truly replaced. Without any proven
replacement the worst frame would reach 130,104 bytes and fail.

QRC3 closes that provenance gap by observing the real compact submitter after
it has projected each ordinary world batch. The diagnostic pass shares the
shipping subdivision selector and records root level, adjacent level-zero
pairing, crack-sealing underdraw, whole-surface rejection, theoretical packet
bytes before polygon screen rejection, actual output bytes, and a deterministic
topology fingerprint. Two complete PSoXide routes matched all 3,795 rows and
retained the exact gameplay, VRAM (`0x09a7f019bb9a5e7c`) and display
(`0x9bac66f3bec0e66b`) hashes.

The selected ordinary stream contained 2,597,514 root triangles. Level zero
handled 2,300,662 (88.57%), level one handled 164,685 (6.34%), level two
handled 36,554 (1.41%), and whole-surface screen rejection removed the other
95,613 roots (3.68%). All level-one roots and 34,476 of 36,554 level-two roots
needed underdraw, so subdivision residency must include those crack-sealing
variants rather than treating them as rare fallbacks.

The actual final ordinary stream was 39,036/47,548/56,664 bytes P50/P95/max,
or 172.87% of the un-subdivided base bytes over the whole trace. Reserving the
complete theoretical topology, including slots later removed by polygon-level
screen rejection, costs 44,100/55,952/75,648 bytes. The rejected-slot overhead
was 16.97% in aggregate. Replacing the actual ordinary stream with that more
conservative theoretical prefix gives a complete-arena bound of
63,192/75,700/108,488 bytes P50/P95/max: no frame exceeds the 120 KiB safe
limit. A stable-attribute subset can only reduce this bound because every
excluded surface removes at least as many theoretical bytes as actual bytes.

Topology persistence is high enough to justify the cache. The selected-face
fingerprint repeated on 2,930 of 3,794 transitions. On those transitions the
adaptive topology also repeated 2,884 times (98.43%); across every frame it
repeated 2,885 times (76.04%), with a 1,659-frame longest run. The transferable
unit is therefore an exact `(selected-stream, adaptive-topology)` working set.
Visibility alone is insufficient, but topology changes do not erase its
temporal value.

Four PSoXide-only A/Bs further distinguish the mechanism from superficial
temporal caches:

| Experiment | Exact output | PSoXide result | Decision |
| --- | --- | ---: | --- |
| conservative leaf-AABB facing tags | yes | 21.857 -> 21.936 fps | reject; +0.079 is below the 0.122 fps layout band |
| exact plane/order scene-stream reuse | yes | 21.857 -> 21.344 fps | reject |
| exact-key previous packet-range restore combined with selection reuse | yes | 22.128 -> 21.917 fps | reject |
| sequential previous-arena packet copy | yes | 21.857 -> 21.655 fps | reject |

The leaf experiment is especially useful as a bound: conservative 32-unit
source-leaf boxes prove 72.47% of leaf/face facing decisions invariant, but the
extra tag tests do not repay even those avoided plane products. The packet
experiments show that restoring/copying completed streams still pays memory
traffic for every invariant UV, colour, CLUT, TPAGE, and command word. Quake II
does not do that: `InitBrush` installs a 52-byte GT4 template directly in both
destination pools, then the frame kernel changes only four XY words and the DMA
link. Thirty-two of the 52 packet bytes remain invariant.

### Offline tessellation memory bound

`fixed-quad-tessellation-census` tests whether subdivision can simply be moved
wholesale into PSB5. It clips every ordinary source polygon on a 64-, 128-, or
256-unit texture-space grid, deduplicates the resulting quantized positions,
expands every affected leaf mark, and measures the replacement face/corner
records plus fixed GT3/GT4 base packets.

The naive global form is not viable. For E1M1, 128-unit cells add 4,061 faces,
16,871 corners, 3,990 positions, 5,743 leaf marks, about 236 KiB of immutable
base packets, and 211,004 resident bytes. The projected resident map is
919,052 bytes, 39,052 beyond its 880,000-byte arena. A coarse 256-unit grid
fits E1M1 with 65,114 bytes spare, but E1M2, E1M3, and E1M4 still exceed the
arena by 56,566, 101,862, and 115,190 bytes respectively. A 64-unit grid
overflows E1M1 by 335,194 bytes.

This rules out permanent map-global tessellation as the explanation or the
transfer mechanism. The bounded design must keep compact source/base geometry
and allocate/cache only the subdivision leaves actually requested by the
camera, with eviction or fallback under pressure. Reproduce the corpus bound
with:

```sh
cargo run --release --bin fixed-quad-tessellation-census
```

The next implementation is therefore a cooker/runtime contract, not another
feature branch inside the current per-face writer:

1. The cooker triangulates each convex surface once, pairs adjacent fan
   triangles into fixed GT4s, emits the odd GT3 remainder, and assigns compact
   batch-local position indices.
2. It prebuilds invariant command, colour, UV, CLUT and TPAGE fields for both
   display pools and gives each packet object a stable identity.
3. A variable leaf/PVS-and-topology-keyed working set reserves the complete
   theoretical ordinary packet shape, including screen-rejected slots. The
   measured maximum is 75,648 bytes and the conservative combined high-water
   is 108,488 of the safe 122,880 bytes. A map-global prefix remains rejected.
   Each display pool owns its own key and contents; misses rebuild invariant
   fields directly and never copy last frame's stream.
4. The selected-face stream retains source/OT order. Only order-stable object
   ranges use direct splices; near, water, sky, and ambiguous ranges keep their
   existing paths.
5. The runtime consumes cooker-authored local indices sequentially, projects
   each object position once, and patches only current-pool XY/link words.
   Runtime hash tables, lookup/scatter maps, and whole-stream copies are ruled
   out by the measured regressions above.
6. Adaptive affine leaves become a fixed-capacity topology/packet cache keyed
   by source packet and the exact selected topology class, with deferred
   reclamation across GPU ownership and the existing dynamic fallback. QRC3's
   98.43% conditional and 76.04% overall topology persistence are the cache-hit
   targets; the current writer remains authoritative on a miss.

The clean-room decomp crate now encodes this contract in
`renderer::resident_packets`: exact fan-packet footprints, dual-pool GT4 patch
isolation, deterministic cell-local placement, preserved render order, and a
per-view arena high-water proof. QRC3 now supplies the adaptive packet
provenance that the earlier optimistic audit lacked. A feature-gated topology
working-set implementation is authorized, but it must retain the current
writer on misses and pass PSoXide gameplay, VRAM, display, packet-order,
arena-high-water, and canonical 0.122 fps noise-band gates before acceptance.

The runtime residency controls now close the per-root and per-polygon forms.
A bounded 26-slot L2 stream cache remained exact at 22.276 fps. Replacing its
generic hit writer with a fixed, non-calling 3,048-byte position-only scatter
reached 22.272 fps, and outlining cold lookup/initialization reached 22.233
fps. L2's 1.41% root share cannot repay the surrounding cache machinery.

Moving residency to the 88.57% L0 population made the rejection decisive. A
512-slot direct-map GT3/GT4 cache reached 20.827 fps. Four-way associativity
plus batch-folded counters raised logical hits from 898,102 to 1,238,998 and
cut fallbacks from 872,182 to 526,651, but fell again to 20.583 fps. Both runs
retained canonical VRAM `0x09a7f019bb9a5e7c` and display
`0x9bac66f3bec0e66b` hashes. Greater cache effectiveness made timing worse
because every resident polygon fractures the compact dynamic stream and
returns to lookup/insertion machinery.

Do not tune the per-polygon cache further. The next admissible control is one
fixed contiguous range per bounded L0-only batch: at most 39 vertices and 13
surfaces fit a closed 988-byte packet bound, so a 1 KiB slab can use one batch
identity check, one tag/XY patch loop and one tagged-stream link. Begin with 32
slabs per display pool and preserve the authoritative dynamic writer for
adaptive, oversized, changed or already-live ranges. The complete handoff and
clean-room range model are committed privately at `quake2-psx-decomp`
`34f6f8b`.

### Quake II-informed selector/materialization leader

The next profile-guided pass broke the 23.428 fps working baseline without
changing resolution, draw order, polygon topology, simulation cadence, or any
rendered pixel. The accepted stack completes the deterministic E1M1 route in
3,028,132,969 bus cycles at 23.856 fps. Both complete runs present 2,134 frames
and retain VRAM hash `0x09a7f019bb9a5e7c` and display hash
`0x9bac66f3bec0e66b`. This saves 54,838,749 cycles and gains 0.424 fps (1.81%)
against the previous documented 23.432 leader. The final scratchpad step alone
saves 25,705,766 cycles and gains 0.200 fps against the 23.656 predecessor.

The cumulative exact A/B sequence was:

| Stack addition | Bus cycles | Fixed-step fps | Increment |
| --- | ---: | ---: | ---: |
| documented cell-policy leader | 3,082,971,718 | 23.432 | baseline |
| GTE near classification | 3,078,401,738 | 23.467 | +0.035 |
| retained liquid policy | 3,068,691,494 | 23.541 | +0.074 |
| Quake-specialized submit kernel | 3,057,266,396 | 23.629 | +0.088 |
| liquid visibility fast rejection | 3,054,981,456 | 23.647 | +0.018 |
| inline baked-corner materializer | 3,054,410,221 | 23.651 | +0.004 |
| scheduled liquid-warp delay slots | 3,053,838,735 | 23.656 | +0.005 |
| scratchpad liquid phase window | 3,028,132,969 | 23.856 | +0.200 |

The first change keeps near classification as a separate, register-light pass
but places its three-product AABB support test in the otherwise unused second
GTE light-matrix row. The four frustum planes and auxiliary near plane are
loaded together only on a selection-cache miss. The second change stores a
liquid bit in the retained cell's spare surface-index bit. This removes the
texture-table load from every PVS face in the selector and lets the later
visible-liquid scan reject ordinary faces before reading material state. The
last change copies Quake's most useful code-shape lesson: the dominant baked
indexed-corner gather is one fixed, non-calling MIPS schedule inside the
materialization body. Dynamic light styles and UV offsets still use the
authoritative generic path.

The liquid-warp disassembly exposed another assembler code-shape trap. In
reorder mode each explicit branch-delay NOP acquired a second inserted NOP.
An explicit `.set noreorder` schedule moves the destination advance into the
inner branch delay slot and precomputes the next row in the outer slot. The
exact 64x64 resample shrinks from `0xa8` to `0x9c` bytes and saves 571,486
route cycles. A four-texel unroll grew the kernel to `0xec` bytes and regressed
to 3,054,410,276 cycles (23.651 fps), so the smaller two-texel form remains
authoritative.

The final step copies the 64-byte turbulence displacement window into the
PS1 scratchpad once per active liquid phase. Every visible 64x64 liquid tile
then reuses one-cycle scratchpad reads instead of fetching 4,096 displacement
entries from main RAM. The original source tile, warped upload buffer, atlas
coordinates and 20 Hz phase policy remain unchanged.

The first research build measured 3,028,704,094 cycles at 23.852 fps, but it
depended on two auxiliary GTE helpers present only in an ignored SDK hydration.
The accepted version composes the same fifth AABB plane from PSoXide's pinned
public matrix API and retains the MAC2 test locally. A clean hydration of
PSoXide `5048fbde0ea650c8f728f1fb271a9529a447a90b` now builds the feature stack
without transient source edits. The clean reconstruction is also 571,125
cycles faster than that research build.

An out-of-band PSoXide PC sample on the accepted predecessor kept the next
targets honest. `gpu_end_frame` waits account for 20.05% of samples, the
specialized world submitter 11.11%, collision trace 7.13%, `draw_frame` 6.73%,
the Quake loop 6.44%, sector reading 5.88%, face selection 5.25%, liquid warp
5.15%, and materialization 4.02%. Across the measured window the CPU spends
43.66% of cycles issuing instructions, 42.01% stalled on RAM loads, 8.86% on
I-cache misses, and 8.53% on stack-load stalls. The evidence therefore still
favours compact fixed schedules and fewer dependent RAM reads; it does not
support adding another runtime cache or descriptor interpreter.

Exact but slower controls remain feature-gated for research. Propagating
hierarchical block clip flags reached only 23.463 fps because the selector grew
from `0x7f0` to `0x9f0` bytes. The first out-of-line baked materializer reached
23.471 fps, and fusing near classification into the selector reached 23.638
fps versus its 23.647 parent. A reordered inline-assembly prototype was also
discarded after disassembly exposed a destination increment moved out of the
branch delay slot; the accepted routine uses an explicit `.set noreorder`
schedule. These are code-shape rejections, not visual failures: the completed
controls retained the canonical hashes.

Reproduce the leader or build its non-regression playable disc using only
PSoXide:

```sh
cargo run --release -- e1m1-gpu-polygon-scratch-liquid-bench \
  --psoxide ../PSoXide/target/release/frontend
cargo run --release -- gpu-polygon-scratch-liquid-disc \
  --psoxide ../PSoXide/target/release/frontend
cargo run --release -- e1m2-e1m3-scratch-liquid-route-regress \
  --psoxide ../PSoXide/target/release/frontend
```

The playable image passes the shipping boot gate with 62,240 bytes of heap
free. Two complete authored E1M2/E1M3 runs also agree exactly: 6,189 gameplay
frames, all `0x1fffffff` mechanism bits, 86 target edges, two map transitions
into E1M4, VRAM hash `0x4c2b7b22ffcc6780`, and display hash
`0x3438b9054b141195`.

Run the PSoXide-only census and analyzer with:

```sh
cargo run --release -- e1m1-renderer-census \
  --psoxide ../PSoXide/target/release/frontend
python3 tools/analyze_renderer_census.py \
  captures/e1m1-renderer-census/run-a/console.log \
  captures/e1m1-renderer-census/run-b/console.log
```

Run the static transfer census with:

```sh
cargo run --release --bin quake2-transfer-census
```

The action never invokes DuckStation. `tools/analyze_renderer_census.py`
validates the selection funnel, checks every row across both runs, and reports
net block-test costs rather than gross rejected faces.

Re-run the accepted selection-cache gates with:

```sh
cargo run --release -- e1m1-selection-cache-bench \
  --psoxide ../PSoXide/target/release/frontend
cargo run --release -- selection-cache-regress \
  --psoxide ../PSoXide/target/release/frontend
cargo run --release -- selection-cache-ship-boot \
  --psoxide ../PSoXide/target/release/frontend
```

### Where the E1M1 frame actually goes (August 2026 worker pass)

The stable baseline was reproduced from clean source before any experiment:
2,134 presentations, 3,034,987,462 cycles, 23.803 fps, VRAM
`0x09a7f019bb9a5e7c`, display `0x9bac66f3bec0e66b`. That is 0.022 fps from the
previously recorded exact capture with identical hashes, so it sits inside the
established layout-noise band.

Two measurements reframed the target.

**The renderer is not waiting on the GPU; it is waiting on the display.** A
PSoXide PC-sample profile over gameplay ticks 600..5700 attributes 22.05% of
samples to `gpu_end_frame`, but 17.69 of those points sit on exactly four
instructions at `0x8004b56c..0x8004b578`. Disassembling them shows
`lw at,(counter); nop; beq at,v1; nop`: the vblank counter spin inside
`wait_vblank`. `wait_for_pending_submission`, the actual GPU fence, never
appears. The GPU finishes before the CPU does, so submitted GPU work is not the
limiter and `gpu_end_frame` contains only about 4.4% of real CPU work.

**A frame is therefore quantized to whole NTSC fields, and the useful metric is
work per frame, not fps.** Presentation intervals over the canonical route
partition as 8.44% one field, 39.01% two, 48.62% three, 3.05% four, 0.75% five
and 0.14% six. 47.45% of frames already present at 30 Hz or better. Subtracting
the measured spin share per 64-tick window gives the work distribution against
the 1,130,089-cycle two-field budget:

```text
mean work/frame  1,227,919 cycles  2.173 fields
p50              1,264,508          2.24
p75              1,421,363          2.52
p90              1,586,693          2.81
p95              1,624,573          2.88
p99              1,976,447          3.50
```

A stable 30 fps therefore needs roughly a **29-31% work reduction** across the
heavy sections and about 43% for the worst window, not the 20% the mean fps
suggests.

### Diagnostic ceiling: selected-face sensitivity

`renderer-selection-decimate` keeps every other accepted face in
`select_frame_faces_blocked`. It is a labelled diagnostic; the image is wrong
and its hashes must differ.

```text
presentations: 2,134
cycles:        2,633,978,973
fps:           27.427
VRAM hash:     0x5927dda76aa5e224
display hash:  0x7f0eb055aee3041d
fields:        9.66% one, 67.65% two, 19.78% three, 2.67% four, 0.23% five
```

Halving the selected world faces removes 401,008,489 cycles (13.2%) and moves
the two-field share from 47.45% to 77.31%. This fixes the slope: the
face-proportional part of the frame is about 24% of all work, so **even
removing every world face cannot reach a stable 30 fps on its own**. The
remainder is collision, game logic, liquid warping, entities and fixed renderer
cost.

Gameplay-window symbol shares behind that conclusion (PC samples, ticks
600..5700, 312,673 samples):

```text
gpu_end_frame                     22.05%  (17.69 spin, 4.36 real)
submit_quake_classic_affine_batch 11.89%
Renderer::draw_frame               7.44%
collision trace_into               7.36%
Quake run                          7.01%
select_frame_faces_blocked         5.76%
liquid warp_tile_64_prepared       4.99%
materialize_surface                4.25%
SceneCollision trace               3.02%
alias model submit                 2.90%
mark_visible_faces                 2.60%
point_leaf_index                   2.26%
memcpy                             1.98%
scoped windowed fan                1.95%
```

Grouped: world face path about 35%, collision and physics 15.7%, game logic
7.6%, liquid 5.6%, entities 4.3%, real `gpu_end_frame` 4.4%.

### Exact BSP portal reconstruction and its RAM verdict

No `.prt` or `.map` files exist locally, so `tools/portal-census.rs` rebuilds
the portal graph from the compiled BSP with the standard qbsp
`MakeHeadnodePortals`/`MakeTreePortals` construction, including qbsp's
`WindingIsTiny` rejection. For E1M1 it recovers 3,312 leaf-to-leaf portals
between open leaves over 2,750 nodes and 1,531 leaves, mean 4.00 vertices and
at most 8, with 4 portals per leaf at p50 and 12 at p95.

The census then samples every open leaf centre at eight yaws with the runtime's
own four-plane frustum (`forward +- right`, `forward +- up`) and compares what
a conservative portal walk admits against the current PVS-plus-frustum path:

```text
mode         cells  doorways   pvs  frustum  admitted   removed  tests/frame
leaf/rect     1531      3312  759.0    260.5     138.5   46.85%        847.8
leaf/aabb     1531      3312  759.0    260.5     189.6   27.22%        239.7
leaf/pair     1531      3312  759.0    260.5     227.4   12.69%        299.7
merge>=16384  1242      2667  759.0    260.5     172.1   33.93%       1475.0
merge>=4096    763      1260  759.0    260.5     214.0   17.86%       1896.0
merge>=1024    495       349  759.0    260.5     232.5   10.73%        534.1
merge>=256     408        69  759.0    260.5     242.1    7.05%        107.5
```

`leaf/rect` is the exact recursive screen-rectangle narrowing; `leaf/aabb`
crosses a portal when its world AABB survives the frustum planes, with no
projection and therefore no near-plane hazard; `leaf/pair` replaces the portal
bound with the intersection of the two leaves' already-resident 32-unit leaf
bounds, so it needs no cooked geometry at all. Merging leaves into rooms by
portal area was tested across five thresholds and consistently loses more
rejection than it saves in doorway count.

Combined with the decimation slope, `leaf/aabb` is worth about +1.9 fps and
`leaf/rect` about +3.4 fps before paying for the walk.

**This does not fit in RAM.** The guest reserves one 880,000-byte resident-map
arena and the largest cooked map already needs 865,958 bytes, leaving a
**14,042-byte margin for every map**. The cheapest honest leaf-portal layout for
E1M1 alone is a `[u16; leaves+1]` offset table (3,064 bytes) plus one `u16`
neighbour per portal side (6,624 entries, 13,248 bytes) — 16,312 bytes with no
portal geometry at all, and that layout is exactly the weak `leaf/pair` variant.
Adding a portal AABB good enough for `leaf/aabb` costs another six bytes per
portal. Byte-level packing (delta-coded neighbours, fraction-of-leaf-box
bounds) reaches roughly 26 KB, still nearly twice the whole arena margin, and
the larger maps have less room than E1M1, not more.

Per-map sidecar cost for the plain layout (leaf offset table, one `u16`
neighbour per portal side, one 6-byte portal AABB per portal):

```text
start 35,078   e1m1 36,184   e1m2 35,142   e1m3 29,680   e1m4 41,896
e1m5  27,644   e1m6 17,914   e1m7  8,306   e1m8 14,800
```

Every Episode 1 map except `e1m7` and `e1m8` exceeds the whole arena margin.

**A bounded gate budget does not rescue it either.** Scoring each portal by how
often it actually stopped the walk and keeping only the best K as gates, then
merging the leaves either side of everything else:

```text
gates   cells  doorways  sidecar  admitted  removed  tests/frame
   64     385         0     772B     243.9    6.37%          0.0
  128     385         0     772B     243.9    6.37%          0.0
  256     387        27   1,046B     243.6    6.47%         19.6
  512     389        29   1,070B     243.5    6.51%         21.0
 1024     418       451   5,348B     240.2    7.77%        230.2
 2048     668     1,656  17,898B     219.6   15.68%        178.2
 3312    1531     3,312  36,184B     189.6   27.22%        239.7
```

The rejection value is spread thinly across the entire portal set: there is no
small high-value subset. Inside the 14,042-byte budget the best result is about
7.8% fewer admitted faces, which the decimation slope prices at roughly +0.5
fps before paying for the walk, the admitted-face bitset and the extra
selector test. Portal admission is closed at every affordable budget, and the
census binary is retained so the numbers can be re-derived rather than
re-argued.

### Accepted: cull against the frustum the GPU can actually draw

`quake_frustum` built its four planes as `forward +- right` and `forward +- up`,
a 90-degree half-angle on both axes. The horizontal one is right: OFX 160 over a
160 projection plane puts the screen edge at exactly tangent one. The vertical
one is not. OFY 120 over the same plane puts the top and bottom edges at 0.75,
so a third of the culled volume was an off-screen band whose faces were
selected, materialized, submitted and then discarded by the draw area.

`renderer-screen-frustum` scales the forward component of the two vertical
planes by 3,277/4,096. The water warp only ever lengthens the projection plane
(165 +- 2) and shifts the offsets by two pixels, so 0.8 stays conservative for
every configured window.

```text
presentations: 2,134
cycles:        3,020,706,845   (baseline 3,034,417,010)
fps:           23.915          (baseline 23.803)
VRAM hash:     0x09a7f019bb9a5e7c
display hash:  0x9bac66f3bec0e66b
```

Both canonical hashes are exact, which is the proof that the removed work was
invisible. The gain is 13.7 million cycles, 0.45%. That is under the 0.122 fps
layout-noise band on its own, but the cycle reduction with byte-identical output
is not noise, and it costs nothing.

### Accepted: redundant collision, visibility and packet work

Four exact changes, all with byte-identical canonical hashes, measured together
because each is small on its own.

`EntityScene::trace_hull` had no broad phase. Its sibling `SceneCollision::trace`
already builds a `SweptUnitBox` and skips candidates that cannot overlap it, but
the hull path traced all 29 solid submodels of E1M1 through their full hulls on
every call regardless of where the mover stood. `monster_step` fans over six
directions, so one blocked monster cost up to 180 hull traces. The same filter
is now applied with a 64-unit margin, which covers the largest Quake hull.

The camera was located in the BSP five times per frame on one unmoving point:
`prepare_visibility`, `mark_visible_faces`, `water_portal` and the view-model
lighting each descended independently. `mark_visible_faces` cached its result
but only after the descent. One memo keyed on `(generation, origin)` collapses
all five to one, and `water_portal` now takes the resolved leaf.

The GT4 pairing test re-derived the subdivision level of a neighbour it had just
proved sat at the same depth key. When the profile carries no affine-error term,
`QUAKE_REFERENCE` included, the level is a pure function of that key, so the
three-way ladder and its range guard are already known. Disassembly confirmed
the dead half: `next_otz < 60` is strictly implied by `next_otz < 136`, and the
`andi` feeding it was computed twice. The general form stays for profiles that
consult the UVs.

`plane_contact` reached `__divdi3` four times per contact. R3000A has no 64-bit
divide. `div_q12_i32` produces the same `numerator * 4096 / denominator` from a
whole/remainder split in 32-bit arithmetic, so the fraction no longer calls it.
The three endpoint terms keep the widening product: a 64-bit *product* is native
`mult`, and `long_floor_probe_keeps_subunit_contact_precision` proves that a Q16
or Q31 ratio loses sub-unit precision over a 32,768-unit probe.

```text
                                        cycles           fps
session baseline                 3,034,417,010        23.803
+ drawable frustum               3,020,706,845        23.915
+ broad phase, camera memo       3,004,712,412        24.042
+ pairing ladder, plane_contact  2,965,296,903        24.362
```

VRAM `0x09a7f019bb9a5e7c` and display `0x9bac66f3bec0e66b` at every step.

### The frame at the cadence a 30 fps build would actually run

`perf-fixed-ticks` advances three simulation ticks per frame, which matches
today's ~24 fps over a 60 Hz clock. A build holding 30 fps consumes two.
`perf-fixed-ticks-30hz` measures that:

```text
presentations: 1,982
cycles:        2,699,670,817
fps:           24.852
fields:        3.63% one, 57.34% two, 36.55% three, 1.82% four, 0.66% five+
```

**60.97% of frames already present in two fields**, against 47.45% at the start
of this pass. At 1,362,094 cycles per frame and a 17.7% vblank-spin share, mean
work is about 1,120,000 cycles against the 1,130,089-cycle two-field budget.
**The mean already fits.** What remains is entirely the tail: 36.55% of frames
land between two and three fields, and the GPU census shows those frames carry
1,529.6 commands against 947.8 for the frames that fit. The tail is geometry,
not a fixed overhead.

That reframes the remaining work. It is not "remove 20% everywhere"; it is
"stop the heavy views from costing 1.6x the light ones".

### Correction: the August 2026 worker numbers were measured on a dirty SDK

An independent verification of `9ba2614..c5a1ed9` could not reproduce the
24.418 fps this document previously reported. It is wrong, and so is every
cycle count in the tables below that predates this section. The cause is worth
recording because the tooling reports it on every run and it was still missed.

`.psoxide` is a *hydration*, and it is gitignored. The worker's tree had been
hydrated from a local scratch checkout, `/private/tmp/psoxide-quake2-harmonize`
at `e31ea70b`, which is a **descendant of the pinned SDK `5048fbde`**, and then
hand-edited on top. Eight files differed from the pin, five of them in the
guest path: `psx-bsp/collision.rs`, `psx-bsp/resident.rs`,
`psx-engine/classic_affine.rs`, `psx-engine/lib.rs`, `psx-gpu/ot.rs`. The build
tool printed `PSoXide existing hydration stamp: local ... at e31ea70b` every
single run.

Two consequences:

1. **Commit `0c78469` does not contain the work its own title claims.** It is
   called "Cut redundant collision, visibility and packet work", but `.psoxide`
   is gitignored, so the collision part (a 32-bit `plane_contact` that removes
   four `__divdi3` calls per contact) and the packet part (the GT4 pairing
   ladder collapse) never left the untracked hydration. Only the entity broad
   phase and the camera-leaf memo were committed. The GT4 pairing was in
   `5048fbde` already in any case, so it was never a gain between these two
   Quake revisions.
2. **The accepted stack's real gain is the drawable frustum and nothing else.**

Verified on a clean `c5a1ed9` against a clean `5048fbde` checkout, twice,
byte-identical both times:

```text
9ba2614   3,029,846,768 cycles   23.843 fps
c5a1ed9   3,021,278,457 cycles   23.911 fps
change       -8,568,311 (-0.283%)     +0.068
```

Both revisions produce the canonical hashes. **+0.068 fps is inside the 0.122
layout-noise band**, so the committed stack carries no defensible aggregate
gain. The two-tick cadence figures were also stale: 24.690 fps and 59.62% of
presentations in two fields or fewer, not 24.852 and 60.97%.

**The reproducible target to beat is 23.911 fps / 3,021,278,457 cycles.**

Rule that follows: never measure against `.psoxide` without checking the
hydration stamp against `PSOXIDE_REV` first, and never describe SDK work in a
quake-psx commit message, because the commit cannot contain it.

### Fixed: the no-world diagnostic did not remove the world

`renderer-selection-drop-world` had exactly one effect site, inside
`select_frame_faces_blocked_plane_indexed`. That selector needs
`renderer-plane-index-cache`, which `e1m1-no-world-bench` does not enable, so
the feature was dead code in that build and the benchmark silently measured the
every-other-face decimation ceiling instead. `select_frame_faces_blocked` now
applies the same admission rule.

Validated by an independent GPU census rather than by the fps moving, since the
whole point is that the fps moved before while the world was still drawn:

```text
                       accepted stack     no-world
textured quads/frame          125.3            7.4
textured tris/frame           374.5          231.9
GP0 commands/frame            550.7          251.3
```

The residual is the view model, HUD and dynamic entities, which is what this
ceiling is meant to price. Hashes change by construction
(`0xe4a6eb66c384a603` / `0xe773bf072dec7062`).

**The geometry-free ceiling, measured clean:**

```text
baseline    3,021,278,457 cycles    23.911 fps
no-world    2,186,128,663 cycles    33.045 fps
world path    835,149,794 cycles    27.6% of the frame
```

30 fps over this route needs 2,409,200,640 cycles, so it needs 612,077,817
cycles removed: **20.3% of the frame, which is 73% of the entire world surface
path**, while keeping every polygon. That is the size of the problem, and it is
now measured on a clean pinned build rather than inferred. Micro-optimization
cannot reach it; only an architectural change to how world surfaces are
selected, materialized and submitted can.

### Added: a benchmark that actually runs the monsters

Every route regression, and therefore every canonical benchmark, compiles
`update_monsters` and `update_monster_missiles` out. The comment at the `cfg`
says why: the probes want deterministic waypoint movement. The consequence is
that **no canonical benchmark exercised the solid-submodel collision path a
collision change is supposed to make cheaper**, so the broad phase in `0c78469`
was never measured by the number used to justify it.

`route-monsters` compiles the monster think loop back into the E1M1 chain route.
Two things had to change for it to work as a benchmark:

- **The chain probe cannot be reused.** With monsters running the scripted route
  cannot complete: a live body blocks the path, and the probe stops at waypoint
  12 at `(-9, 1047, -230)`. That is not a bug, it is what the route asserts. A
  performance route needs determinism over a fixed window, not completion, so
  `run_monster_route_bench` requires only that two runs agree on timing and both
  hashes.
- **The timing window had to change.** `full_level_render_metrics` brackets
  gameplay between the end of the initial load and the start of the transition
  load, found as the largest gap between `ReadN` commands. With no level change
  there is no closing session and the window collapses.
  `monster_route_render_metrics` opens at the last `ReadN` of the initial load
  and runs to the final presentation.

```text
cargo run --release -- e1m1-monster-route-bench --psoxide <clean 5048fbde>

quake-psx E1M1 monster route: PASS
deterministic_runs=2
monster_updates=enabled
full_level_presentations=3795
full_level_elapsed_bus_cycles=4461374086
full_level_fps_x1000=28802
vram_fnv1a_64=0xcce4c4d5cf9173a6
display_fnv1a_64=0xc12223e502d5df7e
```

That the same route completes without monsters and stalls at waypoint 12 with
them is itself the proof that the monster loop is running and reaching the
collision path.

Read this benchmark for what it is: the player stalls partway, so it is weighted
toward monster AI, movement and submodel traces rather than heavy world views.
Use it for collision and gameplay changes, and the canonical E1M1 route for
renderer changes. Its 28.802 fps is not comparable to the canonical 23.911.

### The frame is memory-bound, not instruction-bound

The instruction attribution above answers *what runs*. It does not answer *what
the frame costs*, and on an R3000A with no data cache those are different
questions. PSoXide's cycle profiler separates them. Over the E1M1 route on the
accepted stack, per frame:

```text
profiled CPU cycles      1,387,178
  issue                    611,438   44.1%   (one per retired instruction)
  RAM load stalls          580,994   41.9%
  I-cache refill stalls    116,604    8.4%
  RAM store stalls          48,816    3.5%
  multiply/divide           23,376    1.7%
  MMIO                       5,950    0.4%
```

**Load stalls very nearly equal instruction issue.** Cutting instructions is at
best half a lever; the other half is cutting loads. Two corrections follow from
measuring this properly.

First, earlier attributions in this document were taken over a window that ran
past the E1M1 route into the E1M2 load. That inflated the whole table by about
a third and put `SectorReader::read_sector` at 7.5% of "gameplay". Restricting
the window to route ticks 658..5985 and 2,136 presentations removes it
completely: it was all disc. The clean gameplay frame is **611,359 retired
instructions**, not 1,303,376. Attribute through the map's `.text.*` section
ranges, not its symbol lines: identical-code folding leaves local functions
unnamed, and symbol-line attribution silently charges their cost to whatever
precedes them. That is what once made `quake_core::train::leg_distance` appear
to cost 5.4% of the frame when the code at that address is the liquid warp.

Second, the vblank spin is far more expensive than its instruction count
suggested, and it is still idle. The two-line spin inside `gpu_end_frame`
retires 108,364 instructions per frame and pays **166,900 of the frame's load
stall cycles** polling a counter in main RAM. Net of it, real work is about
**1,112,000 cycles against the 1,130,089-cycle two-field budget** - the mean
fits by 1.6%, confirming the cadence measurement above by an independent route.

Load stalls net of the spin, per frame:

```text
 54,558  quake::run                     18,549  MovementTrace::trace
 48,337  draw_frame                     15,921  warp_tile_64_prepared
 35,535  materialize_surface            14,548  AliasModelHeader::decode
 35,314  CollisionHull::trace_into      13,789  mark_visible_faces
 30,127  select_frame_faces_blocked     13,190  submit_quake_classic_affine_batch
 22,001  submit_classic_alias_model     10,677  point_leaf_index
```

Nearly every entry sits near one stall per retired instruction, which is what
streaming large arrays through no data cache costs. The exception is
instructive: `submit_quake_classic_affine_batch` is the largest instruction
consumer at 11.92% but only 2.27% of load stalls. It is issue-bound, and its
72,853 instructions are spread almost flat across 439 cache lines - a packet
writer, with no hot loop to attack.

Reproduce with:

```sh
frontend launch --path build-psoxide-e1m1-collision-broad-phase-bench/quake-psx.cue \
  --digital-pad --steps 12000000000 --guest-frames 2136 \
  --ram-load-stall-line-log captures/matched/loadstall.csv \
  --ram-load-stall-line-start-route-tick 658
```

`--ram-load-stall-line-log` was added to PSoXide for this pass. It mirrors the
existing MMIO stall line log: capture the PC before the step, charge the
counter delta to that instruction's 16-byte line.

### Closed: splitting the retained face record

The selection pass is the clearest bandwidth case in the frame. It streams the
entire retained face array every frame, and its 30,127 stall cycles are almost
exactly what reading 774 records of 36 bytes from main RAM costs. Halving the
record should have halved the stall.

`renderer-compact-cell-stream` moves the plane into a parallel array, taking
`VisibleFace` from 36 bytes to 24. On the full accepted stack it measured
**24.371 fps and 2,964,154,309 cycles against 24.371 and 2,964,154,491** - 182
cycles apart over three billion, with both canonical hashes exact.

It changes nothing because the pass reads the plane anyway, for the backside
test. Splitting the record splits the stream; it does not shorten it. The only
way to cut this pass's bandwidth is to admit fewer candidates, and every
affordable narrowing mechanism is already closed above. Do not retry record
packing without first removing a field the selector actually reads.

### Closed: micro-optimization against the load-stall table

The load-stall table above names its own targets, and four of them were tried
directly. All four land inside the +-0.122 fps layout-noise band, and they are
recorded so the table is not mined for them again:

- **Borrow before copying in `collect_pickups`.** Both of its whole-scene scans
  copied the 112-byte `RenderEntity` before testing the one or two bytes that
  reject nearly every entity, twice per tick. Rewritten to reject through a
  borrow: 24.418 fps and 2,958,442,237 cycles against 24.409 and
  2,959,584,601, hashes exact. Kept, because it is a strictly smaller and
  simpler loop, but it is not a measurable win.
- **Memoizing the view-model lookup.** `submit_view_model` linear-scans the
  alias model table every frame and decodes one cooked header per candidate to
  find the weapon. A generation-keyed memo measured 24.414 fps. Reverted: an
  unmeasurable cache is complexity with no payoff.
- **Narrowing the per-face `VisibleFace` copy** in the dispatch loop to the ten
  bytes a shipping build reads. Neutral; LLVM had already narrowed the load.
- **Reordering `PlaneRecords` so the only variant the game constructs is first**,
  saving one failed discriminant compare on each of the two plane distances per
  hull node visit. Neutral, and it is shared code, so it was reverted.

The pattern is consistent and worth stating plainly: at this point every change
small enough to be safe is too small to measure, and the noise band is wider
than any of them.

### The vblank spin is genuinely free

Worth recording because the load-stall table makes it look otherwise. The
two-line spin pays 166,900 RAM load stall cycles per frame polling
`__psx_rt_vblank_count`, which sits in main RAM - 12% of all frame cycles, and
the single largest line in the profile. The tempting conclusion is that this
traffic starves the GPU's DMA reads of the ordering table and could be fixed by
moving the counter into the scratchpad.

It cannot. On the PS1 the DMA controller has bus priority over the CPU: DMA
preempts a spinning CPU, not the other way round. The spin's RAM traffic costs
the CPU stall cycles it was going to spend waiting anyway, and delays nothing.
Do not move the counter to the scratchpad for performance; the scratchpad is
also already carrying the liquid turbulence phase.

### Closed: bounding the sky lattice

The sky lattice *is* the sky: sky brush faces are never drawn, and 240 screen
quads plus 143 view-ray samples are painted into the farthest OT slot every
frame whether the aperture is the whole screen or a doorway. E1M1 typically
selects about eight sky faces, so bounding the lattice to the projected union
box of those faces looked like 3.3% of work.

It is not, on this route. Measured 24.029 fps and 3,006,426,008 cycles against
24.042 and 3,004,712,412: **1.7 million cycles worse**, with both hashes exact.
Exact hashes with a bounded lattice means the bound never excluded a cell, so
E1M1's outdoor start keeps the sky box projecting across the whole viewport and
the eight-corner projection is pure added cost. The mechanism is sound and would
pay on an indoor map; it does not pay here, and the benchmark is here.

### Closed: capping liquid warps per frame

One 64x64 liquid tile is 4,096 gathered texels, about 71,000 cycles, and E1M1
has three liquid textures (`*water0`, `*slime0`, `*teleport`), so a frame that
sees all three would spend 213,000 cycles of a 1,130,089-cycle budget on water
animation. Warping one tile per frame and round-robining the rest measured
24.414 fps with **both hashes exact**, which is the proof that it never fired:
the route never has more than one tile needing a warp in the same frame. There
is no multi-tile spike on this route to remove. The cost is real but flat at
about one tile per frame.

### Closed: packing the liquid resampler's stores

The resampler retires one `sb` per texel, 4,096 byte stores per tile, and the
R3000A write buffer does not merge byte stores to consecutive addresses.
Assembling four texels in a register and issuing one `sw` cuts write
transactions fourfold for about +0.75 instructions per texel. Measured 24.404
fps and 2,960,156,347 cycles against 24.362 and 2,965,296,903: **inside the
noise band**, and the hashes changed, so the rewritten kernel is also not yet
byte-exact. No gain to justify chasing the remaining difference; reverted.

Worth recording from the attempt: the first version put `sll $14, $14, 8`
directly after `lbu $14, 0($15)`, copying the shape of the existing
`lbu`/`sb` pair. That is not the same hazard. A store reads its data register a
pipeline stage later than an ALU instruction reads its operand, so `lbu` into
`sb` is safe where `lbu` into `sll` reads the stale register. Filling the slot
with the column advance fixed that specific fault and the output still differed,
so there is at least one more.

Also established while measuring this: the turbulence resample is genuinely
two-dimensional and does not separate. A row-rotate pass followed by a
column-rotate pass yields `src[(y+T[x])&63][(x + T[(y+T[x])&63]) & 63]`, with
`T` indexed by the shifted row rather than by `y`, and the same failure appears
in the other order. With no data cache the 4,096 source reads are 4,096 bus
transactions whatever the layout, so reordering or swizzling the source tile
buys nothing either.

### Closed: leaf-granular portal flooding

The exact portal graph was cooked into the visibility lump (6 bytes per portal:
BSP plane index plus the 2D bounding rectangle of the opening on that plane, on
the 32-unit grid) with a per-leaf CSR of sides, and a runtime walk was built
that narrows a screen rectangle at every opening and replaces the PVS row
entirely. `tools/portal-runtime-check.rs` proves the cooked graph round-trips
exactly: 3,312 portals and 6,624 sides for E1M1, every reconstructed corner on
its stored plane within 0.89 units, zero plane-class mismatches.

The walk works. It is the cost distribution that kills it:

```text
per sample   p50    p90     p99     max
visits        12    231     718    1435
side tests    73   1798    5889   12178
projections    -    350       -       -
```

The mean over uniformly sampled leaf centres is only 78 visits and 600 tests,
but roughly 1,300 of E1M1's 1,530 leaves are sealed slivers a player never
occupies. The route lives in the open, highly connected leaves, which are the
tail: a guest census counter measured 588 visits, 4,888 side tests and 709
portal projections per frame on the canonical route, matching the host's p95 to
p99. At that rate the walk costs more than the candidate reduction saves.

Three partitions were tried to cut the churn and all made both quality and cost
worse, because merging leaves admits whole cells and therefore reaches further:

```text
partition        cells  doorways  candidates mean/p90   tests mean/p90
leaf              1531      6624       196.3 /   569     600.2 /  1798
face cap 24        821      4970       275.8 /   773     880.6 /  2803
face cap 96        600      3900       439.9 /  1176    1414.4 /  4802
area >= 16384     1120      4758      1368.2 /  3372    1833.7 /  5745
area >= 4096       536       968      3758.2 /  5396     560.2 /  1175
```

Unbounded area merging chains distant rooms into one component through wide
corridors and admits most of the map. Face-capped growth avoids that but every
merge still increases reach. Merging leaves with identical PVS rows, which is
visibility-equivalent by construction, only collapses 1,531 leaves into 1,120.

The structural reason is in the decomp itself: Quake II's cells were authored by
hand, "making the runtime efficient but content production expensive". It tests
7.17 doorways per present and admits 1.40. A Quake BSP offers 4.3 portals per
leaf across 1,530 leaves and no room structure to recover. Do not reopen leaf
portal admission without authored or offline-clustered rooms that bound both
doorway count and cell reach.

### Closed: coplanar face merging

`tools/face-merge-census.rs` applies qbsp's own `TryMerge` rule (same plane and
side, same texture information, same light styles, one shared edge, convex
after joining, collinear boundary vertices retained so no T-junction appears)
to every Episode 1 map. It merges **zero** faces on all nine maps: id's qbsp
already runs its merge pass after splitting, so the remaining fragmentation is
exactly the part that cannot be rejoined convexly.

### Where the Quake II gap actually is

Normalising both engines to their own surviving faces closes the question:

```text
                        Quake II PSX     quake-psx
work per frame            120,516 instr   516,235 instr
static world renderer      33,826          ~213,000
surviving source faces         56.46          253.8
cost per surviving face       599 instr      ~874 instr
cycles per selected face    ~1,360          ~1,479
textured primitives           510 quads     1,156
hardware triangles          ~1,020         ~1,682
resolution                  512x240         320x240
```

**quake-psx costs essentially the same per surviving face as Quake II.** It is
not slower per unit of work. It draws 4.5 times as many faces, producing 1.65
times the hardware triangles into 0.39 times the pixel area, which is 4.2 times
the geometric density. Quake II's world is an authored mesh of large quads that
subdivide at runtime, 14.9 quads per brush and 15.2 brushes per present; a Quake
BSP hands the renderer 5,516 already minimal convex fragments.

The remaining transferable mechanism is therefore not visibility and not
per-face efficiency. It is Quake II's resident `POLY_GT4` templates: 60.44% of
quake-psx's packet bytes are invariant UV, colour, CLUT, TPAGE and opcode, which
matches Quake II's 32-of-52-byte split almost exactly.

### Rejected: run-decomposed liquid resampler

`warp_tile_64_runs` replaced the per-texel gather with the phase's
constant-displacement column runs. Columns sharing a displacement read one
contiguous span of a single source row, so the per-texel index arithmetic and
the masked column advance disappear. A `quake-core` test proved the output
byte-identical to the dense resampler over all 128 phases, and a
register-level simulation confirmed the shipping MIPS kernel and the portable
host kernel already agree.

```text
cycles: 3,039,557,989
fps:    23.767   (baseline 23.803)
hashes: changed
```

No gain, and the guest image changed for a cause not localized. The arithmetic
explains the null result: the turbulence window averages 3.84 columns per run,
so a tile needs 64 rows x 18 runs = 1,152 run setups for 4,096 texels. Each
setup recomputes the source row, the source column and the wrap split, which
costs about as much as the per-texel arithmetic it removes. Run decomposition
cannot beat the dense gather at this run length; do not retry it without a
representation that amortizes the per-row setup.

### What is actually left between quake-psx and Quake II PSX

Two of Quake II's measured advantages do not transfer, and the reason is
structural rather than a missing optimization.

Its cooked cell streams narrow the candidate set offline. Reproducing that on a
Quake BSP means a leaf portal graph, and the graph does not fit the resident
arena at any budget that keeps its value.

Its resident `POLY_GT4` templates supply 84.47% of textured quads, patched with
XY and DMA linkage instead of rebuilt. That works because Quake II's scene is
instanced: templates are per static model, each capped below 64 polygons, so a
few hundred models cover the level. A Quake BSP world has no instancing. E1M1
alone cooks 5,890 render faces, so per-face templates would need about 212 KB
against a 14 KB margin. The previously rejected 16-slot resident cache is the
same wall seen from the other side: 30,674 hits against 587,881 packets, a 5%
hit rate, while hot text grew from 7,064 to 11,064 bytes.

What remains transferable is bounded and already partly done: small sequential
hot loops, stable source order, low texture-window churn, and rejecting dynamic
actors before animation. None of those is worth the roughly 30% of frame work a
stable 30 fps still needs.

The measured priority order for the remaining work, by share of gameplay CPU:

1. World face path, about 35%. Face-proportional work is about 24% of the
   frame and a 50% face cut is worth +3.624 fps, but the affordable
   candidate-narrowing mechanisms are now closed. What is left is cost per
   selected face, not fewer faces.
2. Collision and physics, 15.7%. `trace_into` alone is 7.36% and is recursive;
   stack load stalls are already 8.52% of all cycles. An explicit-stack hull
   trace is untried. Two earlier micro-attempts (compact 12-byte planes,
   shared single descent) were exact but measured 23.753 and 23.785 fps.
3. Instruction cache, 8.80% of cycles and 125,536 stall cycles per frame over
   25,660 refill events. `submit_quake_classic_affine_batch` is 7,064 bytes
   against a 4 KB direct-mapped cache. Every splitting variant tried so far
   (`renderer-quake-level0-run` 22.943, the compact kernel family) lost more
   than it saved.
4. Game logic, 7.6%, and liquid, 5.6%.

A 20% work cut converts roughly 95% of frames to two fields; 30% is needed
before the worst window fits. Nothing measured so far offers a single change of
that size.

## Visual checks

The fixed E1M1 camera is stored in
`tools/visual-parity-cameras.json`. Run:

```sh
cargo run --release -- visual-parity-regress \
  --psoxide ../PSoXide/target/release/frontend
```

The check repeats the capture and compares:

- display size;
- world, HUD and final-display hashes;
- submitted command counts;
- texture-window state;
- draw-packet overflow;
- the visual probe written by the PS1 executable.

The reference values live in the runner so code and expected output change
together during a reviewed visual update. See [VISUAL_PARITY.md](VISUAL_PARITY.md)
for the acceptance rules.

## Known limitations

- Very large or steep polygons can still show affine distortion.
- Fixed-point midpoint rounding can expose a seam on unusual surfaces.
- Translucent water intentionally opens only a limited additional visibility
  set to keep memory and packet use predictable.
- The 30 fps target has not yet been demonstrated across the whole episode on
  original hardware.

## Profiling

A fixed-tick E1M1 route is available for comparing renderer changes without
allowing simulation speed to alter the path:

```sh
cargo run --release -- e1m1-chain-bench --psoxide ../PSoXide-quake
```

The canonical accepted-stack benchmark is:

```sh
cargo run --locked --release -- e1m1-gpu-polygon-scratch-liquid-bench \
  --psoxide /path/to/PSoXide/target/run-fast/frontend
```

Two diagnostics answer "how much would this be worth" before an optimization is
written. Both change the image and must never be shipped:

```sh
cargo run --locked --release -- e1m1-selection-decimate-bench --psoxide ...
cargo run --locked --release -- portal-census e1m1
```

`e1m1-selection-decimate-bench` halves the selected world faces and prices the
face-proportional part of the frame. `portal-census` rebuilds the exact BSP
portal graph host-side and reports admission ceilings, merge and gate sweeps,
and the resident-arena cost of every sidecar layout.

Because presentation is quantized to whole NTSC fields, fps is a coarse
readout. Prefer the two-field share and the work-per-frame distribution: take a
`--pc-sample-window-log`, treat the four instructions of the `wait_vblank`
counter spin as idle, and subtract that share from each window's bus-cycle
span. `47.45%` of frames present in two fields or fewer at the current
baseline.

For a shipping-cadence result, use:

```sh
cargo run --release -- e1m1-chain-regress --psoxide ../PSoXide-quake
```

Compare command counts, GPU estimates, displayed frames and route progress.
Screenshots should also be checked before accepting a speed improvement.
