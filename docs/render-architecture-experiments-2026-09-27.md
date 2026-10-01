# Quake / PS1 rendering architecture experiments

Date: 2026-09-27. Scope: the research and experiments prompted by modern
geometry compression, NVFP4, Nanite, and alternatives to a Quake BSP renderer.

**Decision: reject the tested simplification approach.** User review of the
comparisons found clearly visible gaps exposing the background, poorer
tessellation and increased texture warping. The fastest render-mesh prototype
improves emulated E1M1 FPS by 6.42%, but fails the required visual quality. This
is a failed optimization experiment, not a recommended performance mode.
The tested runtime
LOD selectors, projection caches and retained packet caches regress
performance. Modern patch simplification produces useful offline reductions,
but has not been integrated into a complete PS1 HLOD renderer. No experimental
runtime or asset variant is enabled by default.

[Interactive screenshot viewer](render-architecture-2026-09-27/compare.html)
• [Exact measurements](render-architecture-2026-09-27/measurements.json)
• [Screenshot provenance](render-architecture-2026-09-27/screenshot-provenance.json)

## What the research supports

A BSP gameplay/collision world does not require the renderer to consume its
original face representation. A separate render-world compiler can build
visibility regions, geometry batches, and coarser replacement meshes. The
research supports investigating that separation; it does not supply a measured
speed multiplier for the current Quake port.

| Precedent | Demonstrated approach | Relevance and constraint |
|---|---|---|
| Spyro on PS1 | Separate detailed textured nearby geometry and simpler untextured distant geometry | A shipped PS1 precedent for multiple world representations and a free camera. It uses visual approximation, not pixel identity. |
| Crash Bandicoot on PS1 | Offline visibility/sort computation and streaming | Moves expensive work into the content build. Its controlled camera allows precomputation that cannot be directly assumed for a freely moving FPS. |
| SlaveDriver | Sector/doorway visibility with projected screen windows | Its released source provides a concrete alternative to fine BSP-leaf traversal. The available backend is Saturn code, not a PS1 drop-in. |
| Batched Multi Triangulation (2005) | Precomputed optimized patches replace costly triangle-level LOD decisions | Demonstrates the importance of decision granularity; the paper's hardware/results are desktop, not PS1. |
| meshoptimizer | Attribute-aware simplification and cluster-LOD construction | Modern code usable in a host asset compiler. Guest format, visibility, ordering and seam handling still require PS1-specific engineering. |

