//! Rust-owned Quake runtime.
//!
//! This module is the root of the shipping game. The target contains no
//! foreign game objects or native-source compatibility layer.

use quake_core::level_session::{CenterprintText, LevelPresentation};

/// The original holds a centerprint for two seconds.
const CENTERPRINT_TICKS: u16 = 120;

/// How long the end-of-level panel holds before the next map loads.
///
/// The original waits for `+attack` after a minimum dwell. A console port
/// cannot stall a headless run on a button, so the dwell is bounded and a
/// press only skips the remainder. Every gate therefore crosses the panel
/// deterministically without pressing anything.
const INTERMISSION_TICKS: u16 = 150;
/// Earliest tick a press may skip the panel.
const INTERMISSION_SKIP_TICKS: u16 = 30;

/// The end-of-level panel between two maps.
struct Intermission {
    view: quake_core::level::IntermissionView,
    next: crate::asset::EpisodeMap,
    camera: crate::renderer::Camera,
    remaining: u16,
    elapsed: u16,
}

/// Boot the PSoXide devices and own the game loop forever.
///
/// Size-optimising this orchestration function reclaims six 2 KiB PS-X EXE
/// sectors while leaving the renderer and gameplay workers at speed. The
/// fixed-step E1M1 route measured 22.201 fps versus 22.221 without it, well
/// inside the established 0.122 fps code-layout band.
#[optimize(size)]
pub fn run() -> ! {
    // Match VoXide's hardware-proven ordering exactly: initialise the GPU,
    // complete the DualShock 0x43/0x44 exchange, and only then enable VBlank
    // interrupts. An interrupt in the middle of that exchange can leave a
    // capable pad reporting digital mode for the rest of the session.
    crate::platform::gpu_init_before_interrupts();
    #[cfg(not(any(feature = "episode1-regression", feature = "hardware-regression")))]
    {
        let _ = psx_pad::enable_analog_port1();
    }
    crate::platform::start_vblank_counter();
    psx_spu::init();
    // Route the CD's own audio into the mix, for the disc music `music.rs`
    // borrows; `psx_spu::init` leaves the CD input muted. Half scale rather
    // than the SDK's maximum: Quake keeps the main volume at `Volume::MAX` and
    // plays its one-shots at `Volume::linear(1, 2)`, so CD at full scale would
    // put the music above every gunshot and under all twelve dynamic voices
    // plus eleven ambient loops at once. This is the one constant to turn if
    // the balance reads wrong on a console.
    psx_spu::set_cd_volume(
        psx_spu::CdVolume::linear(1, 2),
        psx_spu::CdVolume::linear(1, 2),
    );
    psx_spu::enable_cd_audio(true);
    // The shared PSoXide boot intro. Route regressions and the fixed-step
    // bench drive the pad from boot on a tick schedule, so they skip it.
    #[cfg(not(any(
        feature = "emulator-telemetry",
        feature = "ambient-regression",
        feature = "hardware-regression",
        feature = "perf-fixed-ticks",
        feature = "perf-fixed-30hz"
    )))]
    crate::intro::show(crate::platform::boot_framebuffer());

    // One fixed-capacity allocation is reused across every Episode 1 map. The
    // PSoXide target allocator is intentionally monotonic, so allowing each
    // transition to grow a fresh collection would eventually exhaust PS1 RAM.
    let mut world = crate::asset::ResidentMap::new();
    if world.load_graphics().is_err() {
        psx_rt::tty::println("quake-psx: Rust graphics load failed");
        psx_rt::halt();
    }
    let mut entities = crate::entity::EntityScene::new();
    let mut audio = crate::audio::AudioBank::new();
    // Silent until it has probed the drive, which it does once the game loop is
    // running: the boot loads below own the drive until then.
    let mut music = crate::music::Music::new();
    let mut presentation = LevelPresentation::new();
    // `cl_dlights`. Held beside the presentation pools rather than inside
    // them because only the renderer ever reads it, and it follows the same
    // gameplay-session token they do.
    let mut dynamic_lights = quake_core::effects::DynamicLights::new();
    crate::platform::configure_quake_projection();
    let mut renderer = crate::renderer::Renderer::new();
    let boot_map = initial_map();
    let Some(loading_picture) = world.picture(quake_formats::GraphicsPictureId::Disc) else {
        psx_rt::tty::println("quake-psx: Rust loading picture is missing");
        psx_rt::halt();
    };
    let global_audio = quake_core::loading::present_before_payload(
        || renderer.draw_loading(loading_picture, boot_map),
        || {
            let mut stream_scratch = world.take_stream_scratch();
            let result = audio.load_global(&mut stream_scratch);
            world.restore_stream_scratch(stream_scratch);
            result
        },
    );
    if global_audio.is_err() {
        psx_rt::tty::println("quake-psx: Rust global sound load failed");
        psx_rt::halt();
    }
    let Some(mut player) = load_level(
        &mut world,
        boot_map,
        &mut entities,
        &mut audio,
        &mut music,
        &mut renderer,
        &mut presentation,
    ) else {
        psx_rt::tty::println("quake-psx: Rust initial level load failed");
        psx_rt::halt();
    };
    let mut weapon = quake_core::combat::WeaponState::new();
    #[cfg(feature = "arsenal-regression")]
    if !crate::arsenal_regression::setup(&world, &entities, &mut player) {
        psx_rt::tty::println("quake-psx: Rust arsenal regression setup failed");
        psx_rt::halt();
    }
    #[cfg(feature = "combat-regression")]
    if !crate::combat_regression::setup(&world, &mut entities, &mut player) {
        psx_rt::tty::println("quake-psx: Rust combat regression setup failed");
        psx_rt::halt();
    }
    #[cfg(feature = "monster-regression")]
    if !crate::monster_regression::setup(&world, &entities, &mut player) {
        psx_rt::tty::println("quake-psx: Rust monster regression setup failed");
        psx_rt::halt();
    }
    #[cfg(feature = "monsterjump-regression")]
    if !crate::monsterjump_regression::setup(&world, &mut entities, &mut player) {
        psx_rt::tty::println("quake-psx: Rust monster-jump regression setup failed");
        psx_rt::halt();
    }
    #[cfg(feature = "bestiary-regression")]
    if !crate::bestiary_regression::setup(&world, &entities, &player) {
        psx_rt::tty::println("quake-psx: Rust bestiary regression setup failed");
        psx_rt::halt();
    }
    #[cfg(feature = "survival-regression")]
    if !crate::survival_regression::setup(&world) {
        psx_rt::tty::println("quake-psx: Rust survival regression setup failed");
        psx_rt::halt();
    }
    #[cfg(feature = "ambient-regression")]
    {
        let Some(origin) = audio.regression_ambient_origin() else {
            psx_rt::tty::println("quake-psx: Rust ambient regression found no emitter");
            psx_rt::halt();
        };
        let camera = player.camera();
        audio.spatialize(origin, camera.angles[1]);
    }
    psx_rt::tty::println("quake-psx: Rust Start map resident");

    let mut input = crate::input::Input::new();
    let mut menu = quake_core::menu::Menu::new();
    #[cfg(any(
        feature = "episode1-regression",
        feature = "ambient-regression",
        feature = "combat-regression",
        feature = "arsenal-regression",
        feature = "monster-regression",
        feature = "monsterjump-regression",
        feature = "bestiary-regression",
        feature = "start-route-regression",
        feature = "e1m1-chain-regression",
        feature = "e1m2-e1m3-route-regression",
        feature = "survival-regression",
        feature = "episode1-route-regression",
        feature = "systems-regression",
        feature = "visual-parity-regression",
        feature = "hardware-regression"
    ))]
    menu.close_for_game();
    let mut intermission: Option<Intermission> = None;
    let mut movement_tick = psx_rt::interrupts::vblank_count();
    // `cl.faceanimtime`, held beside the loop that owns the damage signal.
    let mut pain_face = quake_core::hud::PainFaceTimer::new();

    loop {
        #[cfg(not(any(feature = "perf-fixed-ticks", feature = "perf-fixed-30hz")))]
        let audio_tick = psx_rt::interrupts::vblank_count();
        // Performance benchmark: the animation clock (liquid warp phase, light
        // styles, sound scheduling) also advances three ticks per frame, so a
        // frame dump at frame N is the same picture on any build.
        #[cfg(feature = "perf-fixed-ticks")]
        let audio_tick = {
            static mut FIXED_TICK: u32 = 0;
            // SAFETY: the game loop is single-threaded and this is the only
            // access.
            unsafe {
                FIXED_TICK = FIXED_TICK.wrapping_add(3);
                FIXED_TICK
            }
        };
        // Thirty-Hz control: keep the route deterministic while charging the
        // two simulation ticks a stable NTSC 30 fps frame actually advances.
        #[cfg(feature = "perf-fixed-30hz")]
        let audio_tick = {
            static mut FIXED_TICK_30HZ: u32 = 0;
            // SAFETY: the game loop is single-threaded and this is the only
            // access.
            unsafe {
                FIXED_TICK_30HZ = FIXED_TICK_30HZ.wrapping_add(2);
                FIXED_TICK_30HZ
            }
        };
        let elapsed_ticks = audio_tick.wrapping_sub(movement_tick).clamp(1, 4) as u16;
        // The visual oracle must sample the same simulated instant when a
        // renderer optimization changes how many VBlanks one frame spans.
        // Shipping continues to consume the measured 60-Hz delta; only this
        // fixed-camera diagnostic advances gameplay by one tick per frame.
        #[cfg(feature = "visual-parity-regression")]
        let elapsed_ticks = 1;
        // Performance benchmark: a fixed step keeps the scripted route and
        // its scene sequence identical across builds of differing speed.
        #[cfg(feature = "perf-fixed-ticks")]
        let elapsed_ticks = 3;
        #[cfg(feature = "perf-fixed-30hz")]
        let elapsed_ticks = 2;
        movement_tick = audio_tick;
        presentation.explosion_effects_mut().tick(elapsed_ticks);
        presentation.impact_particles_mut().tick(elapsed_ticks);
        // `enter_map` is a no-op until the session token moves, so this is
        // also the one place a level load puts the last map's lights out.
        dynamic_lights.enter_map(presentation.generation());
        dynamic_lights.tick(elapsed_ticks);
        audio.tick(audio_tick);
        // The drive is the authority on what is playing: it advances at the end
        // of a song and at every level load, so the Options rows are restated
        // from it here and only read back after the player has had a turn.
        music.update(audio_tick);
        menu.sync_music(music.available(), music.enabled(), music.track());
        let raw_controls = input.poll(menu.deadzone_radius());
        if let Some(active) = intermission.as_mut() {
            active.elapsed = active.elapsed.saturating_add(elapsed_ticks);
            active.remaining = active.remaining.saturating_sub(elapsed_ticks);
            // The panel is the one screen the blends were never ticked on, so
            // a flash from the last gameplay frame used to hang there. This
            // also drives the port's transition fade below.
            presentation.screen_blend_mut().tick(elapsed_ticks);
            // PORT ADDITION, not id1: darken into the level change the panel
            // leads to. The original goes straight from panel to next map.
            if active.remaining <= quake_core::screenblend::ScreenBlend::TRANSITION_TICKS {
                presentation.screen_blend_mut().fade_out_to_black();
            }
            let skipped = active.elapsed >= INTERMISSION_SKIP_TICKS
                && raw_controls.pressed
                    & (psx_pad::button::CROSS
                        | psx_pad::button::START
                        | psx_pad::button::SQUARE
                        | psx_pad::button::CIRCLE)
                    != 0;
            // `ExitIntermission` runs the episode's closing text between the
            // panel and `GotoNextMap`; each page waits like the panel does.
            if (active.remaining == 0 || skipped)
                && active.view.episode != quake_core::level::IntermissionView::EPISODE_NONE
                && active.view.episode < quake_core::level::IntermissionView::FINALE_LAST
            {
                active.view.episode += 1;
                active.remaining = INTERMISSION_TICKS;
                active.elapsed = 0;
                // Port addition: the finale pages cross through black too.
                presentation.screen_blend_mut().fade_in_from_black();
            } else if active.remaining == 0 || skipped {
                let destination = active.next;
                intermission = None;
                let Some(next_player) = load_level(
                    &mut world,
                    destination,
                    &mut entities,
                    &mut audio,
                    &mut music,
                    &mut renderer,
                    &mut presentation,
                ) else {
                    psx_rt::tty::println("quake-psx: Rust level transition failed");
                    psx_rt::halt();
                };
                player = next_player;
                weapon.map_loaded();
                movement_tick = psx_rt::interrupts::vblank_count();
                continue;
            }
            let camera = active.camera;
            let view = active.view;
            entities.update();
            audio.spatialize(camera.origin, camera.angles[1]);
            entities.animate_lights(&world, audio_tick);
            renderer.set_light_styles(entities.light_styles());
            renderer.set_dynamic_lights(&dynamic_lights);
            #[cfg(feature = "renderer-owned-sections")]
            if world
                .ensure_render_section_for_point(camera.origin)
                .is_err()
            {
                psx_rt::tty::println("quake-psx: render section activation failed");
                psx_rt::halt();
            }
            let _ = renderer.draw_frame(
                &world,
                camera,
                audio_tick,
                false,
                false,
                entities.entities(),
                None,
                presentation.explosion_effects().active(),
                presentation.impact_particles().active(),
                entities.rotating_yaw(),
                None,
                None,
                None,
                None,
                Some(view),
                music.now_playing(audio_tick),
                presentation.screen_blend(),
            );
            continue;
        }
        crate::renderer::set_brightness_level(menu.view().brightness);
        crate::renderer::set_crosshair(menu.view().crosshair);
        crate::renderer::set_hud_mode(menu.view().hud_mode);
        let opened_pause = !menu.active() && raw_controls.menu_pressed();
        if opened_pause {
            menu.open_pause();
        }
        // Do not feed the edge which opened the pause menu straight back as
        // its Back action; the menu begins consuming input next frame.
        let menu_action = if menu.active() && !opened_pause {
            menu.update(menu_input(raw_controls))
        } else {
            None
        };
        let menu_view = menu.view();
        // Apply a changed SFX level before menu3.wav so the adjustment cue
        // itself demonstrates the new setting, as in the original.
        audio.set_sfx_volume(menu_view.sound_volume);
        match menu_action {
            Some(
                action @ (quake_core::menu::MenuAction::NewGame
                | quake_core::menu::MenuAction::StartLevel(_)),
            ) => {
                let map = match action {
                    quake_core::menu::MenuAction::StartLevel(index) => {
                        crate::asset::EpisodeMap::ALL
                            [usize::from(index).min(crate::asset::EpisodeMap::ALL.len() - 1)]
                    }
                    _ => crate::asset::EpisodeMap::Start,
                };
                entities.reset_game(menu.view().skill);
                let Some(next_player) = load_level(
                    &mut world,
                    map,
                    &mut entities,
                    &mut audio,
                    &mut music,
                    &mut renderer,
                    &mut presentation,
                ) else {
                    psx_rt::tty::println("quake-psx: Rust new game load failed");
                    psx_rt::halt();
                };
                player = next_player;
                weapon = quake_core::combat::WeaponState::new();
                movement_tick = psx_rt::interrupts::vblank_count();
            }
            Some(quake_core::menu::MenuAction::Impulse9) => weapon.impulse_nine(),
            Some(quake_core::menu::MenuAction::Resume) | None => {}
        }
        weapon.set_god_mode(menu_view.god_mode);
        // A level action reloads and resets the SPU voices above, so consume
        // its deferred enter cue only after that reset. Page moves and slider
        // changes reach here in the same frame as their input.
        if let Some(sound) = menu.take_sound() {
            let id = match sound {
                quake_core::menu::MenuSound::Move => 0x6e,
                quake_core::menu::MenuSound::Enter => 0x6f,
                quake_core::menu::MenuSound::Adjust => 0x70,
            };
            let _ = audio.play_one_shot(id, audio_tick);
        }
        let menu_view = menu.view();
        music.apply_menu(
            menu_view.music_on,
            menu_view.track,
            menu_view.music_volume,
            audio_tick,
        );
        #[cfg(not(any(
            feature = "start-route-regression",
            feature = "e1m1-chain-regression",
            feature = "e1m2-e1m3-route-regression",
            feature = "bestiary-regression",
            feature = "monsterjump-regression",
            feature = "survival-regression",
            feature = "episode1-route-regression",
            feature = "systems-regression"
        )))]
        let controls = apply_look_settings(raw_controls, &menu);
        #[cfg(feature = "bestiary-regression")]
        let controls = crate::bestiary_regression::controls(&world, &entities, &player);
        #[cfg(feature = "monsterjump-regression")]
        let controls = crate::monsterjump_regression::controls();
        #[cfg(feature = "systems-regression")]
        let controls = crate::systems_regression::controls();
        #[cfg(feature = "start-route-regression")]
        let controls = crate::start_route_regression::controls(world.map(), &player);
        #[cfg(feature = "e1m1-chain-regression")]
        let controls = crate::e1m1_chain_regression::controls(world.map(), &player);
        #[cfg(feature = "e1m2-e1m3-route-regression")]
        let controls =
            crate::e1m2_e1m3_route_regression::controls(world.map(), &entities, &player, &weapon);
        #[cfg(feature = "survival-regression")]
        let controls = crate::survival_regression::controls(world.map(), &player, &weapon);
        #[cfg(feature = "episode1-route-regression")]
        let controls = crate::episode1_regression::controls(&world, &entities, &player);

        if menu.active() {
            let view = menu.view();
            entities.update();
            let camera = player.camera();
            audio.spatialize(camera.origin, camera.angles[1]);
            entities.animate_lights(&world, audio_tick);
            renderer.set_light_styles(entities.light_styles());
            renderer.set_dynamic_lights(&dynamic_lights);
            #[cfg(feature = "renderer-owned-sections")]
            if world
                .ensure_render_section_for_point(camera.origin)
                .is_err()
            {
                psx_rt::tty::println("quake-psx: render section activation failed");
                psx_rt::halt();
            }
            let _ = renderer.draw_frame(
                &world,
                camera,
                audio_tick,
                view.water_warp && player.water_level() == 3,
                view.water_alpha,
                entities.entities(),
                None,
                presentation.explosion_effects().active(),
                presentation.impact_particles().active(),
                entities.rotating_yaw(),
                None,
                None,
                None,
                Some(view),
                None,
                music.now_playing(audio_tick),
                presentation.screen_blend(),
            );
            continue;
        }
        #[cfg(not(any(
            feature = "ambient-regression",
            feature = "episode1-regression",
            feature = "combat-regression",
            feature = "arsenal-regression",
            feature = "monster-regression",
            feature = "monsterjump-regression"
        )))]
        let player_frame = if weapon.inventory().health() > 0 {
            if menu.view().noclip {
                player.update_noclip(controls, elapsed_ticks)
            } else {
                player.update(&world, &entities, controls, elapsed_ticks)
            }
        } else {
            crate::player::PlayerFrame::default()
        };
        // Deterministic route/audio probes park the player in a trigger or on
        // an ambient source; gravity would turn those focused probes into a
        // second, timing-sensitive movement test.
        #[cfg(any(
            feature = "ambient-regression",
            feature = "episode1-regression",
            feature = "combat-regression",
            feature = "arsenal-regression",
            feature = "monster-regression",
            feature = "monsterjump-regression"
        ))]
        let player_frame = crate::player::PlayerFrame::default();
        #[cfg(feature = "arsenal-regression")]
        crate::arsenal_regression::prepare(&world, &mut entities, &mut player, &mut weapon);
        #[cfg(feature = "monster-regression")]
        crate::monster_regression::prepare(&world, &mut entities, &mut player, &mut weapon);
        player.set_dead(weapon.inventory().health() <= 0);
        // `V_ParseDamage` runs off the damage message the server sends. This
        // port has no message layer, so the same signal is taken from the
        // inventory either side of every damage source in the frame: contents,
        // crush, monsters, projectiles and falls all land in one place.
        let health_before = weapon.inventory().health().max(0) as u16;
        let armor_before = weapon.inventory().armor().max(0) as u16;
        #[cfg(not(feature = "visual-parity-regression"))]
        let camera = player.camera();
        #[cfg(feature = "visual-parity-regression")]
        let camera = crate::visual_parity_regression::camera();
        if player_frame.listener_changed {
            audio.spatialize(camera.origin, camera.angles[1]);
        }
        play_movement_audio(&mut audio, player_frame, audio_tick);
        let survival = weapon.tick_survival(survival_input(player_frame, elapsed_ticks));
        #[cfg(feature = "survival-regression")]
        crate::survival_regression::observe(player_frame, survival, &weapon);
        for &sound in survival.sounds() {
            if audio.contains(sound) {
                let _ = audio.play_one_shot_on(sound, PLAYER_VOICE, audio_tick);
            }
        }
        // `CheckPowerups`: `stuffcmd(self, "bf\n")` once a second while a
        // powerup is in its last three seconds.
        if survival.bonus_flash {
            presentation.screen_blend_mut().pick_up();
        }
        // `DeathSound`'s submerged branch also calls `DeathBubbles(20)`. The
        // fixed pool renders the original sprite with a bounded five-bubble
        // burst; see `spawn_death_bubbles` for the remaining timing tradeoff.
        if survival.death_bubbles {
            presentation
                .impact_particles_mut()
                .spawn_death_bubbles(player.origin());
        }
        // `GibPlayer`: the corpse bursts around the dead view.
        if survival.gibbed {
            entities.gib_player(player.origin(), weapon.inventory().health());
        }
        // `PlayerDeathThink` waits for every button to be released and then
        // respawns on the next attack, jump, or use press. Edge-triggered
        // input gives the release half for free. Single player restarts the
        // current level, so this reuses the ordinary map-load path with the
        // one difference that matters: the inventory does not carry over.
        if weapon.inventory().health() <= 0
            && (controls.fire_pressed() || controls.jump_pressed() || controls.use_pressed())
        {
            let restart = world.map();
            let Some(next_player) = load_level(
                &mut world,
                restart,
                &mut entities,
                &mut audio,
                &mut music,
                &mut renderer,
                &mut presentation,
            ) else {
                psx_rt::tty::println("quake-psx: Rust respawn level load failed");
                psx_rt::halt();
            };
            player = next_player;
            weapon.respawn();
            movement_tick = psx_rt::interrupts::vblank_count();
            continue;
        }
        #[cfg(feature = "episode1-regression")]
        if let Some(destination) = crate::regression::drive(&world, &entities, &mut player) {
            let Some(next_player) = load_level(
                &mut world,
                destination,
                &mut entities,
                &mut audio,
                &mut music,
                &mut renderer,
                &mut presentation,
            ) else {
                psx_rt::tty::println("quake-psx: Rust regression map load failed");
                psx_rt::halt();
            };
            player = next_player;
            weapon.map_loaded();
            movement_tick = psx_rt::interrupts::vblank_count();
            continue;
        }
        #[cfg(feature = "bestiary-regression")]
        if let Some(destination) = crate::bestiary_regression::requested_map() {
            if destination != world.map() {
                let Some(next_player) = load_level(
                    &mut world,
                    destination,
                    &mut entities,
                    &mut audio,
                    &mut music,
                    &mut renderer,
                    &mut presentation,
                ) else {
                    psx_rt::tty::println("quake-psx: Rust bestiary stage load failed");
                    psx_rt::halt();
                };
                player = next_player;
                weapon = quake_core::combat::WeaponState::new();
                if !crate::bestiary_regression::setup(&world, &entities, &player) {
                    psx_rt::tty::println("quake-psx: Rust bestiary stage setup failed");
                    psx_rt::halt();
                }
                movement_tick = psx_rt::interrupts::vblank_count();
                continue;
            }
        }
        let mut rider = player.rider();
        let mut gameplay = entities.update_gameplay(
            &world,
            &mut rider,
            controls.use_pressed(),
            weapon.inventory().keys(),
            elapsed_ticks,
        );
        // `SV_PushMove` moved the player before this frame's triggers were
        // done with it, so take the carry back before anything reads the box.
        if rider.carried {
            player.carry_to(rider.origin);
        }
        let (mut player_mins, mut player_maxs) = player.bounds();
        if let Some(bit) = gameplay.consumed_key {
            weapon.take_key(bit);
        }
        if let Some(destination) = gameplay.teleport {
            player.apply_teleport(destination);
            (player_mins, player_maxs) = player.bounds();
            let camera = player.camera();
            audio.spatialize(camera.origin, camera.angles[1]);
            // `spawn_tfog`'s `TE_TELEPORT`, one ring instead of 896 particles.
            presentation.impact_particles_mut().spawn_ring(
                destination.origin,
                quake_core::effects::ParticleKind::Spark,
                16,
            );
            // `teleport_touch` plays one of five `misc/r_tele1..5` flashes
            // (0x74..=0x78); a hash of the tick stands in for `random`, and
            // the bank falls back to r_tele3 when the others are not cooked
            // (the SPU banks only have room for that one today).
            // SILENT gates both this flash and the virtualised static hum;
            // non-silent trigger brushes continuously hum from their center.
            let flash = 0x74 + ((audio_tick.wrapping_mul(0x9e37_79b9) >> 16) % 5) as i16;
            if !destination.silent {
                let flash = if audio.contains(flash) { flash } else { 0x76 };
                play_sound_event(
                    &mut audio,
                    crate::entity::SoundEvent::at(flash, destination.origin),
                    audio_tick,
                );
            }
        }
        // `changelevel_touch` runs `SUB_UseTargets` before it removes its
        // touch function and schedules the transition. E1M7 uses that edge to
        // fan out through its authored finale relay.
        let pending_change_level = entities.touched_change_level_trigger(player_mins, player_maxs);
        if let Some((_, _, source_index)) = pending_change_level {
            let _edges = entities.fire_change_level_targets(&world, source_index, &mut gameplay);
            #[cfg(feature = "episode1-route-regression")]
            crate::episode1_regression::observe_changelevel_targets(&world, source_index, _edges);
        }
        if gameplay.crush_damage != 0 {
            weapon.take_damage(gameplay.crush_damage.min(i16::MAX as u16) as i16);
        }
        if let Some(bit) = gameplay.needs_key {
            // `door_touch`'s noise3, cooked from the worldtype-selected
            // medtry/runetry/basetry triple under the medtry id.
            if audio.contains(0x39) {
                let _ = audio.play_one_shot(0x39, audio_tick);
            }
            presentation.set_centerprint(
                CenterprintText::Fixed(quake_core::door::needs_key_message(bit)),
                CENTERPRINT_TICKS,
            );
        }
        // `counter_use` prints its countdown before `multi_trigger` gets to
        // the authored message below, so an authored line on the same frame
        // still wins the panel.
        if let Some(text) = gameplay.counter_message {
            presentation.set_centerprint(CenterprintText::Fixed(text), CENTERPRINT_TICKS);
        }
        if let Some(source_index) = gameplay.message_source {
            let source = world
                .entities()
                .get(source_index as usize)
                .unwrap_or_default();
            let text = if source.string != 0 {
                Some(CenterprintText::Cooked(source.string))
            } else if source.class_name == 0x50 {
                // `trigger_secret` defaults its own message at spawn.
                Some(CenterprintText::Fixed(quake_core::secrets::SECRET_MESSAGE))
            } else {
                None
            };
            if let Some(text) = text {
                presentation.set_centerprint(text, CENTERPRINT_TICKS);
            }
        }
        if let Some(sound) = gameplay.message_sound {
            play_sound_event(&mut audio, sound, audio_tick);
        }
        for &sound in gameplay.mover_sounds() {
            play_sound_event(&mut audio, sound, audio_tick);
        }
        presentation.tick_centerprint(elapsed_ticks);
        // Only gameplay frames reach here, so the intermission panel prints
        // time actually spent playing the level.
        presentation.tick_level_clock(elapsed_ticks);
        if let Some(sound) = gameplay.train_sound {
            play_sound_event(&mut audio, sound, audio_tick);
        }
        // `boss_pain` and `boss_death1`. The shock chain has been raising and
        // killing Chthon in silence: the voice was picked but never played.
        if let Some(sound) = gameplay.boss_shock_sound {
            play_sound_event(&mut audio, sound, audio_tick);
        }
        // `boss_death9`'s `TE_LAVASPLASH`, one ring instead of 1024 particles.
        if let Some(origin) = gameplay.boss_death_origin {
            presentation.impact_particles_mut().spawn_ring(
                origin,
                quake_core::effects::ParticleKind::Fire,
                32,
            );
        }
        #[cfg(feature = "start-route-regression")]
        crate::start_route_regression::observe(world.map(), gameplay);
        #[cfg(feature = "e1m1-chain-regression")]
        crate::e1m1_chain_regression::observe(world.map(), gameplay, &entities);
        let pickup = entities.collect_pickups(&world, player_mins, player_maxs, &mut weapon);
        #[cfg(feature = "e1m2-e1m3-route-regression")]
        crate::e1m2_e1m3_route_regression::observe(
            world.map(),
            &entities,
            gameplay,
            pickup,
            &weapon,
        );
        #[cfg(feature = "episode1-route-regression")]
        crate::episode1_regression::observe(
            &world,
            &entities,
            gameplay,
            pickup,
            weapon.inventory().health(),
        );
        #[cfg(feature = "arsenal-regression")]
        crate::arsenal_regression::observe_pickup(pickup, &mut player, &mut weapon);
        if pickup.consumed != 0 {
            presentation.screen_blend_mut().pick_up();
        }
        if let Some(sound) = pickup.sound_id.filter(|sound| audio.contains(*sound)) {
            let _ = audio.play_one_shot_on(sound, PLAYER_ITEM, audio_tick);
        }
        if let Some(text) = pickup.message {
            presentation.set_centerprint(CenterprintText::Fixed(text), CENTERPRINT_TICKS);
        }
        if let Some((destination, no_intermission, _)) = pending_change_level {
            #[cfg(feature = "start-route-regression")]
            crate::start_route_regression::transition_requested(world.map(), destination);
            #[cfg(feature = "e1m2-e1m3-route-regression")]
            crate::e1m2_e1m3_route_regression::transition_requested(world.map(), destination);
            // The arsenal probe drives the player through five maps by hand
            // and re-arms itself on each load; it never wants a panel between
            // them.
            // `changelevel_touch`: spawnflag 1 skips the intermission and
            // goes straight to the next map. Start's own door into E1M1
            // carries it, so entering the episode raised a panel it never
            // should have. The arsenal probe drives five maps by hand and
            // never wants a panel either.
            #[cfg(not(feature = "arsenal-regression"))]
            let skip_panel = no_intermission;
            #[cfg(feature = "arsenal-regression")]
            let skip_panel = {
                let _ = no_intermission;
                true
            };
            #[cfg(not(feature = "arsenal-regression"))]
            if !skip_panel {
                let panel = begin_intermission(
                    &world,
                    &entities,
                    &player,
                    destination,
                    presentation.elapsed_seconds(),
                );
                #[cfg(feature = "episode1-route-regression")]
                crate::episode1_regression::observe_intermission(&panel.view);
                // PORT ADDITION, not id1: `trigger_changelevel` cuts straight
                // to the panel in the original. Bringing it up out of black
                // costs the one full-screen quad that is already there.
                presentation.screen_blend_mut().fade_in_from_black();
                intermission = Some(panel);
                continue;
            }
            let Some(next_player) = load_level(
                &mut world,
                destination,
                &mut entities,
                &mut audio,
                &mut music,
                &mut renderer,
                &mut presentation,
            ) else {
                psx_rt::tty::println("quake-psx: Rust level transition failed");
                psx_rt::halt();
            };
            player = next_player;
            weapon.map_loaded();
            #[cfg(feature = "arsenal-regression")]
            if !crate::arsenal_regression::map_loaded(&world, &entities, &mut player) {
                psx_rt::tty::println("quake-psx: Rust arsenal regression map setup failed");
                psx_rt::halt();
            }
            movement_tick = psx_rt::interrupts::vblank_count();
            continue;
        }

        weapon.tick(elapsed_ticks);
        // `PlayerPreThink` returns above `W_WeaponFrame` once the player is
        // dead, so a corpse neither switches nor fires.
        let alive = weapon.inventory().health() > 0;
        if alive {
            if let Some(direct) = controls
                .direct_weapon_impulse()
                .and_then(quake_core::combat::Weapon::from_impulse)
            {
                // `select` refuses an unowned or empty weapon without
                // disturbing the current one, matching Quake's impulses.
                weapon.select(direct);
            } else if controls.next_weapon_pressed() {
                weapon.cycle(true);
            } else if controls.previous_weapon_pressed() {
                weapon.cycle(false);
            }
        }
        let fire_held = controls.fire_held() && alive;
        #[cfg(feature = "combat-regression")]
        let fire_held = fire_held || crate::combat_regression::fire_held();
        #[cfg(feature = "arsenal-regression")]
        let fire_held = fire_held || crate::arsenal_regression::fire_held(&weapon);
        // Advance only projectiles which existed at the start of this frame.
        // A newly fired rocket is submitted once at its authored spawn before
        // its first physics segment, matching Quake's entity-frame ordering.
        entities.begin_weapon_frame();
        let player_damage_origin = player.damage_origin();
        let rocket_result =
            entities.update_rockets(&world, player_damage_origin, &mut weapon, elapsed_ticks);
        let nail_result = entities.update_nails(&world, player_mins, player_maxs, elapsed_ticks);
        let fireball_result =
            entities.update_fireballs(&world, player_mins, player_maxs, elapsed_ticks);
        let grenade_result =
            entities.update_grenades(&world, player_damage_origin, &mut weapon, elapsed_ticks);
        let attack_weapon = weapon.attack_weapon(fire_held, player_frame.water_level);
        let admission = entities.attack_admission();
        let mut view_model_muzzle_flash = false;
        // `aim()` costs world traces, so only ask when this frame's shot
        // will actually leave along it.
        let aim_forward =
            (fire_held && weapon.ready_to_fire() && attack_weapon.auto_aims(camera.angles[0]))
                .then(|| {
                    entities.auto_aim(
                        &world,
                        player.origin(),
                        quake_core::combat::view_forward(camera.angles),
                    )
                });
        if let Some(attack) = weapon.try_attack_aimed(
            fire_held,
            camera.origin,
            camera.angles,
            weapon_model_frames(&world, attack_weapon.model_id()),
            player_frame.water_level,
            admission,
            aim_forward,
        ) {
            let sound = attack.sound_id();
            let recoil = attack.recoil_pitch();
            if recoil != 0 {
                player.punch(recoil);
            }
            if attack.muzzle_flashes() {
                // A real `MUZZLEFLASH` dlight on the player, not the
                // full-screen tint this used to raise: the original has no
                // screen flash for a shot, and a light at the eye lifts the
                // wall and the monster in front of you without touching the
                // far end of the corridor.
                dynamic_lights.spawn_muzzle_flash(camera.origin);
                view_model_muzzle_flash = true;
            }
            match attack {
                quake_core::combat::WeaponAttack::Axe(attack) => {
                    if let Some(result) = entities.fire_hitscan(&world, attack) {
                        play_response_sound(&mut audio, result.response_sound, audio_tick);
                        if let Some(impact) = result.last_impact {
                            presentation.impact_particles_mut().spawn_blood(impact);
                        }
                    }
                }
                quake_core::combat::WeaponAttack::Shotgun(attack) => {
                    if let Some(result) = entities.fire_shotgun(&world, &attack) {
                        play_response_sound(&mut audio, result.response_sound, audio_tick);
                        if let Some(impact) = result.last_impact {
                            presentation.impact_particles_mut().spawn_blood(impact);
                        }
                    }
                }
                quake_core::combat::WeaponAttack::Nail(spawn) => {
                    let _ = entities.spawn_nail(&world, spawn);
                }
                quake_core::combat::WeaponAttack::Grenade(spawn) => {
                    let _ = entities.spawn_grenade(&world, spawn);
                }
                quake_core::combat::WeaponAttack::Rocket(spawn) => {
                    let _ = entities.spawn_rocket(&world, spawn);
                }
                quake_core::combat::WeaponAttack::Lightning(attack) => {
                    if let Some(result) = entities.fire_lightning(&world, attack) {
                        play_response_sound(&mut audio, result.damage.response_sound, audio_tick);
                        if let Some(impact) = result.damage.last_impact {
                            presentation.impact_particles_mut().spawn_blood(impact);
                        }
                    }
                }
                quake_core::combat::WeaponAttack::LightningDischarge(discharge) => {
                    if let Some(result) = entities.fire_lightning_discharge(
                        &world,
                        discharge,
                        player_damage_origin,
                        &mut weapon,
                    ) {
                        play_response_sound(&mut audio, result.damage.response_sound, audio_tick);
                    }
                }
            }
            if let Some(sound) = sound.filter(|sound| audio.contains(*sound)) {
                let _ = audio.play_one_shot_on(sound, PLAYER_WEAPON, audio_tick);
            }
        }
        if let Some(result) = rocket_result {
            if result.impacts != 0 {
                play_world_sound(&mut audio, 0xc8, result.last_impact, audio_tick);
                play_response_sound(&mut audio, result.response_sound, audio_tick);
            }
        }
        if let Some(result) = nail_result {
            if result.player_damage != 0 {
                weapon.take_damage(result.player_damage.min(i16::MAX as u16) as i16);
            }
            play_response_sound(&mut audio, result.damage.response_sound, audio_tick);
            if let Some(impact) = result.damage.last_impact {
                presentation.impact_particles_mut().spawn_blood(impact);
            }
            if result.world_impacts != 0 {
                // `TE_SPIKE` in `CL_ParseTEnt`: tink1 four times in five,
                // otherwise one of the three ricochets.
                let sound = if audio_tick % 5 == 0 {
                    crate::entity::SPIKE_RICOCHET_SOUNDS[(audio_tick / 5 % 3) as usize]
                } else {
                    crate::entity::SPIKE_TINK_SOUND
                };
                let sound = if audio.contains(sound) {
                    sound
                } else {
                    crate::entity::SPIKE_TINK_SOUND
                };
                play_world_sound(&mut audio, sound, result.last_world_impact, audio_tick);
            }
        }
        #[cfg(feature = "systems-regression")]
        if let Some(destination) = crate::systems_regression::drive(
            world.map(),
            gameplay,
            fireball_result.unwrap_or_default(),
        ) {
            let Some(next_player) = load_level(
                &mut world,
                destination,
                &mut entities,
                &mut audio,
                &mut music,
                &mut renderer,
                &mut presentation,
            ) else {
                psx_rt::tty::println("quake-psx: Rust systems regression map load failed");
                psx_rt::halt();
            };
            player = next_player;
            weapon.map_loaded();
            movement_tick = psx_rt::interrupts::vblank_count();
            continue;
        }
        if let Some(result) = fireball_result {
            // `fire_fly` and `fire_touch` are both silent in the original: a
            // lava spout has no launch or impact noise.
            if result.player_damage != 0 {
                weapon.take_damage(result.player_damage.min(i16::MAX as u16) as i16);
            }
        }
        if let Some(result) = grenade_result {
            if result.bounces != 0 {
                play_world_sound(&mut audio, 0xc1, result.last_bounce, audio_tick);
            }
            if result.explosions != 0 {
                play_world_sound(&mut audio, 0xc8, result.damage.last_impact, audio_tick);
            }
            play_response_sound(&mut audio, result.damage.response_sound, audio_tick);
        }
        // `T_Damage` knockback from every projectile that hurt the player
        // this frame; the monster pass below adds its own. The summed impulse
        // also stands in for the damage message's `from` when the view kick
        // is worked out below.
        #[allow(unused_mut)]
        let mut damage_impulse = add_vec(
            add_vec(
                rocket_result.unwrap_or_default().player_impulse,
                grenade_result.unwrap_or_default().damage.player_impulse,
            ),
            add_vec(
                nail_result.unwrap_or_default().player_impulse,
                fireball_result.unwrap_or_default().player_impulse,
            ),
        );
        player.add_velocity(damage_impulse);
        // Route regressions prove the authored mechanism chains with
        // deterministic waypoint movement; like the Episode 1 map route they
        // keep the monster think loop out of the probe.
        #[cfg(not(any(
            feature = "episode1-regression",
            feature = "combat-regression",
            feature = "arsenal-regression",
            feature = "start-route-regression",
            feature = "e1m1-chain-regression",
            feature = "e1m2-e1m3-route-regression",
            feature = "survival-regression",
            feature = "systems-regression"
        )))]
        {
            let missiles = entities.update_monster_missiles(
                &world,
                player_mins,
                player_maxs,
                &mut weapon,
                elapsed_ticks,
            );
            let monsters = entities.update_monsters(
                &world,
                player.origin(),
                player.velocity(),
                player_mins,
                player_maxs,
                fire_held,
                &mut weapon,
                elapsed_ticks,
            );
            // `barrel_explode` for any misc_explobox killed this frame, by
            // any weapon or by another explosion.
            let detonations =
                entities.detonate_pending_explosions(&world, player.origin(), &mut weapon);
            let mut knockback = quake_formats::Vec3I32::default();
            for result in [missiles, monsters, detonations].into_iter().flatten() {
                for &sound in result.sound_ids() {
                    play_sound_event(&mut audio, sound, audio_tick);
                }
                // An ogre grenade going off draws the same explosion and
                // dynamic light as the player's own rockets and grenades.
                if let Some(impact) = result.last_explosion {
                    dynamic_lights.spawn_explosion(impact);
                    presentation.explosion_effects_mut().spawn(impact);
                    presentation.impact_particles_mut().spawn_ring(
                        impact,
                        quake_core::effects::ParticleKind::Fire,
                        quake_core::effects::EXPLOSION_RING_UNITS,
                    );
                }
                // A gibbed monster bursts where it stood.
                if let Some(origin) = result.last_gib {
                    presentation.impact_particles_mut().spawn_blood(origin);
                }
                // Authored monster closets use the same destination `spawn_tfog`
                // burst as a player teleport, but without moving the camera.
                for &origin in result.teleport_fogs() {
                    presentation.impact_particles_mut().spawn_ring(
                        origin,
                        quake_core::effects::ParticleKind::Spark,
                        16,
                    );
                }
                // `BackpackTouch`: `stuffcmd(other, "bf\n")` and the "You get "
                // line. `army_die3` and `ogre_die3` are the only droppers and
                // both set their one ammo count by hand, so the two lines the
                // player can ever read are already whole.
                if let Some(ammo) = result.backpack_pickup {
                    presentation.screen_blend_mut().pick_up();
                    presentation.set_centerprint(
                        CenterprintText::Fixed(if ammo.shells != 0 {
                            "You get 5 shells"
                        } else {
                            "You get 2 rockets"
                        }),
                        CENTERPRINT_TICKS,
                    );
                }
                knockback = add_vec(knockback, result.player_impulse);
            }
            player.add_velocity(knockback);
            damage_impulse = add_vec(damage_impulse, knockback);
            #[cfg(feature = "monster-regression")]
            crate::monster_regression::observe(&entities, &weapon, monsters.unwrap_or_default());
            #[cfg(feature = "bestiary-regression")]
            crate::bestiary_regression::observe(&entities, &weapon);
            #[cfg(feature = "monsterjump-regression")]
            crate::monsterjump_regression::observe(&world, &mut entities);
        }
        #[cfg(feature = "combat-regression")]
        crate::combat_regression::observe(&entities, &weapon);
        #[cfg(feature = "arsenal-regression")]
        crate::arsenal_regression::observe_combat(
            &world,
            &mut entities,
            &mut player,
            &mut weapon,
            rocket_result.unwrap_or_default(),
            nail_result.unwrap_or_default(),
            grenade_result.unwrap_or_default(),
        );
        entities.update();
        // `CL_RelinkEntities` leaves the model-flag trails once every
        // projectile has moved, so this reads the settled origins.
        entities.emit_projectile_trails(presentation.impact_particles_mut());
        presentation.screen_blend_mut().tick(elapsed_ticks);
        presentation
            .screen_blend_mut()
            .set_contents(if player_frame.water_level > 0 {
                player_frame.water_type
            } else {
                0
            });
        // `V_CalcPowerupCshift`, which folds into the same sustained quad the
        // contents shift above drives rather than adding one of its own.
        presentation
            .screen_blend_mut()
            .set_powerups(weapon.inventory().powerups());
        let health_after = weapon.inventory().health().max(0) as u16;
        let armor_after = weapon.inventory().armor().max(0) as u16;
        let blood = health_before.saturating_sub(health_after);
        let armor = armor_before.saturating_sub(armor_after);
        presentation.screen_blend_mut().take_damage(blood, armor);
        // `Sbar_DamageTake` runs off the same signal as the screen blend.
        pain_face.tick(elapsed_ticks);
        if blood != 0 || armor != 0 {
            pain_face.take_damage();
            // `V_ParseDamage`: `from` is the inflictor, which sits opposite the
            // knockback it dealt; world damage has no direction and only
            // arms the kick timer.
            player.view_damage(
                (i32::from(blood) + i32::from(armor)) / 2,
                quake_formats::Vec3I32 {
                    x: damage_impulse.x.saturating_neg(),
                    y: damage_impulse.y.saturating_neg(),
                    z: damage_impulse.z.saturating_neg(),
                },
            );
        }
        player.tick_view(elapsed_ticks);
        #[cfg(not(feature = "visual-parity-regression"))]
        let render_camera = player.render_camera();
        #[cfg(feature = "visual-parity-regression")]
        let render_camera = camera;
        for impact in [
            rocket_result.unwrap_or_default().last_impact,
            grenade_result.unwrap_or_default().damage.last_impact,
        ]
        .into_iter()
        .flatten()
        {
            // `TE_EXPLOSION`'s dlight, which the original draws instead of any
            // screen tint: it is attenuated by distance from each lit surface
            // and it stays where the blast was, so a rocket round the corner
            // no longer flashes the whole view.
            dynamic_lights.spawn_explosion(impact);
            presentation.explosion_effects_mut().spawn(impact);
            // `R_ParticleExplosion` around the same star, heavily decimated.
            presentation.impact_particles_mut().spawn_ring(
                impact,
                quake_core::effects::ParticleKind::Fire,
                quake_core::effects::EXPLOSION_RING_UNITS,
            );
        }
        // The owner-camera oracle must compare renderer output, not whichever
        // phase of a flickering light the current performance happens to reach
        // after its fixed controller tape. Shipping still uses the live
        // 60-Hz audio/vblank clock; only this diagnostic feature freezes the
        // original light-style animator at a deterministic phase.
        #[cfg(not(feature = "visual-parity-regression"))]
        let render_light_tick = audio_tick;
        #[cfg(feature = "visual-parity-regression")]
        let render_light_tick = 0;
        entities.animate_lights(&world, render_light_tick);
        renderer.set_light_styles(entities.light_styles());
        renderer.set_dynamic_lights(&dynamic_lights);
        #[cfg(feature = "renderer-owned-sections")]
        if world
            .ensure_render_section_for_point(render_camera.origin)
            .is_err()
        {
            psx_rt::tty::println("quake-psx: render section activation failed");
            psx_rt::halt();
        }
        let centerprint_text = presentation.centerprint().and_then(|active| match active {
            CenterprintText::Cooked(offset) => world
                .string_at(offset)
                .and_then(|bytes| core::str::from_utf8(bytes).ok()),
            CenterprintText::Fixed(text) => Some(text),
        });
        let _render_stats = renderer.draw_frame(
            &world,
            render_camera,
            render_light_tick,
            menu.view().water_warp && player_frame.water_level == 3,
            menu.view().water_alpha,
            entities.entities(),
            entities.lightning_beam(),
            presentation.explosion_effects().active(),
            presentation.impact_particles().active(),
            entities.rotating_yaw(),
            Some(crate::renderer::ViewModelInput {
                weapon: weapon.view(),
                velocity: player.velocity(),
                elapsed_ticks,
                muzzle_flash: view_model_muzzle_flash,
                bob_q12: player.view_bob(),
            }),
            Some(
                quake_core::hud::HudView::from_inventory(weapon.inventory())
                    .with_pain(pain_face.active())
                    .with_runes(entities.runes()),
            ),
            centerprint_text,
            None,
            None,
            music.now_playing(audio_tick),
            presentation.screen_blend(),
        );
        #[cfg(feature = "combat-regression")]
        crate::combat_regression::observe_render(_render_stats, weapon.view());
        #[cfg(feature = "arsenal-regression")]
        crate::arsenal_regression::observe_render(_render_stats, weapon.view());
        #[cfg(feature = "visual-parity-regression")]
        crate::visual_parity_regression::observe_render(_render_stats);
        #[cfg(all(
            feature = "e1m1-chain-regression",
            any(
                feature = "renderer-topology-cache",
                feature = "renderer-indexed-projection",
                feature = "renderer-subdivision-cache"
            )
        ))]
        crate::e1m1_chain_regression::observe_render(_render_stats);
        #[cfg(feature = "episode1-regression")]
        crate::regression::observe_render(_render_stats);
    }
}

