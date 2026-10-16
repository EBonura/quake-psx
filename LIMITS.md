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
| `items/damage3`, `items/protect3`, `items/inv3` and `ogre/ogdrag` are not cooked. The code plays each one when its sample is present. | The SPU bank is full: the cooker reports `SPU high-water 0x7f580/0x80000`, 2,688 bytes of headroom, and these samples are 12,396 to 33,320 source bytes each. | `cargo quake-build assets` output; adding the first three exceeded the bank by `0x1c40`. `soldier/sattck1` and `soldier/pain1` do fit, because only maps with soldiers carry them and none is the worst-case map. |
| No `EF_DIMLIGHT`, `EF_BLUE`, `EF_RED` glow around the player for the quad and pentagram, and no `EF_ROCKET` light on a flying rocket. | One dynamic light slot (muzzle flash and explosions share it). | `crates/quake-core/src/effects.rs` header comment. |
| A newly picked up weapon flashes its status bar icon by alternating its lit and unlit icons ten times a second for a second, not through the five `inva*` frames. | The resident picture band is completely packed; only the plain and selected icon states are kept in RAM. | `tools/cfg/id1/picmap.txt` comment above `inv_shotgun`. |
| `PF_checkclient`'s PVS gate is replaced by a line-of-sight trace. | Line of sight implies the PVS test passes, so only sightlines through a PVS-only gap differ; the cooked maps carry no per-leaf visibility the monster think could afford to consult. | `crates/quake-core/src/monster.rs` think input; sight trace budget comment in `game/src/entity.rs`. |
| Blood, trail and explosion particle counts and the single dynamic light remain reduced. | Particle and light pools are fixed and small. | `crates/quake-core/src/effects.rs` header comment (unchanged by this pass). |

## Survival, view and messages

Added with the survival pass (`fix/quake-physics-triggers-2026-10-15`, player motor not included).

| Difference | Reason | Evidence |
| --- | --- | --- |
| Pain voices are the cooked subset (3 of 6), and `gasp2`, `drown1`, `lburn2` and `death2` to `death5` are absent. | The SPU bank holds only the samples cooked into it. | `crates/quake-core/src/survival.rs` comments; `SPU high-water` in the cooker output. |
| No `V_DriftPitch` / `SV_SetIdealPitch`. | The pad is always free look, which turns the original drift off (`cl.nodrift`). | `game/src/input_policy.rs`. |
| `trigger_hurt`, `trigger_push` and `trigger_gravity` are not implemented. | No Episode 1 map authors them (inventory census). | `COVERAGE.md`. |
| The map arena has no margin any more: `RESIDENT_MAP_ARENA_BYTES` is the largest map (`e1m3`, 866,122 bytes) rounded to 8. Every "arena margin" figure above predates it and the conclusions stand. | Shipping heap (`docs/RAM.md`). | `cargo quake-build check` prints the arena census. |
