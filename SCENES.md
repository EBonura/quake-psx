# Quake on the scene core

How Quake maps onto the SDK's scene system: a resident core in a fixed window at the top of RAM, a
handoff block above it, and `SCENES.TBL` at member LBA 22. The row format, kinds and link rules are the
core lane's CORE-ABI.md (campaign `scene-system-1123`, day 0, 11-23); everything here is provisional
with it. CORE-ABI is written for the 10,240 B cap (window 0x801FD700); this branch links for the
provisional 8,192 B window, **0x801FDF00..0x801FFF00**, so every SP and bound below uses 0x801FDF00.
Line numbers are at commit a7a1db4 of this repo; SDK lines are at the pin 6bfe478ff0e0
(`components.lock.json`). Sizes are from the a7a1db4 ship-boot link map unless they say otherwise.

Rule this follows (Manny, 11-23): scene boundaries only where the player already waits today, and every
screen reachable in play stays resident in the playing scene with no new disc read. For Quake that is
one scene: the main menu draws over the live Start map (`quake.rs:721-748`), so it cannot be split from
the game, and every map shares all its code.

## What the player sees

| What the player sees | In the program | Resident in |
|---|---|---|
| Boot splash "Built with PSoXide", 150 frames, skip after 8 | `platform::show_boot_splash` (`platform.rs:300-315`), called at `quake.rs:218-226` | scene 0 today; the core's boot scene under `scene-core` (step 3) |
| Loading picture "GLOBAL SOUND", then loading Start | `present_before_payload` (`crates/quake-core/src/loading.rs:9-12`), `quake.rs:253-280` | scene 0 |
| Main menu over the Start map (NEW GAME, LEVELS, OPTIONS, ...) | `quake_core::menu::Menu`, active from boot (`menu.rs:486-487`) | scene 0 |
| Levels: Start hall, E1M1..E1M8 | `load_level` (`quake.rs:2137-2212`): an in-program data load, not a switch | scene 0 |
| Pause menu (8 rows, options) | Start or Select during play (`quake.rs:528-531`); the world keeps drawing under it | scene 0 |
| Save and Load (pause menu rows) | `quake.rs:617-650`: reads the save, load and card overlays (chunks 109, 110, 111) into the overlay window | scene 0 (reads the disc today; see the load-speed table) |
| Options (deadzone, brightness, HUD, water warp, lights, poses) | menu page | scene 0 |
| Intermission panel, episode end panel | `quake.rs:444-490`, `1798-1846`, `1834-1840` | scene 0 |
| Death and respawn | `quake.rs:871-888`: same map, a residency hit, no map read | scene 0 |
| Exit | none: no quit row (`menu.rs:318-339`); the program never returns | |

## Scene table rows

| id | kind | What it is | Fields that matter | return scene |
|---|---|---|---|---|
| 0 | 0 Program (boot scene: header 0x10 = 0) | `PSX.EXE`, the whole game | `t_addr` 0x80010000, `t_size` 533,504 (0x82800), file 262 sectors with the header, entry PC `__core_boot` once linked with psoxide-core.ld (`_start` today), SP 0x801FDF00, name `QUAKE` | 0xFFFF (none: Manny 11-23, no return to a launcher) |
| 1 | 2 Data **(provisional kind)** | `WORLD.PAK`, member LBA 1024, 7,292 sectors, PSOXWPAK v1 with its own 17-entry table | LBA and sectors only. Quake keeps its own pack table (`PACK_TABLE`, `platform.rs:725-792`) and reads chunks by id | n/a |
| 2 | 2 Data **(provisional)** | `MUSIC1.XA`, LBA 8316, 12,832 sectors | LBA and sectors, so `Music::probe` could skip the PVD and root read if the core ever hands rows to a program (CORE-ABI has no call for that today) | n/a |
| 3 | 2 Data **(provisional)** | `MUSIC2.XA`, LBA 21148, 9,916 sectors | as row 2 | n/a |

Not rows: the five overlay packages (monsters chunk 4, doors 5, save 109, load 110, card 111). They are
chunks of `WORLD.PAK` that Quake loads into its own 30,720 B transfer window through
`psx_residency::arena` (`monster_code.rs:22-31`, 171-186, 588-625). They are in-program data, not
core Modules: the window and the relocation stay Quake's. Rows 2 and 3 are optional; without them the
music lookup stays as it is.