/// Build the end-of-level panel for the map that was just finished.
///
/// `SelectIntermissionPoint` picks a random `info_intermission`; this port
/// takes the first authored one so the panel is deterministic, and falls back
/// to the player's own eye when a map authors none.
/// Keep this out of line. Inlining changes MIPS register allocation and frame
/// timing enough to alter the input-driven E1M1 route.
#[optimize(size)]
#[inline(never)]
fn begin_intermission(
    world: &crate::asset::ResidentMap,
    entities: &crate::entity::EntityScene,
    player: &crate::player::Player,
    next: crate::asset::EpisodeMap,
    seconds: u16,
) -> Intermission {
    let (kills, total_kills) = entities.kills();
    let (secrets, total_secrets) = entities.secrets();
    let camera = match entities.intermission_spot() {
        Some(spot) => crate::renderer::Camera {
            origin: spot.origin,
            angles: spot.angles,
        },
        None => player.camera(),
    };
    Intermission {
        view: quake_core::level::IntermissionView {
            title: quake_core::level::level_title(map_index(world.map())),
            kills,
            total_kills,
            secrets,
            total_secrets,
            seconds,
            // Shareware Episode 1 ends exactly once: the Chthon map's own
            // `trigger_changelevel` back to Start, with the sigil in hand.
            episode: if world.map() == crate::asset::EpisodeMap::E1M7
                && next == crate::asset::EpisodeMap::Start
                && entities.runes() != 0
            {
                quake_core::level::IntermissionView::EPISODE_PANEL
            } else {
                quake_core::level::IntermissionView::EPISODE_NONE
            },
        },
        next,
        camera,
        remaining: INTERMISSION_TICKS,
        elapsed: 0,
    }
}

