# Deliberate PS1 limits

Differences from desktop Quake that are kept on purpose, each with the reason
and the evidence that the limit is real. Anything not listed here is a defect.

## Weapons, items and monsters

Added with the weapons and monsters pass (`fix/quake-weapons-monsters-2026-10-15`).

| Difference | Reason | Evidence |
| --- | --- | --- |
| Fixed projectile pools: 60 nails (shared with trap spikes), 8 rockets, 8 grenades. A shot is swallowed, with no ammo spent, while its pool is full. | No heap allocation in play. The pools are sized to the player's own cadence: nails fit exactly 6 s at 10 Hz, rocket and grenade pools are never filled by the player. | `crates/quake-core/src/combat.rs` `NAIL_POOL_CAPACITY` and its test; `COVERAGE.md` on dropped effects. |
| A rocket, grenade or nail does not explode on another projectile. | The id1 missiles are all `setsize (missile, '0 0 0', '0 0 0')`, so two of them can only touch by exact coincidence of two points; there is no observable behaviour to port. | `progs.dat`: `W_FireRocket`, `W_FireGrenade` and `launch_spike` each call `setsize` with `VEC_ORIGIN, VEC_ORIGIN`. |
| Super nails fly as the plain `spike.mdl`. | `s_spike.mdl` cooks to 7,808 bytes in every map (its skin is its own), against a resident-arena margin of 7,878 bytes on the largest map. | `cargo quake-build assets` with `progs/s_spike.mdl` added to `reslist.txt`: the start map grew from 1,464,923 to 1,472,731 bytes; `PSB5 resident arena: 866122 bytes required, 874000 reserved`. |
| A gibbed player does not throw `h_player.mdl`. | The head model is 14,708 source bytes and cooks to roughly the same 7 KB; it does not fit the 7,878 byte arena margin, and it is invisible from the first-person camera in any case. | Same arena margin as above; `h_player` is model `0x32` in `mdlmap.txt` and absent from `reslist.txt`. |
| `items/damage3`, `items/protect3`, `items/inv3` and `ogre/ogdrag` are not cooked. The code plays each one when its sample is present. | The SPU bank is full: the cooker reports `SPU high-water 0x7fb80/0x80000` (1,152 bytes of headroom after `player/pain2`, 102 of 104 slots on the worst map), and these samples are 12,396 to 33,320 source bytes each. | `cargo quake-build assets` output; adding the first three exceeded the bank by `0x1c40`. `soldier/sattck1` and `soldier/pain1` do fit, because only maps with soldiers carry them and none is the worst-case map. |
| The quad and pentagram light the player (`EF_DIMLIGHT`, 200 units) but with no `EF_BLUE`/`EF_RED` tint and without the original's `rand() & 31` radius jitter. That light, a flying rocket or lava ball (`EF_ROCKET`) and a firing monster (`EF_MUZZLEFLASH`) share the single slot, so they give way to a longer-lived player flash or explosion. | One dynamic light slot (muzzle flash and explosions share it). | `crates/quake-core/src/effects.rs` header comment. |
| A newly picked up weapon flashes its status bar icon by alternating its lit and unlit icons ten times a second for a second, not through the five `inva*` frames. | The resident picture band is completely packed; only the plain and selected icon states are kept in RAM. | `tools/cfg/id1/picmap.txt` comment above `inv_shotgun`. |
| `PF_checkclient`'s PVS gate is replaced by a line-of-sight trace. | Line of sight implies the PVS test passes, so only sightlines through a PVS-only gap differ; the cooked maps carry no per-leaf visibility the monster think could afford to consult. | `crates/quake-core/src/monster.rs` think input; sight trace budget comment in `game/src/entity.rs`. |
| Blood, trail and explosion particle counts and the single dynamic light remain reduced. | Particle and light pools are fixed and small. | `crates/quake-core/src/effects.rs` header comment (unchanged by this pass). |

## Survival, view and messages

Added with the survival pass (`fix/quake-physics-triggers-2026-10-15`, player motor not included).

| Difference | Reason | Evidence |
| --- | --- | --- |
| Pain voices are the cooked subset (4 of 6: pain1, pain2, pain3, pain6), and `gasp2`, `drown1`, `lburn2` and `death2` to `death5` are absent. | The SPU bank holds only the samples cooked into it. | `crates/quake-core/src/survival.rs` comments; `SPU high-water` in the cooker output. |
| No `V_DriftPitch` / `SV_SetIdealPitch`. | The pad is always free look, which turns the original drift off (`cl.nodrift`). | `game/src/input_policy.rs`. |
| `trigger_hurt`, `trigger_push` and `trigger_gravity` are not implemented. | No Episode 1 map authors them (inventory census). | `COVERAGE.md`. |
| The map arena has no margin any more: `RESIDENT_MAP_ARENA_BYTES` is the largest map (`e1m3`, 866,122 bytes) rounded to 8. Every "arena margin" figure above predates it and the conclusions stand. | Shipping heap (`docs/RAM.md`). | `cargo quake-build check` prints the arena census. |

