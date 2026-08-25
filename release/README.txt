quake-psx
=========

This is a native PlayStation port of Quake 1.06 shareware Episode 1, written in
Rust on the PSoXide SDK.

To play, open quake-psx.cue in a compatible PlayStation emulator. For original
hardware, burn the BIN/CUE pair together using software that understands CUE
sheets; do not burn the BIN as an ordinary data file.

The release contains Start and E1M1-E1M8 from the canonical Quake shareware
archive. It does not contain the registered episodes or original soundtrack.

This is not the first homebrew Quake project for PlayStation. id Software's
GPL source and the earlier C-based QuakePSX port were used as references and
remain credited. This release uses a separately implemented Rust runtime and
toolchain built on the PSoXide SDK.

Source: https://github.com/EBonura/quake-psx

See THIRD_PARTY_NOTICES.md and LICENSE for licensing and attribution. Build
inputs and exact artifact hashes are recorded in quake-psx.provenance.json.
