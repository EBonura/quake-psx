# Changelog

## Unreleased

- Save and load: SAVE GAME and LOAD GAME in the pause menu (LOAD GAME on the title
  menu too), three slots, each a memory-card file. A save holds the map, skill, player,
  weapon state and every moving part of the scene (monsters, doors, lifts, trains,
  triggers, targets, secrets, lights) behind a magic number, a format version and a
  checksum; a card from another build or a damaged one is refused. The encoder, the
  decoder and the card driver are three overlay images in the loader's transfer buffer
  and the payload (5,163 bytes on E1M3 on Hard, the largest) waits on the caller's stack
  frame, so the resident cost is 6.9 KB (`heap_free` 24,120 to 17,192, floor 8,192).
  `e1m1-save-regress` covers save, load, reboot, damage and a cross-map load; the
  composition fixes (linker patterns with the wrong mangling length, a generic card
  helper left resident, disc chunks 6 to 8 refused by the chunk lookup) are in the
  commit message.

- RAM: the guest builds with `-Cpanic=immediate-abort`, so a panic is a BREAK at the
  faulting instruction instead of a formatted message. `core::fmt` (integer and
  str Display, `pad_integral`, `Arguments`), every call site's `Location` and file name,
  `psx_rt::report_panic` and `from_utf8` leave the image: `heap_free` goes from 10,136 to
  24,792 (`core::str::from_utf8` and `<[u16]>::contains` were trimmed too). psx-rt's
  exception handler records Cause, EPC and BadVAddr and halts; `ship-boot` reads them from
  the RAM dump and fails on any CPU exception (`QUAKE_PSX_PANIC_MESSAGES=1` restores
  the formatted panic for a chase). With the room back, live one-shots follow the
  listener again (`S_Update` respatialization, 05.1.3). `heap_free` is 24,120 with it
  (floor 8,192); the E1M2/E1M3 route regression disc boots with 11,000 bytes of extra
  padding (it stopped at about 700 before).

- Inventory pass 3: the player's pain voice rolls over pain1, pain2, pain3 and pain6
  (pain2 is a global effect now, 38 global sounds, 102 of 104 slots on the worst map); every
  centerprint line is centred on its own and starts 35 percent down (or at 48 for
  five lines or more); the pentagram shows 666 beside the disc; the pentagram and the quad
  light the player; the intermission looks through one of the map's cameras; a
  teleport blocks backward walking for 0.7 s; wizard spikes puff on walls and leave a
  tracer. `episode1-regress` now raises both
  electrodes (laps of north button, south button, lightning button) and
  passes. `heap_free` is 10,136 (floor 8,192; the E1M2/E1M3 route regression
  disc stops booting between 9,694 and 10,044).

- The music is in the repository, already encoded: `assets/music` holds
  `MUSIC1.XA` and `MUSIC2.XA` (53 MB) with their manifest, digests and credits,
  and every disc build presses them, so a fresh clone and the release workflow
  need no WAVs and no xa-pack checkout (the release build used to stop at
  "pass --music-dir"). `cargo quake-build music-check --music-dir PATH` cooks
  the loops again and fails unless the result is byte for byte the committed
  files; `music-refresh` replaces them. `--music-dir` no longer applies to disc
  builds; regression and benchmark discs that want the music take `--with-music`.
  Music: Mulula VGM Loops, Eerie Ambiences Vol 1, CC BY 4.0,
  https://mululavgmloops.itch.io/ (changes listed in `assets/music/README.md`).

- Inventory pass 2 (parts 04 and 05 of the 2026-10-15 inventory, triaged row by
  row): the three E1M1 sixteen-unit rises with leaning risers no longer stall a
  standing start (routesim `ramps` at 1 to 4 ticks a step: 0 blocked on all nine
  maps); Chthon's lava balls fly from their spawn point at the player and lead
  him on Hard and Nightmare, smoke and glow like rockets; light style 2 is
  id's 51-step pulse (E1M5, E1M6); the `+0` texture chains animate (the E1M1
  slipgate and window, every switch) and a pressed button shows its `+a`
  textures; sound pans against the listener's right; the end-of-level panel
  waits for a button; named lifts cycle again after their first ride; a secret
  door's message prints; trains voice `train2` only at waiting corners; a dead
  player touches no trigger; monsters flash the room when they fire; Start's skill
  portals set the Options row; the classic status bar's ammo counts sit at id's x;
  key doors and pickups say runekey in E1M6 to E1M8. The E1M1 route's fixed-step
  work is +0.17 percent (the texture chains) and `heap_free` is 10,744.

- The 2026-10-15 playtest: a lift car or door sorts four ordering-table slots
  closer than the world faces around it, so a wall piece with a nearer average
  depth no longer overdraws half the car; a tall brush model (the E1M1 lift) is
  drawn when its box touches a visible leaf even if the leaf at its centre is
  not one; the cull frustum is built from the view matrix the GTE draws with,
  because the strafe roll turned the other way in the textbook formulas and a
  rolled view lost a corner of the screen (a hole showing the background; the
  E1M1 route went from 58 frames with background pixels to 14, 25 frames with
  a hole of over 100 pixels to 1); the nailguns are shown a frame at a time so
  the muzzle flash no longer slides between the two muzzles. The E1M1 route's
  fixed-step work is unchanged (+0.2 percent) and `heap_free` is 13,140.
- `probe-disc`, with the `probe-poses`, `probe-clear-magenta` and
  `probe-pose-log` features, rebuilds a playtest view from its camera pose and
  finds holes and cracks by clearing the frame to magenta (VALIDATION.md).
- The shipping heap has 29,540 bytes free (the `ship-boot` floor is 8,192; the
  checkpoint had 9,912). The fixed pools are sized to the cooked Episode 1
  maxima the builder already proves (they carried 74 spare render entities, a
  few movers and triggers, and 1,878 bytes of map arena), `EntityScene::load`
  keeps its size optimisation, the death bubbles use integers instead of
  linking soft-float, and the loading error line no longer uses `alloc::format!`.
  `cargo quake-build ram` prints the whole RAM breakdown; see `docs/RAM.md`.
- Survival, view and message rows (the physics and triggers pass, without its
  player motor rewrite): the damage flash counts half of blood plus armour with
  a floor of ten, powerup tint order and expiry text, death clears the
  artifacts and times its fall animation (the body falls through the ordinary
  motor with no input), respawn waits for a release and then a press, the
  pain voice is muted during weapon frames and the ring, view kick points at
  the real damage source, stairs smooth the view, look limits are 80 and 70
  degrees, the player spawns one unit up, and `SUB_UseTargets` messages play
  `talk.wav`.
- The lightning beam shortening and the sound pan no longer divide 64-bit
  numbers, so the guest links no `__divdi3`.
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
