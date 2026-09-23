# Validation

This file records the checks used for the standalone Quake disc. Results apply
to the source revision that produced them; the JSON file beside each release
contains the exact Quake and PSoXide revisions and artifact hashes.

The current release candidate uses PSoXide revision:

```text
c2c4b90de6f0c836803c6eba02775814a944dafa
```

## Host tests

The project contains several independent Cargo workspaces. Run each suite:

```sh
cargo test --manifest-path host/quake-build/Cargo.toml
(cd crates/quake-cook && cargo test)
(cd crates/quake-core && cargo test)
(cd crates/quake-formats && cargo test)
```

The last pre-release run completed:

| Suite | Result |
| --- | ---: |
| Builder tests (`host/quake-build`) | 59 passed |
| Input policy integration tests | 8 passed |
| Pusher integration tests | 4 passed |
| `quake-cook` | 30 passed |
| `quake-core` unit tests | 308 passed |
| `quake-core` integration tests | 24 passed |
| `quake-formats` | 21 passed |

These tests cover checked file parsing, BSP conversion, collision, movement,
movers, combat, monsters, loading, menus, input and fixed-capacity runtime
state.

## MIPS liquid load-delay regression

Host tests use the scalar implementation, so also execute the actual MIPS inline
assembly when changing liquid rendering or its instruction schedule:

```sh
python3 tools/test-liquid-mips.py \
  --frontend ../PSoXide-editor/target/release/frontend \
  --out /tmp/quake-liquid-mips-check
```

This assembles the checked-in routine with GNU MIPS binutils, runs synthetic
constant and patterned tiles through the headless emulator, and requires every
output texel to match an independent scalar reference. It records the frontend
hash and rejects the previous immediate load-use schedule. No game assets or
diagnostic guest features are required. Set `MIPS_AS` and `MIPS_OBJCOPY` for a
different GNU tool prefix (for example `mipsel-linux-gnu-as` and
`mipsel-linux-gnu-objcopy`).

## Build checks

Use the hydrated PSoXide worktree described in the README:

```sh
cargo run --locked --release --manifest-path host/quake-build/Cargo.toml -- check --psoxide ../PSoXide-quake
cargo run --locked --release --manifest-path host/quake-build/Cargo.toml -- build --psoxide ../PSoXide-quake
cargo run --locked --release --manifest-path host/quake-build/Cargo.toml -- ship-boot --psoxide ../PSoXide-quake
```

The build tool checks:

- the PSoXide revision and clean working tree;
- the Quake 1.06 shareware archive and `PAK0.PAK` digests;
- all nine map payloads and their resident-memory limits;
- the Rust-only PS1 dependency set;
- the MIPS executable header and size;
- generated BIN/CUE names and contents;
- clean, relative paths in the provenance file;
- reproducible guest inputs and an isolated Cargo configuration.

Shipping builds start from an empty guest target directory. Developer and
regression builds may reuse Cargo output.

## Emulator regressions

Every regression builds a separate executable feature and runs it through the
PSoXide frontend. The generated discs and captures are ignored by Git.

```sh
cargo quake-build map-regress --psoxide ../PSoXide-quake
cargo quake-build start-route-regress --psoxide ../PSoXide-quake
cargo quake-build visual-parity-regress --psoxide ../PSoXide-quake
cargo quake-build e1m1-chain-regress --psoxide ../PSoXide-quake
cargo quake-build e1m1-chain-bench --psoxide ../PSoXide-quake
QUAKE_PSX_PC_LINE_LOG=1 cargo quake-build e1m1-chain-bench --psoxide ../PSoXide-quake
cargo quake-build e1m2-e1m3-route-regress --psoxide ../PSoXide-quake
cargo quake-build combat-regress --psoxide ../PSoXide-quake
cargo quake-build monster-regress --psoxide ../PSoXide-quake
cargo quake-build monsterjump-regress --psoxide ../PSoXide-quake
cargo quake-build bestiary-regress --psoxide ../PSoXide-quake
cargo quake-build systems-regress --psoxide ../PSoXide-quake
cargo quake-build arsenal-regress --psoxide ../PSoXide-quake
cargo quake-build survival-regress --psoxide ../PSoXide-quake
cargo quake-build episode1-regress --psoxide ../PSoXide-quake
cargo quake-build audio-regress --psoxide ../PSoXide-quake
cargo quake-build ambient-regress --psoxide ../PSoXide-quake
```

