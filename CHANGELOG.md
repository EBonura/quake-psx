# Changelog

## Unreleased

- Weapons now follow the single player rules: the best-weapon order and ammo
  thresholds (no auto-equip of the launchers, two rounds to select the double
  barrel and super nailgun), the best-weapon upgrade on an ammo box, weapon
  changes held until `attack_finished`, the axe landing two frames after the
  click, the double barrel keeping its model on its last shell, the Thunderbolt's
  `lstart`/`lhit` timing and single effective damage ray, impulse 9's loadout,
  and a god-mode player still burning armour. Radius damage is only halved for
  the attacker (barrels and ogre grenades no longer halved against the player),
  tests five `CanDamage` rays, and the player voices pain on their own blasts.
- Monsters: melee is decided at 120 units eye to eye, the ogre grenades anything
  it sees inside 1000 units on a one to three second reload, zombies roll their
  throw by range, scrags strafe, reload from their last attack frame and cross
  their spikes, knights sidestep during the running attack, a damage wake is a
  full `FoundTarget` (sight voice, pack alert), dogs, fiends, ogres and
  shamblers turn to face before attacking, an ally in the line spoils a shot,
  nightmare soldiers fire a second volley, and a killed scrag falls.
- Pickup, powerup and weapon-change text prints in the top-left notify area
  instead of the screen centre, and the weapon strip flashes a new gun. The
  ring hides the gun. `LIMITS.md` lists the differences that stay on purpose.
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