## Inventory pass 2 (2026-10-16)

Added with `fix/quake-inventory-2-2026-10-16`. The rows of the 2026-10-15 inventory (parts 04 and 05) are triaged one by one in the
lane notes; these are the differences that stay.

| Difference | Reason | Evidence |
| --- | --- | --- |
| A player standing at the foot of a sixteen-unit rise whose riser leans (E1M1 has three, plane normal z 0.124) steps onto it from a standing start. Quake never tries a step there (`SV_FlyMove` marks a step only for `normal[2] == 0`). | The base motor already tries a step on every blocked move; at under one unit a tick it landed on the lean again and the player flickered in place. The retry is bounded, additive and leaves the velocity alone. | `crates/quake-core/tests/e1m1_leaning_riser.rs`; `tools/routesim` `ramps` and `walk`; hull-1 traced from the original `e1m1.bsp` gives the same plane. |
| The end-of-level panel waits for a button in the shipping build. Builds with emulator telemetry (every gate) cross it after 2.5 s. | `IntermissionThink` waits for a button; a headless run cannot press one. | `game/src/quake.rs` `WAIT_FOR_BUTTON`. |
| The E1M7 end chain does not run under the intermission panel; the panel is text. | The chain's effects (the finale relay) are hidden by the panel, and `complete.lmp`/`inter.lmp` do not fit the picture band. | `game/src/quake.rs` `begin_intermission`. |
| Torches and flames do not flicker. | The original does not either: no E1 torch or flame has an animated light style near it (BSP census: styles 1, 6 and 10 appear only in START, 10 in E1M1, 2 in E1M5 and E1M6, 32+ are switches). The quake-srp flicker is an enhancement that needs a cook-time torch channel. | Lane notes. |
| No `fire1` torch crackle, `fl_hum1`/`buzz1` tube hum, or water/wind leaf ambience; no key-use, extra door/button/pain/death/ricochet/powerup samples. | SPU. At the drip/hum rate class `fire1` is 3,712 bytes, `fl_hum1` 7,760, `buzz1` 5,616, `water1` 8,640, `wind2` 4,432, `inv3` 5,184, `protect3` 3,632, `meduse` 1,664, `runeuse` 2,688, against 1,152 bytes free on the worst map and 2 of 104 bank slots (after `player/pain2`); E1 also has 193 flame and torch entities against 32 ambient sources. | Sound lengths read from PAK0; the cooker's `SPU high-water 0x7fb80/0x80000`. |
| Teleport departure fog and sound are not drawn. | They happen behind the new viewpoint and attenuated; tried, no visible effect. | Lane notes. |

## Inventory pass 3 (2026-10-17)

Added with `fix/quake-inventory-3-2026-10-17`; what stays different after the rows it closed.

| Difference | Reason | Evidence |
| --- | --- | --- |
| The intermission camera is chosen from the map's `info_intermission` list by the level's seconds plus kills, not by `random()`; builds with emulator telemetry (every gate) always take the first one. | No random source in the guest outside the combat rolls; the pick only needs to vary between plays. The panel's frames are inside the `e1m1-chain-bench` window and the camera decides how much of the level they draw (30.478 against 31.0 fps with the chain route's own camera pick, same code otherwise). | `game/src/entity.rs` `intermission_spot`. |
| A wizard's spike wall impact is the shared grey six-dot puff, silent (no `wizard/hit`), and its `EF_TRACER` trail is a straight line of olive dots without the original's sideways zigzag. | Particle pool shape (`ParticleKind` ramps), and the bank has no slot for `wizard/hit` (2 of 104 left). | `crates/quake-core/src/effects.rs`. |
| A door chain's message is printed only from a member that carries it. In the original `door_touch` reads the chain master's text, which `LinkDoors` copies in from any member (E1M4 raw #135 and E1M6 #67 differ). | Not done: a per-mover message index costs about 150 bytes of image for two doors. | Lane notes. |
| `multi_touch`'s facing test is not modelled (START hall triggers with an angle and the E1M1 exit trigger fire from any direction). | Needs the player's yaw in the trigger pass; message-only triggers. | Lane notes. |

