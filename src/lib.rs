mod arena;
mod arena_barriers;
mod arena_defs;
mod arena_prop_colliders;
pub mod authority;
pub mod authority_input;
pub mod authority_peer_hub;
pub mod authority_thread;
mod bee_skills;
mod body_collision;
mod bot;
mod camera;
mod canonical_math;
mod canonical_state;
mod characters;
mod chick_skills;
pub mod client_protocol;
mod combat;
mod combat_sfx;
mod components;
pub mod confirmed_progression;
mod constants;
pub mod contact_arbitration;
pub mod dedicated_server;
pub mod determinism;
pub mod ecs_identity;
mod effects;
mod equipment;
mod feel;
mod fighter;
mod game_state;
pub mod headless;
mod hud;
pub mod interpolation;
mod items;
#[cfg(all(feature = "native-net", not(target_arch = "wasm32")))]
pub mod lightyear_adapter;
pub mod listen_authority;
pub mod live_authority;
pub mod live_character_skill_snapshot;
pub mod live_dynamic_snapshot;
pub mod live_hitbox_snapshot;
pub mod live_input;
pub mod live_match_snapshot;
pub mod live_penguin_snapshot;
pub mod live_snapshot;
pub mod live_special_snapshot;
pub mod live_world_snapshot;
pub mod local_loopback;
mod map_editor;
pub mod match_config;
pub mod match_presentation;
pub mod multiplayer_diagnostics;
pub mod multiplayer_observability;
#[cfg(feature = "perf-alloc")]
pub mod multiplayer_performance;
pub mod multiplayer_security;
pub mod native_online;
pub mod native_online_app;
pub mod network_codec;
pub mod network_io;
pub mod network_lab;
#[cfg(test)]
mod network_lab_tests;
pub mod network_protocol;
pub mod network_quality;
pub mod network_runtime;
pub mod online_client;
pub mod online_failure;
pub mod online_lobby;
pub mod online_roster;
mod penguin_skills;
#[cfg(feature = "perf")]
pub mod performance;
pub mod predicted_client;
pub mod presentation_projection;
mod reactions;
pub mod reconnect;
pub mod release_identity;
pub mod remote_online_client;
pub mod replay;
pub mod replay_archive;
pub mod resync_transfer;
pub mod rollback;
pub mod session;
pub mod session_clock;
pub mod sim_event;
pub mod simulation;
#[cfg(test)]
mod simulation_harness;
pub mod snapshot;
pub mod snapshot_ecs;
mod specials;
pub mod state_delta;
pub mod state_sync;
pub mod steam_platform;
pub mod steam_transport;
mod styles;
mod techniques;
pub mod tick_input;
mod user_mode;

#[cfg(target_arch = "wasm32")]
use bevy::asset::{AssetMetaCheck, AssetPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, PresentMode, WindowResolution};

use crate::constants::{WINDOW_HEIGHT, WINDOW_WIDTH};

#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone)]
enum GameSet {
    Global,
    Interpolation,
    PresentationPolicy,
    Presentation,
}

fn primary_present_mode() -> PresentMode {
    #[cfg(feature = "perf")]
    {
        if performance::uncapped_present_mode_requested_from_environment() {
            return PresentMode::AutoNoVsync;
        }
    }

    PresentMode::AutoVsync
}

fn primary_window_exit_condition() -> ExitCondition {
    #[cfg(all(feature = "perf", target_os = "macos"))]
    if performance::active_scenario_requested_from_environment() {
        return ExitCondition::DontExit;
    }

    ExitCondition::OnAllClosed
}

fn primary_window_config() -> Window {
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    {
        return Window {
            title: "Animal Fighter Club".to_string(),
            resolution: WindowResolution::new(WINDOW_WIDTH, WINDOW_HEIGHT),
            present_mode: primary_present_mode(),
            ..default()
        };
    }

    #[cfg(target_arch = "wasm32")]
    {
        let mut window = Window {
            title: "Animal Fighter Club".to_string(),
            resolution: WindowResolution::new(WINDOW_WIDTH, WINDOW_HEIGHT),
            present_mode: primary_present_mode(),
            ..default()
        };

        window.canvas = Some("#bevy-canvas".to_string());
        window.fit_canvas_to_parent = true;
        window.prevent_default_event_handling = true;

        window
    }

    #[cfg(not(any(feature = "native", target_arch = "wasm32")))]
    {
        Window {
            title: "Animal Fighter Club".to_string(),
            resolution: WindowResolution::new(WINDOW_WIDTH, WINDOW_HEIGHT),
            present_mode: primary_present_mode(),
            ..default()
        }
    }
}

