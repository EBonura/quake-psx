# Visual checks

Gameplay tests can pass while the renderer is missing polygons, using the wrong
palette or leaving GPU state active between surfaces. The visual regression
uses a fixed E1M1 camera to catch those problems.

## Reference camera

The camera position, angles and coordinate notes are stored in
`tools/visual-parity-cameras.json`. The guest starts directly at that view,
advances a fixed simulation step and records a small probe in RAM.

Run the check with the same PSoXide revision used for the standalone build:

```sh
cargo run --release -- visual-parity-regress --psoxide ../PSoXide-quake
```

The runner captures the view twice. Both runs must produce the same probe,
display dimensions, command counts and image hashes.

## Checked state

The regression checks:

- the 320x240 display;
- world pixels without the HUD;
- HUD pixels;
- the final display;
- texture-window setup and reset;
- packet-buffer overflow;
- visible world, model and sprite packets;
- the selected map and camera.

Expected hashes are stored in `host/quake-build/main.rs`. A deliberate visual
change must be reviewed from screenshots before those values are updated.

## Representative views

The fixed camera is supplemented by the screenshots in `docs/readme/`:

- normal world rendering with the Minimal HUD;
- the Classic HUD and weapon strip;
- translucent water;
- sprite rendering.

These images are documentation, not the automated comparison input.

## Acceptance

A renderer change is acceptable when:

1. host tests and the PS1 build pass;
2. the fixed camera is deterministic;
3. no geometry, HUD element or sprite disappears unexpectedly;
4. GPU state does not leak between materials;
5. packet counts remain within the fixed buffers;
6. any intentional pixel change has been inspected;
7. performance does not regress without a stated reason.

The regression does not cover every viewpoint. Full-map play, camera rotation
and original-hardware testing are still required before a release.
