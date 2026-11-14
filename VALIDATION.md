# Validation

This file records the checks used for the standalone Quake disc. Results apply
to the source revision that produced them; the JSON file beside each release
contains the exact Quake and PSoXide revisions and artifact hashes.

The current release candidate uses PSoXide revision:

```text
464dcbee633c5ace287d2fb71576bafcb933544d
```

## Host tests

The project contains several independent Cargo workspaces. Run each suite:

```sh
cargo test --manifest-path host/quake-build/Cargo.toml
(cd crates/quake-affine && cargo test && cargo test --all-features)
(cd crates/quake-cook && cargo test)
(cd crates/quake-core && cargo test)
(cd crates/quake-formats && cargo test)
```

The builder tests resolve the emulator frontend themselves, so run them with `QUAKE_PSX_FRONTEND`
unset: two of them (`resolve_frontend_*`) fail when it points at a binary.

The last pre-release run completed:

| Suite | Result |
| --- | ---: |
| Builder tests (`host/quake-build`, every test target, including the input policy and pusher integration tests) | 90 passed |
| `quake-affine` | 31 passed (41 with all features) |
| `quake-cook` | 41 passed |
| `quake-core` unit tests | 410 passed |
| `quake-core` integration tests | 36 passed |
| `quake-formats` | 24 passed |

These tests cover checked file parsing, BSP conversion, collision, movement,
movers, combat, monsters, loading, menus, input and fixed-capacity runtime
state.

## Visual probes

Diagnostic guest features, never in a shipping image. `probe-clear-magenta`
clears the frame to magenta, so any pixel no polygon covered (a crack, a culled
corner) is easy to count. `probe-pose-log` writes the camera to `QUAKE_POSE`
(watch it with `frontend launch --route-watch-u32`). `probe-poses` rebuilds a
view from a script in `QUAKE_PSX_PROBE_POSES` (`x y z pitch yaw roll hold
[mN=x:y:z] [tint=N] [bias=N] [nocull] [vm=MODEL:FRAME]`, origin in whole units,
angles in 4096ths of a turn) and has switches for the behaviours the playtest
fixes replaced (`QUAKE_PSX_PROBE_OLD_CULL`, `QUAKE_PSX_PROBE_BLEND_NAILGUNS`,
`QUAKE_PSX_PROBE_BIAS`). Build one with

```sh
QUAKE_PSX_PROBE_FEATURES=probe-poses,probe-clear-magenta \
QUAKE_PSX_PROBE_POSES='...' cargo quake-build probe-disc
```

and run it with `--guest-frames`, `--route-screenshot-dir` and a
`--route-watch-u32` on `QUAKE_VISUAL_PARITY_PROBE + 8` (the frame counter).

## Route-simulator sweeps

`tools/routesim` links the same movement and collision as the guest and runs
over the cooked maps, so a movement defect can be swept for across a whole map
in seconds. The three sweeps hold full forward from a standstill and need no
guest build:

```sh
cargo build --release --manifest-path tools/routesim/Cargo.toml
for map in start e1m1 e1m2 e1m3 e1m4 e1m5 e1m6 e1m7 e1m8; do
  for mode in slopes ramps traps; do
    ROUTESIM_OPEN_MOVERS=1 <target>/release/quake-routesim id1psx/maps/$map.psb $mode
  done
done
```

`ramps` tries every floor step and slope of a 16 unit grid at 1, 2, 3 and 4
simulation ticks per frame, `slopes` every walkable ramp face, `traps` every
standable floor in eight directions. All nine maps report none blocked and none
trapped (`ramps tested 1064 blocked 0` on E1M1; before the leaning-riser step
it was 3). `walk X Y Z DX DY [--ticks n] [--frames n]` prints one start frame by
frame and `ROUTESIM_TRACE_CALLS=1` logs every trace the motor makes, which is
how the three stalls were taken apart.

## MIPS liquid load-delay regression

Host tests use the scalar implementation, so also execute the actual MIPS inline
assembly when changing liquid rendering or its instruction schedule:

