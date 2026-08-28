# Quake II-style self-contained face selection: rejected

This branch preserves three exact/code-shape controls derived from Quake II
PSX's compact monotonic scene stream. It is not a measured shipping gain and
must not be merged into the accepted renderer.

## Hypothesis

The accepted cell-policy selector walks a 36-byte `VisibleFace`, then follows
its material index into a separate texture table to rediscover liquid policy.
It also reloads the draw-only face flags for backside policy. Quake II instead
feeds small, sequential, self-contained records to non-calling MIPS kernels.

The experiment folded four cold cell decisions into unused high bits of the
retained plane byte:

- source backside;
- liquid material;
- current water-blend support plane;
- cell-invariant front-facing.

The exact per-frame policy remained block AABB, water override or facing,
per-face AABB, stable source order, and the original output markers.

## Variant 1: policy bits in the existing 36-byte record

The first form retained the accepted record layout. On PVS changes it cached
backside, liquid and invariant-front bits; the selector stopped loading the
material table and stopped reading the draw flags. It still compared the
retained source plane index against the active water plane.

Two complete PSoXide-only canonical routes passed:

- 2,086 route frames and 2,134 full-level presentations;
- 3,070,404,483 elapsed bus cycles;
- 23.528 fps;
- VRAM FNV-1a `0x09a7f019bb9a5e7c`;
- display FNV-1a `0x9bac66f3bec0e66b`.

The linked selector fell from the accepted `0x818` bytes to `0x7e8` bytes.
The current-frontend reference is 23.410 fps and the historical accepted
capture is 23.432 fps. The apparent +0.118/+0.096 fps lies at or inside the
established +/-0.122 fps layout-noise band. This is code-shape evidence, not
an accepted improvement.

The preserved summary is
`captures/e1m1-gpu-polygon-self-contained-select-bench/summary.txt`.

## Variant 2: parallel 24-byte hot stream

A second form copied only plane plus AABB into one 24-byte candidate per
retained face. Draw metadata remained in the authoritative list. It was
stopped at the mandatory link-map preflight: the selector grew to `0xa04`
bytes. Keeping independently-strided candidate and draw arrays recreated the
split-stream register/address-generation cost that the experiment was meant
to remove.

## Variant 3: one packed 32-byte record

The final form removed the parallel allocation and packed the minimal draw
identity behind plane plus bounds:

- 12-byte compact plane and cached selection bits;
- 14-byte source index and exact AABB;
- 6-byte draw identity (first corner, 7-bit material, baked UV/light bits,
  six-bit corner count, and two light styles).

The record is exactly 32 bytes and the selector again uses one monotonic
pointer. The guest compiles and the packing assertions pass, but the linked
selector is still `0x9fc` bytes. The route was intentionally stopped before
timing because this fails the same preflight by 484 bytes relative to the
accepted selector. The additional cold water-plane annotation is `0x13c` and
the cell compiler is `0x4a4`.

## Conclusion

Removing a material-table chase is correct but too small by itself. LLVM's
record-stride and aggregate-load choices dominate the intuitive byte count:
24- and 32-byte redesigns both generate substantially more MIPS text than the
ordinary 36-byte record, while the only smaller form improves cadence by no
more than noise.

Do not tune bit assignments or record padding further. A future scene stream
must replace a larger consumer boundary—selection plus draw materialization,
or a full retained packet range—not add or reshape a parallel selector array.
The next independent target should be the measured ordinary-world command and
texture-window traffic, preserving final OT order.