#[optimize(size)]
const fn map_index(map: crate::asset::EpisodeMap) -> u8 {
    match map {
        crate::asset::EpisodeMap::Start => 0,
        crate::asset::EpisodeMap::E1M1 => 1,
        crate::asset::EpisodeMap::E1M2 => 2,
        crate::asset::EpisodeMap::E1M3 => 3,
        crate::asset::EpisodeMap::E1M4 => 4,
        crate::asset::EpisodeMap::E1M5 => 5,
        crate::asset::EpisodeMap::E1M6 => 6,
        crate::asset::EpisodeMap::E1M7 => 7,
        crate::asset::EpisodeMap::E1M8 => 8,
    }
}

#[optimize(size)]
fn initial_map() -> crate::asset::EpisodeMap {
    #[cfg(feature = "arsenal-regression")]
    {
        return crate::asset::EpisodeMap::E1M1;
    }
    #[cfg(feature = "combat-regression")]
    {
        return crate::asset::EpisodeMap::E1M1;
    }
    #[cfg(feature = "monster-regression")]
    {
        return crate::asset::EpisodeMap::E1M1;
    }
    #[cfg(feature = "monsterjump-regression")]
    {
        return crate::monsterjump_regression::initial_map();
    }
    #[cfg(feature = "e1m1-chain-regression")]
    {
        return crate::asset::EpisodeMap::E1M1;
    }
    #[cfg(feature = "bestiary-regression")]
    {
        return crate::bestiary_regression::initial_map();
    }
    #[cfg(feature = "e1m2-e1m3-route-regression")]
    {
        return crate::asset::EpisodeMap::E1M2;
    }
    #[cfg(feature = "survival-regression")]
    {
        return crate::asset::EpisodeMap::E1M1;
    }
    #[cfg(feature = "systems-regression")]
    {
        return crate::systems_regression::initial_map();
    }
    #[cfg(feature = "episode1-route-regression")]
    {
        return crate::episode1_regression::initial_map();
    }
    #[cfg(feature = "visual-parity-regression")]
    {
        return crate::asset::EpisodeMap::E1M1;
    }
    #[cfg(not(any(
        feature = "combat-regression",
        feature = "arsenal-regression",
        feature = "monster-regression",
        feature = "monsterjump-regression",
        feature = "bestiary-regression",
        feature = "e1m1-chain-regression",
        feature = "e1m2-e1m3-route-regression",
        feature = "survival-regression",
        feature = "episode1-route-regression",
        feature = "systems-regression",
        feature = "visual-parity-regression"
    )))]
    {
        crate::asset::EpisodeMap::Start
    }
}

