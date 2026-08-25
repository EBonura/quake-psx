# Third-party notices

## Quake source and QuakePSX

The Quake engine source was released by id Software under the GNU General
Public License. The QuakePSX port is distributed with a GNU GPL version 2
licence file. This derivative source tree is distributed under GPL-2.0-only;
see [LICENSE](LICENSE).

The earlier C-based QuakePSX port was used as the original PlayStation
reference, and a limited amount of GPL code and behaviour was adapted from it.
The current release separately implements or substantially completes its Rust
runtime, cooker, rendering and gameplay integration, menus, audio, regression
tools and release pipeline on the PSoXide SDK. This engineering distinction
does not change the derivative status of the work.

Copyright and attribution notices in individual files remain in force. Credit
is due to id Software, fgsfdsfgs, and the QuakePSX contributors.

## Quake data

Quake maps, models, textures, sounds, UI art, and other game data remain
copyrighted and are not relicensed by the GPL source release. They are not
included in this repository. The local build uses the original Quake 1.06
shareware distribution subject to its accompanying terms.

## PSoXide

[PSoXide](https://github.com/EBonura/PSoXide) provides the PlayStation runtime,
hardware-access crates, linker script, WORLD.PAK/ISO builder, and emulator used
for regression testing. The exact SDK revision is pinned in `Cargo.lock` and
hydrated locally by the build driver. PSoXide is GPL-2.0-or-later; its own
licence and notices apply to the linked runtime and build tools.

## Historical converter libraries

Earlier private development revisions inherited libpsxav, stb, and dr_wav
through QuakePSX. Those native converter sources are not present in this
public repository or compiled by the Rust-only tree. Current audio conversion
uses PSoXide's Rust WAV and SPU ADPCM facilities.