| Regression | What it checks |
| --- | --- |
| `map-regress` | Start and E1M1-E1M8 load, validate and follow every Episode 1 transition |
| `start-route-regress` | Start map movement, buttons and slipgate entry |
| `visual-parity-regress` | Fixed E1M1 camera, display hashes and GPU state |
| `e1m1-chain-regress` | A played E1M1 route with mechanisms, combat and exit |
| `e1m1-chain-bench` | The same E1M1 route at a fixed simulation step for comparable performance measurements. Its fps and elapsed cycles are field-quantised wall numbers; with `QUAKE_PSX_PC_LINE_LOG=1` it also prints `full_level_work_instructions` (retired outside the vblank spin and CD reads), the number that ranks SDK and renderer work |
| `e1m2-e1m3-route-regress` | Linked routes through E1M2 and E1M3 into E1M4 |
| `combat-regress` | Shotgun firing, damage, pain, death and view-model animation |
| `monster-regress` | Authored E1M1 monster population and basic behavior |
| `monsterjump-regress` | Demon jump movement and collision |
| `bestiary-regress` | Shared behavior required by every shareware monster |
| `systems-regress` | Targets, secrets, traps, movers and map-specific mechanisms |
| `arsenal-regress` | Weapon pickups, inventory carry-over, projectiles and firing |
| `survival-regress` | Hazards, drowning, death, respawn, artifacts and megahealth |
| `episode1-regress` | The currently scripted multi-map episode route |
| `audio-regress` | Weapon playback, SPU activity and silence after completion |
| `ambient-regress` | Continuous positional ambient playback |

The visual, route and audio runners repeat their important checks and compare
the resulting probes. A result is accepted only when both runs agree.

## Current results

At source revision `ae28819a1f516e3e10c9ba638714a56b4bbfeb42`, the
pre-release tree passed the host suites, `check`, the standalone build,
`ship-boot`, `arsenal-regress`, `visual-parity-regress` and the fixed-step
`e1m1-chain-bench`. The visual run matched its stored world, HUD and display
hashes with no packet overflow.

The benchmark completed E1M1 twice deterministically at 21.857 fps, covering
2,086 route frames, 2,134 presentations, every authored player mechanism and
the E1M2 transition. The arsenal run likewise passed twice: all six Episode 1
pickup weapons fired and animated, while the separate lightning probe verified
its clipped beam and damage traces. The target remains 30 fps. These figures
are useful for comparing builds, but they are emulator measurements rather
than original-hardware claims.

## Resource checks

The cooker rejects maps that exceed the fixed resident arena, entity limits,
delayed-target limits, model tables or sound banks. The game uses fixed pools
for projectiles, particles, movers, monsters and other short-lived objects so
a busy scene cannot grow the heap during play.

`ship-boot` runs the normal release configuration rather than a reduced test
feature. It checks that the real menu and gameplay path reach the main loop
with at least the configured heap floor remaining.

## Visual checks

The automated camera checks command counts, display dimensions, world and HUD
hashes, texture-window state and packet overflow. Representative screenshots
are stored under `docs/readme/`. See [VISUAL_PARITY.md](VISUAL_PARITY.md) for
the method and [RENDERING.md](RENDERING.md) for renderer details.

Image hashes are deliberately kept in the runner rather than copied here. This
avoids stale documentation when a reviewed visual change updates the reference.

## What remains

- Run the complete episode route from beginning to end.
- Add scripted routes for E1M4, E1M5, E1M6 and E1M8.
- Test the standalone disc and demo-disc build on original PlayStation hardware.
- Measure sustained frame rate and audio behavior on that hardware.
- Investigate the remaining fixed-point seams on extreme surfaces.

A successful emulator run is a development check, not final hardware approval.
