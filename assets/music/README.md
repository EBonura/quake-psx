# Quake ambient music

Eight dark-ambient loops, stored here already encoded as the two XA-ADPCM files
the disc carries (37.8 kHz stereo, single speed, four loops interleaved per
file). A fresh clone and CI press the disc straight from them; no WAV is needed.

| File | Loops (channel) | Size |
| --- | --- | --- |
| `MUSIC1.XA` | Akulak (0), Laska (1), Sirtroin (2), Skinox (3) | 29,975,552 bytes |
| `MUSIC2.XA` | Joik (0), Mioto (1), Notus (2), Opax (3) | 23,163,776 bytes |

`music.json` is the pack manifest `psx-audio-cook xa-pack` wrote, `SHA256SUMS`
the digests of the three files (`sha256sum -c SHA256SUMS`; the builder checks
them on every disc build), and `game/src/music_map.rs` the generated song table
the game compiles in.

## Credits and licence

Music: Mulula VGM Loops, Eerie Ambiences Vol 1, licensed CC BY 4.0
(<https://creativecommons.org/licenses/by/4.0/>), <https://mululavgmloops.itch.io/>.

Changes made, as CC BY 4.0 asks us to say: the eight loops (Akulak, Joik, Laska,
Mioto, Notus, Opax, Sirtroin, Skinox) were resampled to 37.8 kHz, loudness-
matched to -19 LUFS integrated, dithered to 16 bits, laid three times end to end
so a loop plays through without a drive seek, and compressed to XA-ADPCM. The
Read-ME shipped with the pack says "CC BY 4.0" and names the author's pages.
The credit also appears in the README, THIRD_PARTY_NOTICES.md, the release notes
and the shipping provenance. There is no credits screen in the game.

## Refreshing the files

Only needed when the loops change. The eight source WAVs (37.8 kHz 16-bit
stereo, one per loop, named `<Loop>.wav`) are not in the repository; the digests
the builder accepts are in `host/quake-build/music.rs`. With them in a folder:

```sh
cargo quake-build music-check   --music-dir PATH   # cook; fail unless byte-identical to these files
cargo quake-build music-refresh --music-dir PATH   # cook and replace these files and game/src/music_map.rs
```

Both cook with the `psx-audio-cook xa-pack` of the PSoXide revision named in
`host/quake-build/music.rs` (fetched into `.quakepsx/xa-pack`, or `--xa-pack
PATH`). The encoder is deterministic: `music-check` reproduces these bytes
exactly.