#[optimize(size)]
fn weapon_model_frames(world: &crate::asset::ResidentMap, model_id: i16) -> u16 {
    let models = world.alias_models();
    (0..models.len())
        .find_map(|index| {
            let model = models.model_at(index)?;
            (model.header().id == model_id).then_some(model.header().frame_count)
        })
        .unwrap_or(1)
}

#[optimize(size)]
fn menu_input(input: crate::input::InputFrame) -> quake_core::menu::MenuInput {
    use psx_pad::button;

    quake_core::menu::MenuInput {
        up: input.menu_pressed & button::UP != 0,
        down: input.menu_pressed & button::DOWN != 0,
        left: input.menu_pressed & button::LEFT != 0,
        right: input.menu_pressed & button::RIGHT != 0,
        accept: input.menu_pressed & button::CROSS != 0,
        back: input.menu_pressed & (button::CIRCLE | button::START | button::SELECT) != 0,
    }
}

#[optimize(size)]
fn apply_look_settings(
    mut input: crate::input::InputFrame,
    menu: &quake_core::menu::Menu,
) -> crate::input::InputFrame {
    input.look = menu.apply_look_settings(input.look);
    input
}

#[optimize(size)]
fn survival_input(
    frame: crate::player::PlayerFrame,
    elapsed_ticks: u16,
) -> quake_core::survival::SurvivalInput {
    use quake_core::movement::MovementEvents;

    quake_core::survival::SurvivalInput {
        elapsed_ticks,
        water_level: frame.water_level,
        water_type: frame.water_type,
        hard_land: frame.events.contains(MovementEvents::HARD_LAND),
    }
}