/// Builds the complete game application without starting its runner.
///
/// Keeping construction separate from execution lets benchmarks and future
/// integration tests configure the app before driving frames.
pub fn build_app() -> App {
    let default_plugins = DefaultPlugins.set(WindowPlugin {
        primary_window: Some(primary_window_config()),
        exit_condition: primary_window_exit_condition(),
        // Automated profiling runs must finish their labeled measurement even
        // if an external desktop/session manager sends a close request. The
        // performance runner exits explicitly with AppExit after reporting.
        close_when_requested: !cfg!(feature = "perf"),
        ..default()
    });

    #[cfg(target_arch = "wasm32")]
    let default_plugins = default_plugins.set(AssetPlugin {
        meta_check: AssetMetaCheck::Never,
        ..default()
    });

    let mut app = App::new();

    app.add_plugins(default_plugins);
    app.insert_non_send_resource(online_client::EmbeddedOnlineClientController::default());
    app.insert_non_send_resource(native_online::NativeOnlineRuntime::default());
    app.insert_non_send_resource(native_online_app::NativeOnlineApplication::default());

    #[cfg(feature = "perf")]
    app.add_plugins(performance::PerformancePlugin::default());

    app.insert_resource(ClearColor(Color::srgb(0.006, 0.006, 0.012)))
        .insert_resource(Time::<Fixed>::from_hz(simulation::SIM_HZ))
        .insert_resource(GlobalAmbientLight {
            color: Color::srgb(0.85, 0.78, 0.68),
            brightness: 430.0,
            ..default()
        })
        .init_resource::<game_state::MatchState>()
        .init_resource::<game_state::LocalSetup>()
        .init_resource::<game_state::MatchTelemetry>()
        .init_resource::<game_state::Hitstop>()
        .init_resource::<game_state::MatchAnnouncements>()
        .init_resource::<arena_defs::ActiveArena>()
        .init_resource::<simulation::SimTick>()
        .init_resource::<simulation::SimulationDriveMode>()
        .init_resource::<online_client::EmbeddedOnlineClientStatus>()
        .init_resource::<native_online_app::NativeOnlineUiSnapshot>()
        .init_resource::<match_presentation::MatchPresentationPolicy>()
        .init_resource::<match_presentation::PresentedResultSfxHistory>()
        .init_resource::<confirmed_progression::ConfirmedProgressionLedger>()
        .init_resource::<sim_event::TickEventBuffer>()
        .init_resource::<sim_event::SimEventJournal>()
        .init_resource::<sim_event::PresentationEventCursor>()
        .init_resource::<sim_event::PresentationEventRouter>()
        .init_resource::<ecs_identity::SimulationIdentityAllocator>()
        .init_resource::<contact_arbitration::ContactBuffer>()
        .init_resource::<items::ItemContactFrame>()
        .init_resource::<arena::ArenaOrdnanceContactFrame>()
        .init_resource::<tick_input::LocalTickInputState>()
        .init_resource::<interpolation::SimPoseSnapRequest>()
        .init_resource::<combat::HitEffects>()
        .init_resource::<combat::CombatPresentationIntentJournal>()
        .init_resource::<fighter::FighterPresentationIntentJournal>()
        .init_resource::<items::ItemPresentationIntentJournal>()
        .init_resource::<arena::ArenaPresentationIntentJournal>()
        .init_resource::<specials::SpecialPresentationIntentJournal>()
        .init_resource::<bee_skills::BeePresentationIntentJournal>()
        .init_resource::<chick_skills::ChickPresentationIntentJournal>()
        .init_resource::<penguin_skills::PenguinPresentationIntentJournal>()
        .init_resource::<camera::CameraActionEffects>()
        .init_resource::<components::PlayerKeyBindings>()
        .init_resource::<user_mode::UserModeState>()
        .init_resource::<user_mode::UserModeGameplayScene>()
        .init_resource::<user_mode::PresentationTimeScale>()
        .configure_sets(
            Update,
            (
                GameSet::Global,
                GameSet::Interpolation,
                GameSet::PresentationPolicy,
                GameSet::Presentation,
            )
                .chain(),
        )
        .configure_sets(
            FixedUpdate,
            (
                simulation::SimulationSet::TickStart,
                simulation::SimulationSet::Match,
                simulation::SimulationSet::Input,
                simulation::SimulationSet::Action,
                simulation::SimulationSet::Movement,
                simulation::SimulationSet::Combat,
                simulation::SimulationSet::Items,
                simulation::SimulationSet::Respawn,
                simulation::SimulationSet::TickEnd,
            )
                .chain()
                .run_if(simulation::local_simulation_drive_enabled),
        )
        .add_systems(
            PreUpdate,
            (
                online_client::reconcile_embedded_online_client,
                native_online_app::drive_native_online_application,
                fighter::sample_local_player_input
                    .run_if(native_online_app::offline_local_input_enabled),
                native_online_app::sample_native_online_render_input,
                #[cfg(all(
                    feature = "dev-hot-reload",
                    not(feature = "shipping"),
                    not(target_arch = "wasm32")
                ))]
                bot::bot_action_control_input,
                interpolation::initialize_sim_pose_history,
                interpolation::restore_committed_sim_poses,
            )
                .chain()
                .after(bevy::input::InputSystems),
        )
        .add_systems(
            FixedUpdate,
            (
                native_online_app::submit_native_online_inputs,
                online_client::drive_embedded_online_client,
            )
                .chain()
                .before(simulation::SimulationSet::TickStart),
        )
        .add_systems(Last, native_online_app::teardown_native_online_on_exit)
        .add_systems(
            FixedUpdate,
            (
                simulation::advance_sim_tick,
                sim_event::begin_sim_event_tick,
                arena::sync_active_arena_from_match_state,
                ecs_identity::reclaim_orphaned_sim_entities,
                interpolation::begin_sim_pose_tick,
            )
                .chain()
                .in_set(simulation::SimulationSet::TickStart),
        )
        .add_systems(
            FixedUpdate,
            (
                canonical_state::canonicalize_authoritative_state,
                sim_event::commit_sim_event_tick,
                interpolation::capture_sim_pose_tick,
            )
                .chain()
                .in_set(simulation::SimulationSet::TickEnd),
        )
        .add_systems(
            Startup,
            (
                effects::setup_effect_assets,
                combat::setup_combat_visual_assets,
                bee_skills::setup_bee_skill_assets,
                chick_skills::setup_chick_skill_assets,
                penguin_skills::setup_penguin_skill_assets,
                combat_sfx::setup_combat_sfx_assets,
                characters::setup_character_move_catalog,
                feel::setup_combat_feel_tuning,
                #[cfg(all(
                    feature = "dev-hot-reload",
                    not(feature = "shipping"),
                    not(target_arch = "wasm32")
                ))]
                bot::setup_bot_action_control,
                #[cfg(not(all(
                    feature = "dev-hot-reload",
                    not(feature = "shipping"),
                    not(target_arch = "wasm32")
                )))]
                map_editor::setup_map_overlay,
                #[cfg(all(
                    feature = "dev-hot-reload",
                    not(feature = "shipping"),
                    not(target_arch = "wasm32")
                ))]
                map_editor::setup_map_editor,
                specials::setup_special_assets,
                #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
                arena::setup_arena,
                #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
                items::setup_items,
                camera::setup_camera,
                #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
                fighter::spawn_fighters,
                #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
                hud::setup_hud,
                #[cfg(all(
                    feature = "dev-hot-reload",
                    not(feature = "shipping"),
                    not(target_arch = "wasm32")
                ))]
                map_editor::setup_map_editor_ui,
                user_mode::setup_user_mode_ui,
                native_online_app::setup_native_online_ui,
            )
                .chain(),
        )
        .add_systems(
            Update,
            (
                interpolation::apply_sim_pose_snap_request,
                interpolation::interpolate_sim_poses,
            )
                .chain()
                .in_set(GameSet::Interpolation),
        )
        .add_systems(
            Update,
            (
                native_online_app::derive_match_presentation_policy,
                user_mode::sync_online_match_presentation_audio,
            )
                .chain()
                .in_set(GameSet::PresentationPolicy),
        )
        .add_systems(
            Update,
            (
                #[cfg(all(
                    feature = "dev-hot-reload",
                    not(feature = "shipping"),
                    not(target_arch = "wasm32")
                ))]
                map_editor::toggle_map_editor,
                #[cfg(all(
                    feature = "dev-hot-reload",
                    not(feature = "shipping"),
                    not(target_arch = "wasm32")
                ))]
                map_editor::map_editor_input,
                #[cfg(all(
                    feature = "dev-hot-reload",
                    not(feature = "shipping"),
                    not(target_arch = "wasm32")
                ))]
                characters::reload_character_move_catalog,
                #[cfg(all(
                    feature = "dev-hot-reload",
                    not(feature = "shipping"),
                    not(target_arch = "wasm32")
                ))]
                feel::reload_combat_feel_tuning,
                user_mode::sample_user_mode_steam_input,
                native_online_app::handle_native_online_ui_input,
                native_online_app::handle_overlay_unavailable_notice_dismiss,
                user_mode::handle_user_mode_input
                    .run_if(simulation::local_simulation_drive_enabled),
                user_mode::sync_user_mode_controllers,
                game_state::handle_global_input,
                user_mode::sync_user_mode_battle_bot,
                user_mode::sync_user_mode_battle_result,
                user_mode::sync_user_mode_battle_music,
                user_mode::sync_dev_mode_music,
                user_mode::sync_user_mode_preview_scene,
                game_state::sync_setup_character_scene_models,
                game_state::tick_announcements,
            )
                .chain()
                .in_set(GameSet::Global),
        )
        .add_systems(
            FixedUpdate,
            (
                game_state::tick_hitstop,
                game_state::tick_match_timer,
                fighter::update_drunk_status,
            )
                .chain()
                .in_set(simulation::SimulationSet::Match),
        )
        .add_systems(
            Update,
            (
                arena::setup_arena,
                items::setup_items,
                fighter::spawn_fighters,
                hud::setup_hud,
                #[cfg(all(
                    feature = "dev-hot-reload",
                    not(feature = "shipping"),
                    not(target_arch = "wasm32")
                ))]
                map_editor::setup_map_editor_ui,
                user_mode::mark_web_gameplay_scene_loaded,
            )
                .chain()
                .run_if(user_mode::should_spawn_web_gameplay_scene)
                .in_set(GameSet::Global),
        )
        .add_systems(
            FixedUpdate,
            (
                fighter::consume_local_player_input,
                bot::bot_input,
                fighter::apply_drunk_input_modifier,
            )
                .chain()
                .in_set(simulation::SimulationSet::Input)
                .run_if(game_state::match_accepts_gameplay),
        )
        .add_systems(
            FixedUpdate,
            (
                fighter::apply_aim_assist,
                items::handle_item_inputs,
                specials::handle_special_inputs,
                specials::tick_special_cooldowns,
                equipment::tick_equipment_cooldowns,
                fighter::update_fighter_state,
                fighter::update_grab_holds,
                fighter::update_ultimate_locks,
                combat::spawn_attack_hitboxes,
                items::spawn_item_hitboxes,
            )
                .chain()
                .in_set(simulation::SimulationSet::Action)
                .run_if(game_state::match_accepts_gameplay),
        )
        .add_systems(
            FixedUpdate,
            (
                fighter::apply_fighter_movement,
                arena::update_arena_pipe_transits,
                fighter::separate_fighters,
            )
                .chain()
                .in_set(simulation::SimulationSet::Movement)
                .run_if(game_state::match_accepts_gameplay),
        )
        .add_systems(
            FixedUpdate,
            (
                combat::begin_contact_collection,
                combat::update_hitboxes,
                combat::collect_hitbox_contacts,
                specials::collect_special_contacts,
                bee_skills::collect_bee_skill_contacts,
                chick_skills::collect_chick_skill_contacts,
                penguin_skills::collect_penguin_skill_contacts,
                penguin_skills::update_penguin_surfaces,
            )
                .chain()
                .in_set(simulation::SimulationSet::Combat)
                .run_if(game_state::match_accepts_gameplay),
        )
        .add_systems(
            FixedUpdate,
            (
                items::drop_items_from_disabled_fighters,
                items::update_items,
                items::advance_moving_items_and_collect_contacts,
                arena::advance_arena_hazards_and_collect_contacts,
                arena::update_crank_yard_machinery,
                arena::advance_powder_keg_cannons_and_collect_contacts,
                combat::resolve_contacts,
                combat::apply_hitbox_contact_outcomes,
                items::apply_item_contact_outcomes,
                bee_skills::apply_bee_skill_contact_outcomes,
                chick_skills::apply_chick_skill_contact_outcomes,
                penguin_skills::apply_penguin_skill_contact_outcomes,
                specials::apply_special_contact_outcomes,
                arena::apply_powder_keg_contact_outcomes,
                arena::apply_arena_hazard_contact_outcomes,
            )
                .chain()
                .in_set(simulation::SimulationSet::Items)
                .run_if(game_state::match_accepts_gameplay),
        )
        .add_systems(
            FixedUpdate,
            (
                fighter::ringout_and_respawn,
                #[cfg(all(
                    feature = "dev-hot-reload",
                    not(feature = "shipping"),
                    not(target_arch = "wasm32")
                ))]
                fighter::refill_depleted_practice_health,
            )
                .chain()
                .in_set(simulation::SimulationSet::Respawn)
                .run_if(game_state::match_accepts_gameplay),
        )
        .add_systems(
            Update,
            (
                (
                    (
                        combat::present_committed_combat_events,
                        arena::update_arena_fighter_burns,
                    )
                        .chain(),
                    (
                        fighter::sync_fighter_lifecycle_visibility
                            .run_if(user_mode::gameplay_scene_loaded),
                        fighter::sync_fighter_visuals.run_if(user_mode::gameplay_scene_loaded),
                    )
                        .chain(),
                    fighter::sync_light_punch_corner_cues.run_if(user_mode::gameplay_scene_loaded),
                    fighter::sync_fighter_tint_visuals.run_if(user_mode::gameplay_scene_loaded),
                    fighter::sync_guard_shield_visuals.run_if(user_mode::gameplay_scene_loaded),
                    fighter::sync_loadout_visuals.run_if(user_mode::gameplay_scene_loaded),
                    (items::attach_missing_item_visuals, items::sync_item_visuals)
                        .chain()
                        .run_if(user_mode::gameplay_scene_loaded),
                    (
                        specials::attach_missing_special_visuals,
                        specials::sync_special_visuals,
                    )
                        .chain()
                        .run_if(user_mode::gameplay_scene_loaded),
                    (
                        bee_skills::attach_missing_bee_skill_visuals,
                        bee_skills::sync_bee_skill_visuals,
                    )
                        .chain()
                        .run_if(user_mode::gameplay_scene_loaded),
                    (
                        chick_skills::attach_missing_chick_skill_visuals,
                        chick_skills::sync_chick_skill_visuals,
                    )
                        .chain()
                        .run_if(user_mode::gameplay_scene_loaded),
                    (
                        penguin_skills::attach_missing_penguin_visuals,
                        penguin_skills::sync_penguin_visuals,
                    )
                        .chain()
                        .run_if(user_mode::gameplay_scene_loaded),
                    effects::update_effects,
                    (
                        arena::sync_arena_visuals.run_if(user_mode::gameplay_scene_loaded),
                        arena::sync_arena_preview_render_layers
                            .run_if(user_mode::gameplay_scene_loaded),
                    )
                        .chain(),
                    map_editor::sync_map_overlay_visuals.run_if(user_mode::gameplay_scene_loaded),
                    #[cfg(all(
                        feature = "dev-hot-reload",
                        not(feature = "shipping"),
                        not(target_arch = "wasm32")
                    ))]
                    map_editor::sync_map_editor_preview.run_if(user_mode::gameplay_scene_loaded),
                    #[cfg(all(
                        feature = "dev-hot-reload",
                        not(feature = "shipping"),
                        not(target_arch = "wasm32")
                    ))]
                    map_editor::draw_map_editor_gizmos.run_if(user_mode::gameplay_scene_loaded),
                )
                    .chain(),
                (
                    #[cfg(all(
                        feature = "dev-hot-reload",
                        not(feature = "shipping"),
                        not(target_arch = "wasm32")
                    ))]
                    combat::draw_debug_gizmos.run_if(user_mode::gameplay_scene_loaded),
                    combat::tick_feedback_cues.run_if(user_mode::gameplay_scene_loaded),
                    #[cfg(all(
                        feature = "dev-hot-reload",
                        not(feature = "shipping"),
                        not(target_arch = "wasm32")
                    ))]
                    (
                        camera::update_gameplay_camera_controls,
                        camera::toggle_single_player_camera_follow_hotkey,
                        camera::load_single_player_camera_preset_hotkey,
                        camera::save_single_player_camera_preset_hotkey,
                        camera::toggle_camera_action_effects,
                    )
                        .chain(),
                    camera::follow_camera.run_if(user_mode::gameplay_scene_loaded),
                    #[cfg(all(
                        feature = "dev-hot-reload",
                        not(feature = "shipping"),
                        not(target_arch = "wasm32")
                    ))]
                    map_editor::update_map_editor_camera,
                    hud::update_hud.run_if(user_mode::gameplay_scene_loaded),
                    #[cfg(all(
                        feature = "dev-hot-reload",
                        not(feature = "shipping"),
                        not(target_arch = "wasm32")
                    ))]
                    hud::update_dev_arena_label.run_if(user_mode::gameplay_scene_loaded),
                    #[cfg(all(
                        feature = "dev-hot-reload",
                        not(feature = "shipping"),
                        not(target_arch = "wasm32")
                    ))]
                    map_editor::update_map_editor_ui.run_if(user_mode::gameplay_scene_loaded),
                    (
                        user_mode::rotate_user_mode_preview,
                        user_mode::update_user_mode_selection_previews,
                        user_mode::update_user_mode_ui,
                        user_mode::update_user_mode_button_styles,
                        native_online_app::update_native_online_ui,
                        native_online_app::update_native_online_button_styles,
                    )
                        .chain(),
                )
                    .chain(),
            )
                .chain()
                .in_set(GameSet::Presentation),
        )
        .add_systems(
            Update,
            (
                arena::update_arena_hazard_visuals,
                arena::update_arena_pipe_visuals,
                arena::update_crank_yard_machinery_visuals,
                arena::update_vent_spiral_machinery,
                arena::sync_arena_cannon_bomb_visuals,
            )
                .chain()
                .run_if(game_state::match_accepts_gameplay)
                .in_set(GameSet::Presentation),
        )
        .add_systems(
            Update,
            arena::sync_arena_background_to_camera
                .after(camera::follow_camera)
                .run_if(user_mode::gameplay_scene_loaded)
                .in_set(GameSet::Presentation),
        )
        .add_systems(
            Update,
            hud::sync_hud_fighter_plates
                .before(hud::update_hud)
                .in_set(GameSet::Presentation)
                .run_if(user_mode::gameplay_scene_loaded),
        )
        .add_systems(
            Update,
            user_mode::update_user_mode_controls_ui.in_set(GameSet::Presentation),
        )
        .add_systems(
            Update,
            user_mode::sync_user_mode_ui_camera.in_set(GameSet::Presentation),
        )
        .add_systems(
            Update,
            combat_sfx::play_combat_sfx
                .run_if(user_mode::gameplay_scene_loaded)
                .in_set(GameSet::Presentation),
        )
        .add_systems(
            Update,
            user_mode::sync_web_battle_status.in_set(GameSet::Presentation),
        )
        .add_systems(
            Update,
            camera::update_screen_look_transition.in_set(GameSet::Presentation),
        );

    app
}
