# Quake II-style page-local liquid research

Quake II PSX's recovered renderer resolves material state before primitive
emission. Quake-PSX's turbulent surfaces instead emit a scoped GP0(E2)
texture-window selector and reset around every hardware triangle. This branch
tests moving the repeat contract into cooked texture storage so ordinary
compact packets can be used.

## 64-texel cell experiment (rejected)

The cooker split liquid polygons at 64-texel boundaries, stored page-relative
UVs, and gave each double-buffered 64x64 warped tile a repeated right/bottom
edge. The runtime could then batch eligible opaque liquid pieces without a
texture window. All host format and warp tests passed, every Episode 1 map
cooked, the guest MIPS border writer linked, and the two deterministic E1M1
routes completed.

Measured result against the accepted GPU-polygon/cell-policy stack:

- baseline: 23.432 fps
- candidate: 22.864 fps (3,159,518,154 gameplay bus cycles)
- delta: -2.42%
- candidate hashes: VRAM `0xee539f818d66fce8`, display
  `0x1e3e9d94124d222a` (not canonical)
- E1M1 liquid geometry: 678 faces / 2,002 fan roots became 1,212 faces /
  3,162 roots
- resident high water: 865,958 bytes became 887,062 bytes

The packet-state saving did not repay the additional selection, projection,
and raster work. The denser triangulation also changed affine interpolation,
so this is neither faster nor visual-neutral. Do not ship the 64-cell split.

## 128-texel repeated-atlas experiment (rejected)

The constrained successor kept the original face geometry and promoted only
the highest-use liquid material in each map. Its single 130x129 physical atlas
allocation contained four copies of the warped 64x64 tile plus the inclusive
right and bottom samples. After the prior frame's GPU work became idle, the
runtime uploaded one 64x64 warp and used four GP0(80h) VRAM-to-VRAM copies to
populate the repeated allocation. Promoted opaque faces retained their
original 0..128 UVs and joined ordinary compact batches without a texture
window; other liquid materials kept the established double-buffered path.

This version preserved the original E1M1 counts (678 liquid faces and 2,002
fan roots), the 865,958-byte Episode 1 resident high-water mark, and the exact
canonical display hash `0x9bac66f3bec0e66b`. It promoted 522 E1M1 liquid faces
and 1,568 fan roots. The VRAM hash intentionally changed to
`0x55e1c3c378a45f35` because the physical atlas contains repeated pixels that
are outside the displayed image.

Measured results against the 23.432 fps accepted stack:

- repeated atlas: 23.423 fps (3,084,114,394 gameplay bus cycles)
- repeated atlas plus a baked-light/page-local materializer: 23.428 fps
  (3,083,543,491 gameplay bus cycles)
- texture-window changes per presentation: 110.73 became 6.91 (-93.8%)
- total texture-window commands per presentation: 124.61 became 20.79
- the four repeat copies added about 5,625 modeled GPU cycles per presentation

Both timing deltas are inside the documented 0.122 fps layout-noise band. The
specialized materializer was likewise neutral. This rejects GP0(E2) emission
as a material CPU bottleneck on the accepted route: removing almost all of its
state changes does not move the complete-frame result, while the repeat-copy
work consumes the corresponding GPU saving. Do not publish the supporting
PSoXide prototype or ship this format. The next performance work should target
the profiled projection/subdivision kernel instead.