#[optimize(size)]
fn play_movement_audio(
    audio: &mut crate::audio::AudioBank,
    frame: crate::player::PlayerFrame,
    video_tick: u32,
) {
    use quake_core::collision::{CONTENTS_LAVA, CONTENTS_SLIME};
    use quake_core::movement::MovementEvents;

    let events = frame.events;
    if events.contains(MovementEvents::JUMP) {
        let _ = audio.play_one_shot_on(0xA6, PLAYER_BODY, video_tick);
    }
    if events.contains(MovementEvents::SWIM) {
        let id = if video_tick & 1 == 0 { 0x7D } else { 0x7E };
        let _ = audio.play_one_shot_on(id, PLAYER_BODY, video_tick);
    }
    if events.contains(MovementEvents::HARD_LAND) {
        let _ = audio.play_one_shot_on(0x9D, PLAYER_BODY, video_tick);
    } else if events.contains(MovementEvents::LAND) {
        let _ = audio.play_one_shot_on(0x9C, PLAYER_BODY, video_tick);
    }
    if events.contains(MovementEvents::WATER_LAND) {
        play_preferred_sound(audio, 0x99, 0x9C, video_tick);
    }
    if events.contains(MovementEvents::ENTER_LIQUID) {
        match frame.water_type {
            CONTENTS_LAVA => play_preferred_sound(audio, 0x9B, 0x9E, video_tick),
            CONTENTS_SLIME => play_preferred_sound(audio, 0xA7, 0x9A, video_tick),
            _ => {
                let _ = audio.play_one_shot_on(0x9A, PLAYER_BODY, video_tick);
            }
        }
    }
    if events.contains(MovementEvents::LEAVE_LIQUID) {
        let _ = audio.play_one_shot_on(0x72, PLAYER_BODY, video_tick);
    }
}

