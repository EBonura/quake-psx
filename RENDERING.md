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

The latest comparable sustained E1M1 route measured 19.008 fps in the emulator.
The goal remains 30 fps. Emulator timing is useful for comparing revisions, but
only a real console can provide the final result.

## Visual checks

The fixed E1M1 camera is stored in
`tools/visual-parity-cameras.json`. Run:

```sh
cargo run --release -- visual-parity-regress --psoxide ../PSoXide-quake
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
