# Provenance

## Source lineage

This public repository begins with a curated release snapshot rather than the
private development commit history. It contains the Rust Quake port, host
cooker and build tools used by the current PlayStation release.

The implementation is a GPL derivative of id Software's Quake source. The
earlier C-based QuakePSX port by fgsfdsfgs and its contributors was used as the
original PlayStation reference, and a limited amount of GPL code and behaviour
was adapted from it. Those sources remain credited even though their native C
runtime and converter tree are not shipped or compiled here. Required notices
are listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

That reference is not the architecture of the current release. Its shipping
PlayStation executable is Rust-only and built on the PSoXide SDK. Large parts
of the deliverable were implemented or substantially completed for this
project, including:

- the Rust asset cooker and checked runtime formats;
- the BSP renderer integration, collision, moving brushes and target systems;
- the Episode 1 weapon, monster, pickup, hazard and progression systems;
- the menus, two HUD modes, sprite handling, water effects and positional
  audio integration;
- the host route, combat, rendering and hardware-facing regression tools; and
- the reproducible standalone and demo-disc release pipeline.

This distinction describes the engineering boundary; it does not remove the
GPL derivation or the attribution owed to either upstream project.

## Build record

A standalone build writes `dist/quake-psx.provenance.json` beside the BIN, CUE
and executable. It records:

- the clean Quake source revision;
- the clean PSoXide revision;
- the Quake shareware input digest and size;
- the guest build recipe and Rust toolchain;
- the filename, digest and size of each generated artifact.

Only relative artifact names are written. Shipping commands reject dirty
source trees, an unexpected PSoXide revision, a changed hydration stamp,
nonstandard compiler overrides and an unrecognised `PAK0.PAK`.

The sidecar belongs to the artifacts built with it. Rebuilding the disc creates
a new sidecar rather than reusing old results.

## Quake data

No Quake maps, models, textures, sounds or converted game data are tracked by
Git. By default, the builder downloads the Quake 1.06 shareware archive and
checks these SHA-256 values:

```text
quake106.zip  ec6c9d34b1ae0252ac0066045b6611a7919c2a0d78a3a66d9387a8f597553239
PAK0.PAK      35a9c55e5e5a284a159ad2a62e0e8def23d829561fe2f54eb402dbc0a9a946af
```

A local Quake installation may be supplied instead, but its `PAK0.PAK` must
match the same digest. Downloaded and cooked files remain in ignored local
directories.

## Runtime boundary

The PlayStation executable is Rust-only. The build rejects C, C++, assembly
source, native objects and foreign compatibility bindings from the staged game
dependency set. PSoXide supplies the PS1 startup, hardware access, linker
script, renderer support, audio, controller input, disc packaging and emulator
used by the tests.
