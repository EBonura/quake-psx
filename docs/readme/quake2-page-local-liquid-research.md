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

## Next experiment

Keep the original 128-texel liquid subdivision and populate a physically
repeated 128x128 allocation from the one 64x64 warped tile using GPU VRAM-copy
commands. This preserves geometry and moves repeat work out of the per-polygon
emission loop, matching the useful Quake II architectural lesson without the
failed geometry expansion.
