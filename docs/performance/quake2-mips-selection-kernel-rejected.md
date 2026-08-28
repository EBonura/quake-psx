# Quake II-style MIPS face-selection kernel: rejected

This branch preserves an exact code-shape experiment. It is not a measured
performance improvement and must not replace the accepted renderer.

## Hypothesis

The accepted `select_frame_faces_blocked` implementation was 2,072 bytes
(`0x818`) with a 152-byte stack frame. Its disassembly showed that LLVM had
expanded two copies of the four-plane GTE AABB support-point test: once for a
16-face union block and again for each admitted face. Quake II PSX repeatedly
uses small non-calling, hand-scheduled MIPS kernels, so this experiment kept
the exact Rust selection policy but replaced only that expansion with fixed
inline MIPS-I/GTE instructions.

The preserved policy is unchanged:

1. conservative 16-face union AABB;
2. cell-authored invariant-front tag or exact compact-plane facing;
3. exact per-face AABB;
4. water override and marker;
5. original face order and output index.

## PSoXide results

The current pinned-frontend reference is 23.410 fps. The historical accepted
capture is 23.432 fps. All valid timing runs used two deterministic canonical
E1M1 routes, 2,086 gameplay frames and 2,134 full-level presentations.

| Variant | Text | Stack | FPS | Image | Result |
| --- | ---: | ---: | ---: | --- | --- |
| accepted LLVM selector | `0x818` / 2,072 | 152 | 23.410 | exact | reference |
| fixed branchy AABB, corrected VZ0 | `0x5f4` / 1,524 | 56 | 23.372 | exact | reject |
| preselected support descriptors, hazard-safe | `0x708` / 1,800 | 80 | 23.333 | exact | reject |

Canonical hashes for both exact candidates:

- VRAM FNV-1a: `0x09a7f019bb9a5e7c`;
- display FNV-1a: `0x9bac66f3bec0e66b`.

The final exact candidate used 3,096,110,884 full-level bus cycles. The first
exact candidate used 3,090,969,698. Both are slower than the accepted current
capture and outside any useful gain even though their code and stack shrink.

## Invalid controls that found two real MIPS hazards

Two intentionally rejected intermediate captures are retained only as debug
evidence:

- 26.512 fps: XY was packed into `$9`, and the same register was mistakenly
  written to GTE VZ0 while the real Z value in `$10` was ignored. Both image
  hashes diverged.
- 36.372 fps: compact support offsets were loaded with `lbu` and consumed by
  `addu` in the immediately following instruction. MIPS-I read the stale
  register value. Rescheduling useful independent loads into every load-delay
  slot restored exact output and removed the false speedup.

These failures are why code size and route completion alone cannot validate a
hand-scheduled PS1 kernel.

## Conclusion

The original selector's large text is not its dominant cost in isolation.
The branchy microkernel reloads frustum sign masks and distances for every
AABB; the descriptor version hoists those decisions but adds byte-offset and
indirect coordinate loads. Both lose enough data-side cycles to cancel the
I-cache/stack reduction.

Do not tune this inline AABB representation further without changing the
source dataflow. A future full assembly selector would need to keep frustum
support state resident across the complete face walk, not inline another
per-AABB helper. The next destination experiment should instead remove a
larger category of work, such as material/window traffic or runtime source
discovery through a monotonic cooker-authored stream.
