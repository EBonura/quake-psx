# Quake II transfer: same-slot texture-window packing (rejected)

Date: 2026-08-28

## Hypothesis

Quake II PSX minimizes renderer state boundaries by presenting material-local
work to its packet emitter. Quake-PSX currently emits a scoped `GP0(E2h)`
texture-window selector and reset around every subdivided liquid/sky packet.
When consecutive physical packets target the same exact ordering-table slot
and use the same selector, those boundaries are not visible GPU state.

This experiment tested whether removing the redundant selector/reset pairs and
DMA tags after each special face was cheaper than submitting them unchanged.

## Implementation

The feature-gated `renderer-window-slot-packing` path walks only the packet
range just emitted for a special face. Adjacent packets are grouped when their
low 16-bit OT slot and selector are identical and the resulting DMA packet
fits the PSX 255-word limit.

The ordinary prepend linker reverses physical packets within an OT slot. To
preserve the exact final GPU order, the packer copies primitive payloads into
the existing 255-word batch scratchpad in reverse physical packet order, then
retains one selector at group entry and one reset at group exit. Singleton
groups are copied unchanged.

The implementation compiled to `0x258` bytes of MIPS code.

## Exact PSoXide result

Command:

```text
cargo run --locked --release -- e1m1-gpu-polygon-window-slot-pack-bench \
  --psoxide /Users/ebonura/Desktop/repos/PSoXide/target/run-fast/frontend
```

Both 3,800-frame runs were deterministic and passed the canonical E1M1 route:

- full-level presentations: `2,134`
- full-level bus cycles: `3,105,821,849`
- measured frame rate: `23.260 fps`
- VRAM FNV-1a: `0x09a7f019bb9a5e7c`
- display FNV-1a: `0x9bac66f3bec0e66b`

Compared with the current `23.410 fps` reference, this is approximately
`0.65%` slower. It is also below the historical accepted `23.432 fps` result.

## Decision

Rejected. The visual/order invariants are correct, but scanning, grouping and
copying already-emitted packets costs more CPU time than the saved DMA tags and
`E2` words recover. This agrees with the earlier post-link window-range
coalescer result (`23.333 fps`): runtime repair of redundant renderer state is
the wrong abstraction boundary.

The transferable Quake II lesson is therefore narrower: material locality has
to be authored into the retained/cooked work stream, or honored directly while
emitting, so the redundant packets never exist. A future experiment should
not add another per-frame packet walk.