```sh
cargo run --release --manifest-path tools/liquid-mips/Cargo.toml -- \
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
cargo quake-build e1m1-spawn-regress --psoxide ../PSoXide-quake
cargo quake-build e1m1-save-regress --psoxide ../PSoXide-quake
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
| `e1m1-spawn-regress` | Every E1M1 monster stands where `droptofloor` puts it (pinned table, also checked on the host by `crates/quake-core/tests/e1m1_monster_spawn.rs`), only patrollers move while the player stands at the spawn, and with the player at the t2 bridge the woken grunts never stand with their box hanging off the platform edge (`SV_CheckBottom`) |
| `e1m1-save-regress` | Save and load through the real card code on the emulator's memory card, in four boots. Two scripted boots on a blank card: the boot-time loads answer SLOT 2 and SLOT 1 IS EMPTY; a save into slot 1; the player is moved, hurt and given every weapon and the first door starts opening; a save into slot 2; a second perturbation; loads of slots 1 and 2 give back the saved fingerprint, position, health, inventory and mover count; the empty slot 3 leaves the game alone; every map is started and its save size logged; the E1M3 game is saved into slot 3 and loaded back from E1M8. Both boots must agree on every figure and every byte of the card. A third boot on the saved card restores slots 2 and 1; a fourth, on a card with one flipped bit in slot 1, refuses it with SAVE NOT USABLE, still loads slot 2 and leaves the game alone. Host tests (`cargo test` in `crates/quake-core`) round-trip every snapshot type, with struct literals that stop compiling when a saved type gains a field |
| `e1m1-chain-bench` | The same E1M1 route at a fixed simulation step for comparable performance measurements. Its fps and elapsed cycles are field-quantised wall numbers; with `QUAKE_PSX_PC_LINE_LOG=1` it also prints `full_level_work_instructions` (retired outside the vblank spin and CD reads), the number that ranks SDK and renderer work |
| `e1m2-e1m3-route-regress` | Linked routes through E1M2 and E1M3 into E1M4 |
| `combat-regress` | Shotgun firing, damage, pain, death and view-model animation |
| `monster-regress` | Authored E1M1 monster population and basic behavior |
| `monsterjump-regress` | Demon jump movement and collision |
| `bestiary-regress` | Shared behavior required by every shareware monster |
| `systems-regress` | Targets, secrets, traps, movers and map-specific mechanisms |
| `arsenal-regress` | Weapon pickups, inventory carry-over, projectiles and firing |
| `survival-regress` | Hazards, drowning, death, respawn, artifacts and megahealth |
| `episode1-regress` | E1M7 from the sigil to the exit and Start: the arena walk, the lift, then laps of the ring that press both electrode buttons (x=352) before the `event_lightning` button until Chthon dies, the lava bridge, the shaft and the rune hand-over |
| `audio-regress` | Weapon playback, SPU activity and silence after completion |
| `ambient-regress` | Continuous positional ambient playback |

### Music

The ambient music is checked headlessly rather than by a pass/fail runner.
`cargo quake-build music-disc` builds the shipping stack with
`music-trace`, which logs every schedule step as
`quake-music <step> tick=<vblank> song=<n> level=<0..256>` on the guest debug
log (`frontend launch --guest-debug-log --cd-command-log`). Adding
`QUAKE_PSX_EXTRA_FEATURES=music-trace-fast` to a regression run
(`e1m1-chain-regress --with-music`) cuts the silences to seconds so a loop
is playing when the route crosses a map load; the log must then show `stop`
before the load and `restart` after it, and the CD command log must hold no
error. The schedule itself is unit-tested in `crates/quake-core` (`cargo test
ambient`). None of this measures XA behaviour on a console.

The visual, route and audio runners repeat their important checks and compare
the resulting probes. A result is accepted only when both runs agree.

## Current results

At source revision `238f236a5d937baeef5314b10938fdb8b9b97ee2`, with the SDK at
`464dcbee633c5ace287d2fb71576bafcb933544d` and the emulator at
`3ce0aa9c4aba4745a268e8a45e67a7bdb18217ae`, the host suites, `ram` (the
shipping image booted headlessly, with the link-map breakdown), `start-route-regress`,
`e1m1-chain-regress`, `e1m1-spawn-regress`, `e1m1-save-regress`,
`visual-parity-regress` and the fixed-step `e1m1-chain-bench` passed on both the
`real` and the `mister` hardware targets. The visual run matched its stored world,
HUD and display hashes (`0x52a8fd14affff202`, `0xf0ff5a6113cad7ec`,
`0xd4e066ef8f25ed5a`). `map-regress` runs but its initial-load check is red: the
first load takes 440,995,618 cycles against a budget of 420,000,000 and 81
sector-read sessions against 74.

The benchmark completed E1M1 deterministically at 26.715 fps (1,971,343,369 bus
cycles over 1,556 presentations). The shipping image leaves 19,260 bytes of heap
free after boot (floor 8,192). The target remains 30 fps. These figures are useful
for comparing builds, but they are emulator measurements rather than
original-hardware claims.

## Resource checks

The cooker rejects maps that exceed the fixed resident arena, entity limits,
delayed-target limits, model tables or sound banks. The game uses fixed pools
for projectiles, particles, movers, monsters and other short-lived objects so
a busy scene cannot grow the heap during play.

`ship-boot` runs the normal release configuration rather than a reduced test
feature. It checks that the real menu and gameplay path reach the main loop
with at least the configured heap floor remaining, and that the deepest stack
the emulator saw stays within four fifths of the reserved stack. `cargo
quake-build ram` runs the same boot and prints the link-map breakdown of
`.text`, `.data`, `.bss` and the overlay images (`docs/RAM.md`).

## Journeys

Two journeys replay recorded pad tapes through the real title menu, the START hall and the first maps, with live monsters at Easy skill. `tools/journey-bot` plays the journey image with a host controller and records the tapes; `psoxide-gate run <chain.toml> --hardware real|mister` replays them and checks the exact state (map, skill, origin, health, armour, ammunition, weapons, keys, kills, secrets) at every map change. See `tests/journeys/README.md`.

- `quake-e1-chain` (`tests/journeys/e1-chain`, image from `cargo quake-build journey-build`): Start (menu, Easy hall, E1 slipgate) to E1M1 to the E1M2 load, 40 checks, on `real` and on `mister`. The image steps the world one fixed 30 Hz tick per frame (`perf-fixed-ticks-30hz`), so a recorded pad replays to the same state on either hardware model. What it proves is therefore the game at the fixed step; it does not prove the shipping game's variable step (the VBlanks a frame took).
- `quake-e1-shipping` (`tests/journeys/e1-shipping`, image from `cargo quake-build journey-build-shipping`): the same route on the shipping image, with its own timing. Run it on `--hardware real` only: the two models present frames on different VBlanks, so the shipping step, and with it the play, differs on `mister` and the tapes (recorded on `real`) end in another state there (9 of 40 checks fail on `mister`, by design of the timing, not a defect found).

Not covered: every map after E1M2 (see `tests/journeys/README.md`). Record with `--routesim tools/routesim/target/release/quake-routesim`; a missing planner binary makes every `@go` leg fail quietly. `--cut-every N` writes a checkpoint every N frames, to find where a replay leaves the recording.

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