/// `sound (self, CHAN_BODY, ...)`: every jump, landing and liquid transition
/// in the original is a body noise, so a new one cuts the last instead of
/// taking a second voice from the twelve.
const PLAYER_BODY: u16 =
    crate::audio::sound_key(crate::audio::OWNER_PLAYER, crate::audio::CHAN_BODY);
/// `PainSound`, `DeathSound`, the gasp and `CheckPowerups`' warning.
const PLAYER_VOICE: u16 =
    crate::audio::sound_key(crate::audio::OWNER_PLAYER, crate::audio::CHAN_VOICE);
/// `W_Attack`'s firing noise.
const PLAYER_WEAPON: u16 =
    crate::audio::sound_key(crate::audio::OWNER_PLAYER, crate::audio::CHAN_WEAPON);
/// `item_touch`: `sound (other, CHAN_ITEM, self.noise, 1, ATTN_NORM)`.
const PLAYER_ITEM: u16 =
    crate::audio::sound_key(crate::audio::OWNER_PLAYER, crate::audio::CHAN_ITEM);

#[optimize(size)]
fn play_preferred_sound(
    audio: &mut crate::audio::AudioBank,
    preferred: i16,
    fallback: i16,
    video_tick: u32,
) {
    let id = if audio.contains(preferred) {
        preferred
    } else {
        fallback
    };
    let _ = audio.play_one_shot_on(id, PLAYER_BODY, video_tick);
}

