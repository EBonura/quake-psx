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
| E1M8 | Yes | Load and boss tests |

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

Save/load and multiplayer are not implemented.

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
Shambler lightning and Chthon's map-controlled death.

Some details from desktop Quake remain incomplete, including full infighting,
all patrol behavior and several cosmetic gib variations. Monsters that appear
only in the registered game are outside this disc.

## World entities

Supported map behavior includes:

- doors and secret doors;
- buttons, lifts, platforms and trains;
- change-level and teleport triggers;
- relays, counters, delays, killtargets and target chains;
- difficulty and game-mode filters;
- hurt, push, once and multiple triggers;
- secrets, messages and center prints;
- spikes, fireballs, lightning and explosive boxes;
- ambient sound sources and moving-brush sounds;
- E1M7 lightning mechanisms and the E1M8 Chthon sequence.

The cooker checks target names, teleport destinations, map limits and entity
references before writing the disc.

## Rendering and presentation

Implemented:

- BSP visibility and textured world surfaces;
- moving brush models;
- alias models and all Quake sprite orientations;
- layered Quake sky;
- turbulent water and optional PS1 transparency;
- Minimal and Classic HUD modes;
- menus, pause screen, screen blends and light styles;
- weapon models, pickups, monsters, particles and shadows;
- positional SPU sound and optional user-supplied CD audio.

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
