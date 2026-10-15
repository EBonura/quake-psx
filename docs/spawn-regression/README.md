# E1M1 bridge grunts: before and after

Scene: the player stands on the west platform of E1M1's t2 bridge (the button side), facing east, on Normal skill, bridge still retracted. The three grunts on the east platform wake and run at the gap. Frames are the player's view at game tick 180; the lower-left numbers are health and armour.

| | `bridge-before.png` | `bridge-after.png` |
|---|---|---|
| Build | `main` at 4bd5e91 plus the spawn probe | this branch |

Resting positions after the rush (whole units; the platform edge is at x=320, so a hull-1 monster's box is fully supported from x=336):

| Grunt (cooked source) | before | after |
|---|---|---|
| 124 (patroller) | 306,2882 (box 30 over the pit) | 336,2853 |
| 125 (stand, faces the bridge) | 313,2810 (23 over) | 338,2800 |

Before the change a woken monster was moved with a fixed fan of headings and no `SV_CheckBottom`, so its box could straddle the ledge beside the bridge landing. The movement now follows `SV_movestep`, `SV_StepDirection`, `SV_NewChaseDir` and `SV_MoveToGoal`.

Reproduce: `cargo quake-build e1m1-spawn-regress --psoxide ../PSoXide-quake`. The guest prints one `SP` line per monster change (`game/src/spawn_probe.rs`); the host table of dropped spawn positions is `crates/quake-core/tests/e1m1_monster_spawn.rs`.