On the demo disc Quake would be one Program row in the host's table with its own `lba_offset`; it already
chain-loads there through `disc_base` today.

## Shared code versus per scene

One Program row, so everything is shared: renderer, entities, collision, menus, audio, music, the save
path and the pools allocated once at boot (`docs/RAM.md:28-56`). Per map there is only data: the map's
lumps in the 866,128 B arena, its textures in VRAM x 320..959 and its sound bank in SPU RAM above the
global bank.

## The relink (step 1, done)

`game/ram-layout.rs` holds the one copy of the layout: `RAM_SIZE_OVERRIDE = 0x1FE000` (marked
provisional) and the existing 52 KiB `STACK_RESERVE_OVERRIDE`. `game/build.rs` writes both into `-T`
files ahead of psoxide.ld (psoxide.ld:16-21), so the map records them (map lines 2-3).
`host/quake-build` includes the same file for `SHIP_BOOT_HEAP_END` (`main.rs:240-251`), which was a
hard-coded 0x801f2f00. That is the only build change the window needs: left alone, the ship-boot check
would have kept passing its range check and reported 41,900 B free against a heap that now ends 8 KB
lower. No floor was touched. Every build (ship, journey, every regression feature disc) links through
build.rs, so all of them carry the window.

Measured on the normal ship build (`psoxide-pin`, `quake-build build`, `quake-build ship-boot`, the
three commands of `[[build]] ship`), frontend 31982f1e5c8d, against the 33d011c ship build kept by the
1122 lane (`quake-repin-1122/keep/33d011ca39bd/`, map = the installed library map
`Quake (quake-psx).6bfe478ff0e0.map`, sha256 637cfad0...):

| | before (33d011c) | after (a7a1db4) |
|---|---|---|
| header SP (`STACK_INIT`) | 0x801FFF00 | 0x801FDF00 |
| `__heap_end` (`STACK_INIT - STACK_RESERVE`) | 0x801F2F00 | 0x801F0F00 |
| `__bss_end` = `__heap_start`, highest byte the image owns | 0x800DB924 | 0x800DB924 |
| .text / .data / .bss | 498,896 / 35,320 / 299,604 | the same |
| `[[size]]` free (link-time heap region) | 1,144,284 | 1,136,092 (-8,192) |
| heap free after the ship-boot run (bump cursor) | 41,900 (keep PASS json, `heap-free`) | 33,708 (`captures/ship-boot/summary.txt`) |
| heap floor `SHIP_BOOT_MIN_HEAP_FREE` | 8,192 | 8,192, unchanged; 25,516 B above it |
| stack depth over the ship-boot run | not in the keep json | 41,208 of 53,248 (four-fifths line 42,598) |
| exe bytes that differ | | 35 of 536,576: header SP (1), the 32-byte gate receipt digest at 0x408, `_start`'s `__heap_end` immediate (0xBC9) and `psx_rt::heap::ALLOCATOR (.1)`'s initial value (0x7F335) |

**The 8,192 B window fits Quake**, with 25,516 B of heap left above the 8,192 B floor and before any
give-back. The 10,240 B cap would leave 23,468 B. The pools are allocated once at full size, so the
cursor (0x801E8B54 after boot) does not move with the window; only the end of the heap does.

Window check on the map: nothing the program loads is linked at or above 0x801FDF00. The only map
sections above it are the five overlay packages' link addresses (0x80408000..0x8044A808,
`game/monster-overlay.ld:3-7`), which are never loaded there: the packer relocates them into the
transfer window inside the heap.

**Finding: the stack still starts above the window when the BIOS boots the disc.** The ship-boot stack
profile records `entry_sp = 0x801ffff0` (`captures/ship-boot/stack.csv`): the BIOS takes the stack
from `SYSTEM.CNF`'s `STACK = 801FFFF0`, which the SDK writes for every disc (`psx-iso`
`iso9660.rs:692`), not from the header. psx-rt's `_start` does not set `$sp`; it trusts the loader
(`psx-rt/src/lib.rs:164-168` is a doc comment, not code). So a relink alone moves the stack's link-time
budget, not where the stack runs: on this disc the stack still covers 0x801F5EF8..0x801FFFF0, 8,432 B of
it in the window. This is harmless until something lives in the window, and it is what CORE-ABI 4.3
step 1 fixes (`__core_boot` sets `$sp = 0x801FDF00` first). It means the relink cannot be tested for
"the window stays untouched" before `__core_boot` exists, and the SCENES.md files of the first three
programs say `_start` takes the header's stack, which is not what runs. Started at 0x801FDF00 the
measured depth would end at 0x801F3E08, 12,040 B above the heap end.