Primary sources: [Spyro developer interview](https://kirstenvanschreven.com/wp-content/uploads/2021/10/Behind-the-scenes-of-Spyro-The-Dragon-gamesTM.pdf),
[Andy Gavin's Crash account](https://all-things-andy-gavin.com/2011/02/04/making-crash-bandicoot-part-3/),
[released SlaveDriver source](https://github.com/Lobotomy-Software/SlaveDriver-Engine),
[Batched Multi Triangulation paper](https://vcg.isti.cnr.it/Publications/2005/CGGMPS05/BatchedMT_Vis05.pdf),
[meshoptimizer cluster-LOD implementation](https://github.com/zeux/meshoptimizer/blob/9e1f07b159d3cb777f1c67ed31fc11fd117986f4/demo/clusterlod.h).

The existing local Quake II PSX comparison also motivates changing the content
representation: it records similar cycles per surviving face, with substantially
fewer surviving faces in the sampled Quake II scene. Those are different
workloads, not a controlled engine speed comparison. See
[the renderer investigation](../RENDERING.md#where-the-quake-ii-gap-actually-is).

## Measurement contract

- Quake revision: `16e27a9ee905edf27fbc5770661af063f0aeede9`.
- Frontend SHA-256:
  `8e3406169cb0148cb7de21a14556fd4c4a5b18bcedf0fbf6f2553dad824aef69`.
- Fixed three-tick E1M1 simulation route, 3,800 guest markers including the
  transition and subsequent E1M2 residence. FPS excludes loading and E1M2.
- Profile comparisons use common E1M1 guest frames 6–1557 (1,552 frames).
- Gameplay is checked independently through the complete 136-byte probe:
  60 waypoints, mechanism mask `0x7fff`, 16 target edges and E1M2 transition.
- All figures are emulator measurements. No original PlayStation test was run.
- Renderer construction (`RENDER`) and `RENDER + OT_SUBMIT` are different
  metrics. The cache experiments can move work between those stages; do not
  mix their reported cycle percentages.

## Experiment ledger

All FPS changes below are relative to the preserved 35.189384 FPS control.

| Experiment | E1M1 FPS | Change | Outcome |
|---|---:|---:|---|
| Original | 35.189 | — | Control |
| Runtime subdivision selector, strict | 34.608 | −1.65% | Rejected: selection overhead exceeds savings |
| Runtime subdivision selector, relaxed | 34.197 | −2.82% | Rejected: same issue |
| Cooked position layout, original renderer | 35.176 | −0.04% | Layout control; no useful gain |
| Project complete cooked groups | 32.180 | −8.55% | Rejected: excess transforms and bookkeeping |
| Project selected group positions | 31.869 | −9.44% | Rejected despite 27.18% fewer eligible root transforms |
| Native packet batches, v1 | 32.857 | −6.63% | Rejected; capacity issue also found in the 32-slot family |
| Native packet batches, v2 | 32.857 | −6.63% | Rejected; three overflow-protection frames |
| Native packet batches, v3 | 33.212 | −5.62% | Rejected; safe tested capacity, still slower |
| Offline remesh, small tolerance | 36.184 | +2.83% | Rejected approach: visual degradation |
| Offline remesh, wider tolerance | 36.398 | +3.43% | Rejected approach: visual degradation |
| Offline remesh, aggressive | 37.448 | +6.42% | Rejected: background leaks and texture warping |
| Offline remesh, junctions preserved | 35.203 | +0.04% | Control only; no meaningful speed gain |

### Runtime subdivision selection

Uses the existing zero/one/two-level affine subdivision hierarchy. Strict
selection allows a projected root of at most 32×32 pixels and a continuous
affine interpolation bound of 0.25 texel; relaxed uses 64×64 and 1 texel.
Near, saturated and varying-light roots retain the reference path.

Textured polygon submissions fall only 0.25% and 0.47%, while renderer
construction rises 4.67% and 6.88%. Both variants match the existing fixed-camera
image exactly. This rejects those selectors; it did not test replacement
geometry, a cooked cluster hierarchy or streaming.

### Cooked shared-position groups

Groups hold at most 31 positions plus a metadata slot. One runtime policy
projects complete groups; another first determines which positions selected
faces need. The selected policy removes 27.18% of eligible root transforms but
increases renderer construction cost 24.82%. E1M1 resident payload grows
64,548 bytes. Applying that layout to every map would overflow the existing
arena on several maps. Both projection policies match the fixed-camera image.

### Retained native packet batches

The prototypes retain prepared GTE words and GT3/GT4 templates, patching
coordinates/linkage when reusable. They use runtime cold preparation from
existing assets, not an offline native-stream cooker. V3 reduces capacity to
16 slots per display pool and moves its patch kernel to a proven scratchpad
stack. Its combined rendering/OT cost still rises 14.64%.

Added RAM reads, stack traffic and instruction-cache pressure outweigh saved
packet writes. A 1 KiB slot is not a universal polygon bound: a 39-corner fan
can require 1,480 bytes when unpaired. Fallback remains necessary. V3 passes
the tested capacity and fixed-camera checks; it is rejected for performance.

### Offline render-world remeshing

This experiment changes the asset representation, using a byte-identical
413,696-byte guest executable for every timed variant. It joins compatible
coplanar polygons and optionally removes collinear boundary corners. It
checks convexity, exact oriented area and accumulated attribute samples.
Merged faces are admitted from the union of their original leaf marks.

Changes are limited to ordinary static baked-lit world surfaces. BSP planes,
nodes, collision hulls, PVS bytes, entities, textures, sounds, leaf lighting and
dynamic brush geometry retain their original content. Only render arrays and
their references/index are rebuilt.

| Remesh | UV/light tolerance | Faces | Corners | Potential fan triangles | Textured submissions over route |
|---|---|---:|---:|---:|---:|
| Original | — | 5,890 | 28,330 | 16,550 | 1,692,618 |
| Small | 1 / 8 | 5,846 | 26,242 | 14,550 | 1,499,236 (−11.43%) |
| Wider | 4 / 16 | 5,819 | 25,638 | 14,000 | 1,446,658 (−14.53%) |
| Aggressive | 255 / 255 | 5,203 | 22,527 | 12,121 | 1,314,100 (−22.36%) |
| Junctions preserved | 1 / 8, protected corners | 5,846 | 28,242 | 16,550 | 1,694,344 (+0.10%) |

Attribute tolerances are cooked UV/light units, not screen-space error bounds.
Potential fan triangles count the whole asset before visibility/subdivision.
GPU submission totals count textured triangle and quad commands, not equivalent
hardware triangles. Tiny submission differences can include presentation-window
boundary attribution.

The aggressive PSB is 57,220 bytes smaller. Its combined renderer/OT cost falls
11.76%. Original and the three faster variants reproduce the same FPS and
gameplay probes under both default and experimental FIFO DMA models. Protecting
all positions that are non-collinear corners elsewhere in the world removes
all collinear simplification gains in this implementation.

## Before / after screenshots

These comparisons document the rejected experiments. Their FPS labels record
measurements, not an endorsement of the resulting image quality.

These are the actual fixed-camera emulator captures, converted losslessly to
PNG. The sheets enlarge them by integer nearest-neighbor scaling. There is no
color adjustment, smoothing, scene reconstruction or generated imagery.

All use camera `owner-e1m1-2026-08-13`, origin Q12
`[888798, 3824884, -728959]`, angles `[43, 1088, 0]`, frozen light phase and a
fixed simulation step. Capture stops at 180 guest markers; each probe reports
176 render observations and zero packet-overflow/reset failures. FPS labels
describe the separate full-route benchmark, not the still image.

### Small-tolerance prototype: +2.83% FPS

![Original and small-tolerance remesh](render-architecture-2026-09-27/before-after-approximate.png)

5,187 of 76,800 pixels change. This is not a pixel-preserving optimization.

### Wider-tolerance prototype: +3.43% FPS

![Original and wider-tolerance remesh](render-architecture-2026-09-27/before-after-moderate.png)

16,400 pixels change. Looser interpolation tolerances increase the visual cost.

### Aggressive prototype: +6.42% FPS

![Original and aggressive remesh](render-architecture-2026-09-27/before-after-geometry.png)

41,500 pixels change. The nearby right wall and floor clearly change texture
mapping; thin seams are visible in this fixed view.

![Right-wall detail](render-architecture-2026-09-27/detail-wall.png)

![Floor detail](render-architecture-2026-09-27/detail-floor.png)

### Junction-preserving prototype: +0.04% FPS

![Original and junction-preserving remesh](render-architecture-2026-09-27/before-after-stitched.png)

117 pixels change; the measured FPS gain is negligible. Changed-pixel counts
are evidence of different output, not perceptual quality scores. A single
view does not establish quality across the whole level.

The previous runtime LOD and shared-position variants, and native v1/v3,
reported zero changed pixels at this same fixture. Repeating visually identical
images would not demonstrate their cost; the benchmark table records it.

Moving-route images are retained in the local artifact bundle as qualitative
evidence only. Matching gameplay probes does not guarantee the displayed front
buffer contains the same frame age. Disabled camera telemetry fields were not
used to claim image alignment.

## Modern patch simplification: host-only evidence

Upstream meshoptimizer revision:
`9e1f07b159d3cb777f1c67ed31fc11fd117986f4`.

The probe triangulates complete polygon boundaries, groups 3,589 eligible
ordinary world faces by material/flags/styles and spatial region, and calls
`meshopt_simplifyWithAttributes`. Borders are locked; permissive attribute-seam
collapse is enabled; the target is 50% of input triangles. All eligible faces
triangulate successfully, producing 10,189 nondegenerate triangles.

Representative results at combined error setting 1, UV weight `1/scale` and
light weight `0.1/scale`:

| Patch scope | Groups | Triangles after | Reduction |
|---|---:|---:|---:|
| 256-unit cells plus material | 991 | 9,863 | 3.20% |
| 1,024-unit cells plus material | 322 | 9,553 | 6.24% |
| Whole-map material groups | 84 | 8,983 | 11.84% |

Whole-map material groups reach 8,403 triangles at error setting 4 and 7,987
at 16. These settings govern a combined quadric metric, not a strict vertex
displacement or pixel bound. Larger patches reduce locked boundaries, but a
whole-map material group is unsuitable as a visibility cluster. It could admit
far more hidden geometry.

This is an offline feasibility result. No simplified mesh from this probe was
exported to a PS1 meshlet format, visually validated, or benchmarked in the
guest. No runtime FPS claim follows from these counts. The normalized 36-case
sweep is preserved in the local render-mesh bundle.

## Corrections and conclusions

### Visual acceptance and direction

User review rejects obtaining performance by removing tessellation that keeps
the PS1's affine-textured surfaces visually stable. Background leaks are
rendering defects, not an acceptable LOD tradeoff. Passing convexity, oriented
area, gameplay probes and packet-capacity checks did not establish rasterized
surface continuity or acceptable texture mapping.

Removing a collinear corner can preserve a polygon's mathematical area while
breaking shared edge tessellation; projection rounding can then expose cracks.
Larger affine-textured primitives also change interpolation and increase
warping. These mechanisms are consistent with the observed failures; the exact
cause of each visible gap has not been isolated. The junction-preserving
control recovers no useful performance gain.

Future architecture experiments must retain surface coverage and the existing
affine-tessellation quality, including during camera movement and near-plane
clipping. No new background leaks, increased warping, or visible LOD popping
are acceptable. Start comparisons with the original geometry and subdivision
policy, testing conservative visibility rejection and reduced traversal or
submission overhead. Those are candidate directions, not demonstrated gains.
Any change to geometry must pass matched-camera visual checks and moving-view
inspection before its FPS improvement can count as a usable result.

### Historical corrections

The old zero-merge conclusion was caused by a reversed BSP29 winding test in
`tools/face-merge-census.rs`. The corrected census finds 916 E1M1 source-face
joins (16.6%) and 7,119 across Episode 1 (16.8%). It retains boundary junctions,
so fan triangle count does not fall. Source geometric compatibility does not
prove cooked UV/light compatibility; the small-tolerance asset accepts only
44 joins. The tool now has convex/concave regression tests.

The historical Quake II comparison also had a pixel-area arithmetic error:
320×240 is 0.625 of 512×240, not 0.39. For the listed 1,682 versus 1,020 hardware
triangles, density per pixel is about 2.64×, not 4.2×. That sentence is corrected
in `RENDERING.md`; the recorded raw measurements are unchanged.

These tests support reducing total submitted work and moving preparation into
the host compiler. They also show why fewer transforms or triangles alone are
insufficient: traversal, memory traffic, packet construction, ordering, surface
attributes and connectivity all matter. They do not establish that BSP or PS1
architecture has no further headroom.

A future full HLOD experiment still needs a PS1-native patch format, bounded
visibility regions, crack handling, appearance/error validation and runtime
selection. No NVFP4 arithmetic, full Nanite port, geometry-streaming hierarchy,
authored room replacement, or complete alternative engine was implemented here.

## Artifact and reproduction index

Detailed local bundles are under `~/Documents/`:

| Directory | Contents |
|---|---|
| `PSoXide-nanite-quake-2026-09-27` | Runtime selectors, patches, repeated benchmarks and fixed-camera captures; report now marks the census erratum |
| `PSoXide-shared-vertices-quake-2026-09-27` | Cooked grouping prototypes, counters, memory census, restoration checks |
| `PSoXide-native-stream-quake-2026-09-27` | Packet-cache versions, capacity diagnostics, FIFO checks, patches |
| `PSoXide-engine-architecture-research-2026-09-27` | Inspected SlaveDriver source excerpts |
| `PSoXide-render-mesh-quake-2026-09-27` | Asset remesher, playable BIN/CUEs, raw captures, meshoptimizer source/probes, report |

Each experimental implementation remains in its bundle. The repository keeps
this synthesis, selected measurements, screenshot exports/generator and the
census correction. Original runtime source and game assets remain unchanged.

Regenerate the screenshot deliverables from the preserved capture bundle:

```sh
python3 -B docs/render-architecture-2026-09-27/make_comparisons.py \
  "$HOME/Documents/PSoXide-render-mesh-quake-2026-09-27"
```

The viewer embeds all five PNGs and works offline when opened directly.
`screenshot-provenance.json` records source paths within the bundle, camera,
image dimensions, source hashes and raw RGB hashes. PNG roundtrips and recorded
changed-pixel counts are checked during generation. `measurements.json` keeps
source-result hashes and separates render-stage definitions between experiments.

To rerun an asset-only guest experiment from its local bundle:

```sh
python3 -B remesh.py baseline/e1m1.psb approximate/e1m1.psb \
  --mode combined --uv-tolerance 1 --light-tolerance 8
python3 -B pack.py approximate
python3 -B capture.py approximate
python3 -B capture.py approximate --fifo
python3 -B analyze.py
```

Full reproduction commands and limitations for the other experiments are in
their individual `REPORT.md` files. The experiments passed the host and guest
checks recorded there; this documentation/gallery pass does not rerun the
performance experiments or change their measurements.