#[optimize(size)]
fn play_response_sound(
    audio: &mut crate::audio::AudioBank,
    sound: Option<crate::entity::SoundEvent>,
    video_tick: u32,
) {
    if let Some(sound) = sound {
        play_sound_event(audio, sound, video_tick);
    }
}

/// A world sound at `origin` when the emitter's position is known this
/// frame, otherwise centred like a listener-owned one.
#[optimize(size)]
fn play_world_sound(
    audio: &mut crate::audio::AudioBank,
    id: i16,
    origin: Option<quake_formats::Vec3I32>,
    video_tick: u32,
) {
    play_sound_event(
        audio,
        crate::entity::SoundEvent::world(id, origin),
        video_tick,
    );
}

#[optimize(size)]
#[cold]
#[inline(never)]
fn play_sound_event(
    audio: &mut crate::audio::AudioBank,
    sound: crate::entity::SoundEvent,
    video_tick: u32,
) {
    if !audio.contains(sound.id()) {
        return;
    }
    let _ = match sound.placement() {
        Some((origin, attenuation)) => {
            audio.play_one_shot_at(sound.id(), origin, attenuation, sound.key(), video_tick)
        }
        None => audio.play_one_shot_on(sound.id(), sound.key(), video_tick),
    };
}

#[optimize(size)]
fn load_level(
    world: &mut crate::asset::ResidentMap,
    map: crate::asset::EpisodeMap,
    entities: &mut crate::entity::EntityScene,
    audio: &mut crate::audio::AudioBank,
    music: &mut crate::music::Music,
    renderer: &mut crate::renderer::Renderer,
    presentation: &mut LevelPresentation,
) -> Option<crate::player::Player> {
    #[cfg(feature = "hardware-performance")]
    crate::platform::hardware_performance_pause();

    // Every disc read in the running game happens below this line, so this is
    // the one place the drive changes hands. Even a residency hit reloads the
    // map's sound bank, so the handoff is unconditional.
    music.suspend_for_load();

    let loaded = if world.is_resident(map) {
        #[cfg(feature = "emulator-telemetry")]
        psx_telemetry::emit::debug_log("quake-psx: map residency hit");
        reset_level_state(world, map, entities, audio, presentation)
    } else {
        #[cfg(feature = "emulator-telemetry")]
        psx_telemetry::emit::debug_log("quake-psx: map residency miss begin");
        let Some(loading_picture) = world.picture(quake_formats::GraphicsPictureId::Disc) else {
            #[cfg(feature = "hardware-performance")]
            crate::platform::hardware_performance_resume();
            return None;
        };
        quake_core::loading::present_before_payload(
            || renderer.draw_loading(loading_picture, map),
            || {
                let residency = match world.ensure_resident(map) {
                    Ok(residency) => residency,
                    Err(error) => {
                        #[cfg(feature = "emulator-telemetry")]
                        emit_map_load_error(error);
                        return None;
                    }
                };
                debug_assert!(!residency.is_hit());
                debug_assert_eq!(residency.generation(), world.generation());
                #[cfg(feature = "emulator-telemetry")]
                psx_telemetry::emit::debug_log("quake-psx: map residency miss loaded");
                reset_level_state(world, map, entities, audio, presentation)
            },
        )
    };

    music.resume_after_load(psx_rt::interrupts::vblank_count());

    #[cfg(feature = "hardware-performance")]
    crate::platform::hardware_performance_resume();
    loaded
}

