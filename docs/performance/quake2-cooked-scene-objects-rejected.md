# Quake II-style cooked scene objects: rejected

This branch is a preserved, exact-output experiment. It is not a performance
improvement and must not be merged into the accepted Quake-PSX renderer.

## Hypothesis

Quake II PSX cooks persistent render objects and submits compact retained
topology. Quake-PSX already performs a 16-face block AABB test before its exact
per-face policy. This experiment tested whether a cooker-authored intermediate
object hierarchy could cheaply reject several admitted faces at once.

The cooker grouped faces deterministically when they shared a complete edge,
plane, and material. A group was capped at 32 faces and 255 unique positions.
Groups with fewer than three faces were encoded as unclustered. Source face
order was never changed.

## Disk and memory format

The optional `QSO1` sidecar is stored immediately before the existing `QLB1`
leaf-bounds suffix in cooked visibility data:

- one little-endian `u16` object ID per face;
- `u16::MAX` for unclustered faces;
- an eight-byte footer containing magic, face count, and object count.

This costs `2 * face_count + 8` bytes per map. Exact cooked totals for the
shareware episode were:

- persistent maps: 14,695,648 bytes;
- persistent maps plus global sounds: 14,855,066 bytes;
- largest resident map: 877,098 bytes in the unchanged 880,000-byte arena;
- worst-case arena margin: 2,902 bytes.

The resident layout therefore still fit, but left an uncomfortably small
margin.

## Cooked topology census

| Map | Clustered faces | Objects | Faces/object | Unclustered |
| --- | ---: | ---: | ---: | ---: |
| start | 2,972 | 429 | 6.93 | 2,778 |
| e1m1 | 3,446 | 463 | 7.44 | 2,444 |
| e1m2 | 3,545 | 441 | 8.04 | 2,267 |
| e1m3 | 3,121 | 419 | 7.45 | 2,445 |
| e1m4 | 3,904 | 495 | 7.89 | 2,710 |
| e1m5 | 2,901 | 387 | 7.50 | 2,372 |
| e1m6 | 1,722 | 228 | 7.55 | 2,686 |
| e1m7 | 979 | 128 | 7.65 | 801 |
| e1m8 | 2,141 | 268 | 7.99 | 1,302 |

The maximum object size was 32 faces.

## Runtime design

On PVS changes, the renderer reconstructed current-PVS object AABBs from the
already retained face bounds. Objects with fewer than three members in the
current PVS bypassed the hierarchy. Each frame used a lazy byte state per
visible object: unknown, admitted, or rejected.

Selection preserved the accepted renderer's exact order and tests:

1. original 16-face block AABB;
2. object AABB when applicable;
3. invariant-front or compact-plane facing policy;
4. original face AABB;
5. water override and flags;
6. original source order.

The normal 36-byte `VisibleFace` was not enlarged; a parallel object-slot
array carried the optional membership.

## PSoXide results

The accepted reference measured 23.410 fps in the current pinned frontend
(historical accepted capture: 23.432 fps). Deterministic canonical E1M1 route,
2,086 frames:

| Variant | FPS | Object tests | Faces rejected by objects | Result |
| --- | ---: | ---: | ---: | --- |
| all cooked groups | 22.651 | 307,272 | 284,079 | reject |
| groups >=3 globally and in current PVS | 22.774 | 84,365 | 171,436 | reject |

The filtered result was exact and deterministic across two runs:

- VRAM FNV-1a: `0x09a7f019bb9a5e7c`;
- display FNV-1a: `0x9bac66f3bec0e66b`;
- full-level elapsed bus cycles: 3,172,084,975;
- full-level presentations: 2,134.

It removed roughly 87,071 original face-AABB tests, yet regressed from 23.410
to 22.774 fps (-2.72%), far outside the measured +/-0.122 fps layout noise.

## Code-shape evidence

- accepted `select_frame_faces_blocked`: `0x818` bytes (2,072);
- filtered scene-object selector: `0xb7c` bytes (2,940);
- object rebuild routine: `0x7a4` bytes (1,956).

The extra slot lookup, lazy-state branches, and 868 hot bytes in selection cost
more than the saved face AABB tests. The first variant's higher rejection count
also failed, so threshold tuning cannot rescue this runtime hierarchy.

## Conclusion

Do not continue tuning runtime object thresholds or add another dynamic culling
layer. Current Quake-PSX PVS sets are already small enough that the hierarchy's
bookkeeping dominates.

The useful Quake II lesson is narrower: move work into deterministic cooking,
but consume it with small, sequential, non-calling MIPS kernels. The next
experiment should specialize the exact accepted face selector (block AABB,
facing, face AABB, water override, stable output order) and reduce its current
`0x818`-byte hot implementation. A later structural experiment may test a
cooker-authored sequential command stream, but it must not add runtime object
lookups.
