# Episode 1 coverage

This is the current gameplay checklist for Quake 1.06 shareware. It describes
the Rust runtime in this repository, not the registered episodes.

## Maps and routes

| Map | Loads | Scripted route |
| --- | :---: | :---: |
| Start | Yes | Yes |
| E1M1 | Yes | Yes |
| E1M2 | Yes | Yes |
| E1M3 | Yes | Yes |
| E1M4 | Yes | Partial |
| E1M5 | Yes | Partial |
| E1M6 | Yes | Focused monster-jump test |
| E1M7 | Yes | Focused mechanism and boss tests |
| E1M8 | Yes | Load tests |

The map regression follows all ten Episode 1 transitions, including the E1M4
secret exit and the E1M7 return to Start. A complete player-driven route for
the whole episode is still missing.

## Player

Implemented:

- walking, acceleration, friction, air control and jumping;
- stairs, ramps, water movement and swimming;
- BSP, moving-brush and dynamic-body collision;
- health, armor, ammo, keys, runes and powerups;
- lava, slime, falling damage, drowning and environmental suits;
- death, respawn and level-to-level inventory;
- DualShock analog input and digital-controller fallback;
- configurable deadzone, brightness, HUD, water warp and translucent water.

Saving and loading the game on the memory card is implemented (below).
Multiplayer is not.

## Save and load

SAVE GAME is in the pause menu and LOAD GAME in both menus. Each opens a page
of three slots; every slot is its own memory-card file
(`BESLES-00000QPSX0001` to `0003`, card in port 1 first, then port 2), so the
BIOS card manager lists three QUAKE saves. A save holds the map, the skill and
the runes in hand, the player (position, velocity, view angles), the weapon
state (health, armour, ammunition, owned weapons, powerups, the loadout the
level was entered with, survival timers) and the scene's moving parts: every
monster's place, state, cooldowns and pack/ambush flags, the health of
everything damageable, which pickups and bodies are still there, how far each
door, button, lift and train has travelled, which triggers and teleports have
fired, the target graph, the secret and monster counters, the switchable light
styles, the event lightning. It is framed by a magic number, a format version
and an FNV-1a checksum; a card written by another build, or damaged, is refused
with SAVE NOT USABLE and leaves the game alone. A save is refused while the
player is dead (`Host_Savegame_f`). Loading from the title menu works, and
loading a save from another map goes through an ordinary level change.

Not saved (they restart with the map): projectiles in flight, particles and
decals, the sounds playing, the intermission. See `LIMITS.md`.

## Weapons and effects

All shareware weapons are available:

- Axe
- Shotgun
- Super Shotgun
- Nailgun
- Super Nailgun
- Grenade Launcher
- Rocket Launcher
- Lightning

The runtime includes hitscan spread, projectiles, splash damage, water
discharge, weapon switching, recoil, view-model animation, muzzle flashes,
blood, sparks, explosions and gibs.

Projectile and effect storage is fixed-size. When a pool is full, the game
drops the new effect instead of allocating memory during play.

## Monsters

| Monster | Present |
| --- | :---: |
| Soldier | Yes |
| Dog | Yes |
| Ogre | Yes |
| Zombie | Yes |
| Knight | Yes |
| Wizard | Yes |
| Shambler | Yes |
| Demon | Yes |
| Chthon | Yes |

Shared behavior includes target acquisition, walking, melee and ranged
attacks, pain, death, collision and authored sounds. Class-specific behavior
includes Zombie resurrection, Ogre grenades, Demon leaps, Wizard missiles,
Shambler lightning and Chthon's map-controlled death (E1M7).

Some details from desktop Quake remain incomplete, including several cosmetic
gib variations (see `LIMITS.md`). Monsters that appear only in the registered
game are outside this disc.

## World entities

Supported map behavior includes:

- doors and secret doors;
- buttons, lifts, platforms and trains;
- change-level and teleport triggers;
- relays, counters, delays, killtargets and target chains;
- difficulty and game-mode filters;
- once and multiple triggers (`trigger_hurt`, `trigger_push` and `trigger_gravity`
  are not implemented: no Episode 1 map uses them);
- secrets, messages and center prints;
- spikes, fireballs, lightning and explosive boxes;
- ambient sound sources and moving-brush sounds;
- E1M7's lightning mechanisms and Chthon sequence (Chthon is only in E1M7).

The cooker checks target names, teleport destinations, map limits and entity
references before writing the disc.

## Rendering and presentation

Implemented:

- BSP visibility and textured world surfaces;
- moving brush models;
- alias models and all Quake sprite orientations;
- layered Quake sky;
- turbulent water and PS1 transparency (on by default);
- Minimal and Classic HUD modes;
- menus, pause screen, screen blends and light styles;
- weapon models, pickups, monsters, particles and shadows;
- positional SPU sound and ambient XA music streamed from the disc.

Extreme projected surfaces can still show fixed-point seams or affine
distortion. These cases remain under investigation.

## Memory limits

The PS1 runtime avoids unbounded collections. Maps, entities, bodies, target
events, projectiles, particles, audio requests and draw packets all have
explicit limits checked either by the cooker or at runtime.

The standalone build also checks the final executable size, map arena usage,
SPU use, packet capacity and remaining heap during a normal release boot.

## Remaining work

1. Finish the player-driven routes for E1M4, E1M5, E1M6 and E1M8.
2. Run a complete Start-to-E1M8 episode test.
3. Continue renderer and gameplay performance work toward 30 fps.
4. Check the final standalone and demo-disc builds on original hardware.
5. Fix any hardware-only timing, audio or controller problems found there.