#[cfg(feature = "emulator-telemetry")]
#[optimize(size)]
fn emit_map_load_error(error: crate::asset::MapLoadError) {
    use crate::asset::MapLoadError;

    let message = match error {
        MapLoadError::Storage(_) => "quake-psx: map residency miss error storage",
        MapLoadError::Format => "quake-psx: map residency miss error format",
        MapLoadError::TooLarge => "quake-psx: map residency miss error too-large",
        MapLoadError::BadTextureData => "quake-psx: map residency miss error texture",
        MapLoadError::BadVertexData => "quake-psx: map residency miss error vertex",
        MapLoadError::BadAliasModels => "quake-psx: map residency miss error alias",
        MapLoadError::VramUpload => "quake-psx: map residency miss error vram",
        MapLoadError::BadFace(_) => "quake-psx: map residency miss error face",
        MapLoadError::BadMarkSurface(_) => "quake-psx: map residency miss error marksurface",
        MapLoadError::BadLeaf(_) => "quake-psx: map residency miss error leaf",
        MapLoadError::BadNode(_) => "quake-psx: map residency miss error node",
        MapLoadError::BadClipNode(_) => "quake-psx: map residency miss error clipnode",
        MapLoadError::BadBrushModel(_) => "quake-psx: map residency miss error brushmodel",
        MapLoadError::BadEntity(_) => "quake-psx: map residency miss error entity",
        MapLoadError::MissingEntities => "quake-psx: map residency miss error entities",
    };
    psx_telemetry::emit::debug_log(message);
}

/// Rebuild mutable level state over immutable resident assets.
///
/// This must run for same-map death and New Game requests too: the cache hit
/// skips CD/VRAM payload work, never the Quake server-state reset.
#[optimize(size)]
fn reset_level_state(
    world: &mut crate::asset::ResidentMap,
    _map: crate::asset::EpisodeMap,
    entities: &mut crate::entity::EntityScene,
    audio: &mut crate::audio::AudioBank,
    presentation: &mut LevelPresentation,
) -> Option<crate::player::Player> {
    let player = crate::player::Player::from_start(world, entities.runes())?;
    entities.load(world).ok()?;
    let camera = player.camera();
    let mut stream_scratch = world.take_stream_scratch();
    let audio_result = audio.load_map(
        world,
        camera.origin,
        camera.angles[1],
        entities,
        &mut stream_scratch,
    );
    world.restore_stream_scratch(stream_scratch);
    audio_result.ok()?;
    let player = presentation.commit_ready(Some(player))?;
    #[cfg(feature = "episode1-regression")]
    crate::regression::map_loaded(_map);
    #[cfg(feature = "start-route-regression")]
    crate::start_route_regression::map_loaded(_map);
    #[cfg(feature = "e1m1-chain-regression")]
    crate::e1m1_chain_regression::map_loaded(_map);
    #[cfg(feature = "e1m2-e1m3-route-regression")]
    crate::e1m2_e1m3_route_regression::map_loaded(_map);
    #[cfg(feature = "survival-regression")]
    crate::survival_regression::map_loaded(_map);
    #[cfg(feature = "systems-regression")]
    crate::systems_regression::map_loaded(_map, entities.secrets(), entities.skill());
    #[cfg(feature = "episode1-route-regression")]
    crate::episode1_regression::map_loaded(_map, entities, world, &player);
    Some(player)
}

fn add_vec(left: quake_formats::Vec3I32, right: quake_formats::Vec3I32) -> quake_formats::Vec3I32 {
    quake_formats::Vec3I32 {
        x: left.x.saturating_add(right.x),
        y: left.y.saturating_add(right.y),
        z: left.z.saturating_add(right.z),
    }
}
