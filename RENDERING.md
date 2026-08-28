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

The latest accepted feature-gated selector build measured 22.587 fps on the
complete fixed-step E1M1 route in PSoXide, up from the 22.128 fps exact-key
selection-cache baseline and the original 21.857 fps renderer baseline.
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
3,054,410,221 bus cycles at 23.651 fps. Both complete runs present 2,134 frames
and retain VRAM hash `0x09a7f019bb9a5e7c` and display hash
`0x9bac66f3bec0e66b`. This saves 29,133,270 cycles and gains 0.223 fps (0.95%)
against the 23.428 capture, or 28,561,497 cycles and 0.219 fps against the
previous documented 23.432 leader.

The cumulative exact A/B sequence was:

| Stack addition | Bus cycles | Fixed-step fps | Increment |
| --- | ---: | ---: | ---: |
| documented cell-policy leader | 3,082,971,718 | 23.432 | baseline |
| GTE near classification | 3,078,401,738 | 23.467 | +0.035 |
| retained liquid policy | 3,068,691,494 | 23.541 | +0.074 |
| Quake-specialized submit kernel | 3,057,266,396 | 23.629 | +0.088 |
| liquid visibility fast rejection | 3,054,981,456 | 23.647 | +0.018 |
| inline baked-corner materializer | 3,054,410,221 | 23.651 | +0.004 |

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
cargo run --release -- e1m1-gpu-polygon-leader-bench \
  --psoxide ../PSoXide/target/release/frontend
cargo run --release -- gpu-polygon-leader-disc \
  --psoxide ../PSoXide/target/release/frontend
cargo run --release -- e1m2-e1m3-leader-route-regress \
  --psoxide ../PSoXide/target/release/frontend
```

The playable image passes the shipping boot gate with 62,240 bytes of heap
free. Two complete authored E1M2/E1M3 runs also agree exactly: 6,023 gameplay
frames, all `0x1fffffff` mechanism bits, 86 target edges, two map transitions
into E1M4, VRAM hash `0xb43b84dba8258f74`, and display hash
`0xdd1e0c9c06d994cf`.

The research feature gates in this branch use matching local PSoXide engine
and GTE SDK changes inside the deliberately ignored `.psoxide` hydration.
In particular, `renderer-gte-near-classification` needs the auxiliary AABB row
API. These changes are not part of the shipping/default build and must not be
inferred to exist in the pinned public SDK until the PSoXide-side API is ported
and reviewed separately.

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

For a shipping-cadence result, use:

```sh
cargo run --release -- e1m1-chain-regress --psoxide ../PSoXide-quake
```

Compare command counts, GPU estimates, displayed frames and route progress.
Screenshots should also be checked before accepting a speed improvement.
