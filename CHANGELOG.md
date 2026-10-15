# Changelog

## Unreleased

- Added the game's own music: eight dark-ambient loops on the disc as XA-ADPCM,
  played sporadically (a random loop after 30 to 90 s of silence, fading in and
  out, then 60 to 180 s of silence). The Options TRACK row is gone; MUSIC and
  MUSIC VOLUME stay.
- Repeats of a loop are gapless: each loop is stored three times end to end in
  the XA file, so a song is one uninterrupted drive read (about 53 MB of music).
- The shipping heap has 8,952 bytes free again (the `ship-boot` floor is 8,192;
  main had 4,952): the resident-map arena margin is trimmed by 6,000 bytes, the
  hazard trampoline reserve drops from 256 to 192 words, and the music player
  drives the CD directly instead of through the SDK's looping player.
- Removed the code that borrowed the PSoXide demo disc launcher's CD-DA tracks
  by number. Standalone Quake now has music, and no longer depends on the
  launcher's track layout.
- Music: Mulula VGM Loops, Eerie Ambiences Vol 1, CC BY 4.0, https://mululavgmloops.itch.io/


## Source 2026.09.05.1

Tagged `source-2026.09.05.1`. Removed a constant cache-miss condition and its
stale comment left after deleting the unused renderer cache.

## Source 2026.09.05

This source snapshot is tagged `source-2026.09.05`. Download versions are
listed separately below; source cleanup does not replace an already published disc.

- Removed unselected renderer experiments and their obsolete benchmark commands.
- Added a bootstrap command so the builder works from a fresh clone.
- Kept the accepted renderer features and historical SDK pin used by the release.
- Updated build instructions and formatted Rust sources.

## 0.1.0-263f43f9 | 2026-09-05

Standalone disc published on itch.io.

- Published the validated 263f43f9 build with its pinned shared renderer and Quake shareware data.