## Conflicts with CORE-ABI.md

- **Window size.** CORE-ABI 0 and 4.1 say 0x801FD700 / `RAM_SIZE_OVERRIDE = 0x1FD800` (10,240 B); this
  branch uses the brief's 8,192 B. Moving to 10,240 B is a one-line change in `game/ram-layout.rs` and
  still fits (23,468 B above the floor).
- **Disc layout.** mkisopsx writes `PSX.EXE` at LBA 22; the table wants 22 and the exe at 23. SDK branch
  lane/disc-scenes-1123 adds `mkisopsx --scenes`; this repo's pin (6bfe) does not have it. Quake's
  build stops if the ISO root exceeds one sector (`host/quake-build/main.rs:4857-4880`); one more root
  record (`SCENES.TBL`) keeps it at one sector (five files today).
- **Blocking reads end in a proven Pause.** `psx_core_read` does (CORE-ABI 3). `ChunkStream`
  (`platform.rs:805-938`) keeps one `ReadN` session open across many small reads on purpose ("starting
  and pausing the drive for every eight-row texture upload was the dominant cost of the all-Rust
  loader", `platform.rs:805-808`). Map loads must therefore go through the stream-client calls
  (`psx_core_client_attach`, `psx_core_stream_open`, `psx_core_stream_sector`, `psx_core_pause`,
  `psx_core_client_detach`), not through `psx_core_read`, or every map load gets slower. Only
  `read_storage_burst` and `fill_storage_cache` (`platform.rs:989-1066`) are true blocking range reads.
- **Retries on a stream.** Quake retries a session open 3 times with a 400-poll settle between
  (`platform.rs:940-987`, from hardware-tests v2.7). `psx_core_read` carries the row's attempt count,
  but `psx_core_stream_open` has no retry in the ABI. Either the core retries the open or Quake keeps a
  three-try loop around it.
- **The drive "still next to the exe".** `Music::probe` runs before any other read so the PVD and root
  lookup costs no seek (`quake.rs:230-233`). With Quake as the disc's boot program and the core reading
  only the table at LBA 22, that still holds. If a separate boot-scene program runs first, it does not.
- **No CD IRQ handler, polled only.** True of Quake today: the XA `Player` is polled and every data read
  is polled (`platform.rs`, `music.rs`).
- **Saves across scenes.** Card I/O stays in Quake's card overlay and only runs from the pause menu, so
  a switch can never find a `CardJob` pending.

## What the core replaces (step 3)

Measured on the a7a1db4 ship map (text and data are byte-for-byte the 33d011c layout, so the 6bfe library
map gives the same numbers):

| What | Symbols (file:line) | Bytes |
|---|---|---|
| Boot splash | `psx_splash::show` 4,252, `logo::expand` 540, `draw_line` closure 260, `FontAtlas::upload` 428 (text); `logo::CODED` 2,746, `logo::PALETTE` 32 (data). Called from `platform::show_boot_splash` (`platform.rs:300-315`, inlined) at `quake.rs:218-226` | 5,480 text + 2,778 data = **8,258** |
| SDK polled sector reader | `SectorReader::prepare` 1,300, `read_sector` 1,036, `send_command_waiting` 724, `start_read` 540, `ack_unexpected` 448, `stop` 232; `ReaderPause::wait_seek_end` 668, `status` 588, `wait_completion` 464 | **6,000** text |
| psx-io command layer | `Cd::command_within` 520, `cd::begin_polled_command` 160, `read_response_fifo` 168, `finish_failed_polled_command` 112, `drain_response_limited` 84, `ack_all_and_reset_parameters` 48 | **1,092** text |
| Quake's session layer (retry and settle) | `platform::read_chunk_exact` 936, `settle_drive` 220, `begin_session` 136, `settle_after_load` 32 (`platform.rs:940-1131`) | **1,324** text (`read_chunk_exact`'s cache logic stays; the retry and drive code goes) |
| The one reader and its sector | `platform::READER` 2,064 data (`platform.rs:46`, the `SectorReader` with its own sector buffer) | **2,064** data |
| | **Total that leaves once the core owns the drive** | **about 10,480 B plus the 8,258 B splash** |

Stays in Quake: the XA player (`psx_io::cd::xa::Engine` and its `Drive` impl, 2,232 B text: it keeps
running from Quake's loop, PLAN 3.1, and talks to the drive through `psx_core_cd`), the pack table
(`entry_for` 1,140, `PACK_TABLE` 204), `ChunkStream` (968 B of text in its four methods, the stream client), the two-sector
cache (`STORAGE_CACHE` 4,096), the bounce sector `SECTOR` 2,048, and the music lookup
(`Music::locate_files` 632 + 184, `probe` 68: the PVD and root reads become `psx_core_read` calls).

`Cd::steal()` sites, all three replaced by core calls: `platform.rs:46` (`READER`), `music.rs:322` (the
lookup reader), `music.rs:361` (`Player::new`, which keeps a `Cd` token but drives it through
`psx_core_cd`).

**Done now, behind the off-by-default `scene-core` feature (commit 0173d78):** the splash call is
compiled out (`quake.rs:218-226`). A plain-cargo `--features scene-core` link has no `psx_splash` or
`FontAtlas::upload` symbol. The default ship exe from 0173d78 is byte-identical to a7a1db4's (`cmp`).
The feature is not usable on a disc yet: nothing else shows the splash.

**TODO when `psx-core` exists** (the reader cannot go behind a feature before then: removing it means
calling the core, and there is no crate, `psx-core-abi.ld` or image to link):

1. `game/build.rs`: `-T psoxide-core.ld` after the ram-size file; `game/Cargo.toml`: `psx-core`
   (feature `link`) under `scene-core`.
2. `platform.rs`: `READER`, `begin_session`, `settle_drive` go. `ChunkStream::open_at` becomes
   `psx_core_client_attach` + `psx_core_stream_open(lba, 0)` (explicit SeekL; Quake has no
   console-proven implicit-seek log to keep), `ensure_sector` becomes `psx_core_stream_sector(SECTOR)`,
   `stop` becomes `psx_core_pause` + `psx_core_client_detach`. `read_storage_burst` and
   `fill_storage_cache` become `psx_core_read` (whole sectors; the burst keeps `SECTOR` as its bounce for
   the unaligned head and tail). `settle_after_load` becomes `psx_core_pause`.
3. `music.rs`: `suspend_for_load` keeps stopping the `Player` and calls `psx_core_music_yield` before
   the first data read; the lookup's `SectorReader` becomes two `psx_core_read`s (PVD, root).
4. Splash: `scene-core` on, and the boot scene (or a row's partner-credit flag) shows it. See the
   load-speed table for why Quake's own disc may want to keep its splash.
5. No exit row (CORE-ABI return scene 0xFFFF).

## Load speed (Manny 11-23: every load the same speed or faster)

No code or data that is resident today becomes a disc read: one Program row, and every read under the
core is a read Quake does today. Per-transition times are not measured anywhere in the repo; the
journeys rebase their tapes at the first `JOURNEY_STATE` publish so load time moves nothing in them
(`tests/journeys/README.md`), and the PASS json only has whole-route ticks.

| Transition | Disc read today | Under the core | Measured today | Slower? What keeps it equal |
|---|---|---|---|---|
| Boot to first frame (splash) | BIOS loads `PSX.EXE`, 261 payload sectors | the same plus the core image stored after `.data`, about 4 sectors at 8,192 B **(estimate)**, plus `__core_boot`'s copy and word FNV over 8 KB | not measured (no boot-to-splash figure) | **yes, slightly (estimate):** about 4 sectors, about 27 ms at double speed. With `scene-core` on and a separate boot-scene program showing the splash, add that program's whole load (the splash alone is 16,512 B linked, PLAN 3.3) and a seek back for `Music::probe`. Keeping the splash in Quake when Quake is the disc's first program keeps this to the 4 sectors |
| Music lookup | PVD (LBA 16) and one root sector, head next to the exe | the same two sectors via `psx_core_read` | not measured | no, as long as Quake is the first program to read |
| Boot assets | chunk 1 graphics 38 sectors, pack table 1, episode directory 1, global sound bank 79 | the same, blocking reads | not measured | no: same sectors; the core's 256-sector chunks never split them |
| Level load (Start, E1M1..E1M8) | one `ChunkStream` session per map chunk, 679 to 901 sectors, kept in one `ReadN` | stream-client session (`stream_open`/`stream_sector`) | not measured per load; whole routes: e1-chain 17,943 ticks, smoke-boot-play 1,507 (keep PASS json, journeys) | **only if it goes through `psx_core_read`:** a Pause and SeekL per small read was the dominant cost before `ChunkStream` (`platform.rs:805-808`). Through the stream client it is the same session |
| Overlay installs (monsters chunk 4, 8 sectors; doors chunk 5, 3 sectors), map sound bank | today's blocking reads after each load | `psx_core_read` | not measured | no: same sectors |
| Music stop before a load, settle after | `suspend_for_load` waits the Pause (`music.rs:203-221`); `settle_after_load` 400-poll settle | the core's proven Pause | not measured | not expected: the core's Pause proof replaces the settle; the console cost (R7, about a second per music/data switch) is the same either way |
| Save / Load from the pause menu | overlays 109 + 111 (4 + 6 sectors) or 110 + 111 (5 + 6) into the window, plus the card | the same reads via `psx_core_read` | e1m1-save-regress: 131 M cycles press to notice for a 1.6 KB save (`LIMITS.md:65`, card time dominates) | no. Note: this is a disc read from an in-play menu that exists today; keeping the card code out of the heap needs it (`LIMITS.md:67`). Nothing new is added |
| Death and respawn on the same map | none (residency hit) | none | | no |
| Pause, options, intermission | none | none | | no |

Console-only unknowns, listed and not requested (PLAN load-speed gate): whether the core's explicit
SeekL on every `stream_open` costs more than Quake's implicit seek on today's session open; the folded
checksum's pace on the drive.

## Tests, bars and goldens that move

| Check | Moves? |
|---|---|
| `[[size]] ship` (baseline text 498,896, data 35,320, bss 299,604, free 1,144,284) | yes: free 1,136,092 (-8,192); text, data, bss unchanged. Re-bless with the window as the reason |
| `[maps] ship` (baseline 637cfad0...) | yes: c609fc63... (the override line, the moved `__heap_end`, new paths) |
| `[[host]] heap-free` (blessed 41,900, tolerance 0, floor 8,192) | yes: 33,708; fails until re-blessed. The floor holds with 25,516 B to spare |
| ship-boot stack check (four fifths of 53,248) | no: 41,208, the same image; measured from the BIOS's 0x801FFFF0 (see the finding above) |
| `[[speed]] start-hall-walk`, library `speed-facts.json` | not expected: no text or data address moves. Not run. A relink restarts the library speed facts by the install rule |
| journeys (11) and `[cross]` smoke | not expected: `JOURNEY_STATE` and every `.bss` address keep their place. Not run |
| `e1m1-chain-bench` bus cycles (tolerance 8.7 M) | not expected (no layout move). Not run |
| other host regressions (`combat-regress` VRAM hash, `visual-parity` regions, ...) | not expected from the relink. A core that leaves VRAM resident (a font, a loading frame) breaks `combat-regress`'s hash of all VRAM outside the framebuffers (`host/quake-build/main.rs:6736-6745`) |
| `[clean]` (bin, cue, exe) | a rebuild of 0173d78 equals a7a1db4's exe (`cmp`); the bin and cue were not compared |
| goldens | none (every journey checkpoint is `golden = false`) |
| PGO layouts | none (Quake ships no layout) |
| install facts | yes: exe sha256 f4e5562b..., bin bca8d06b...; the library `provenance.json`, `install-stamp.json` and the demo disc launcher's required result (da4f38cf..., `launcher-6bfe/repo/psoxide-test.toml:99-101`) all move |
| docs | `docs/RAM.md:16,30` quote the old heap end 0x801f2f00; `tools/liquid-mips/src/main.rs:75,356` writes its own test exe with SP 0x801FFF00 (a separate tool, not the game) |
