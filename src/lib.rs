mod arena;
mod arena_barriers;
mod arena_defs;
mod arena_prop_colliders;
mod audio_settings;
mod bee_skills;
mod body_collision;
mod bot;
mod camera;
mod characters;
mod chick_skills;
mod combat;
mod combat_sfx;
mod components;
mod constants;
mod control_settings;
mod controller_haptics;
mod effects;
mod equipment;
mod feel;
mod fighter;
mod game_state;
mod game_transition;
mod hud;
mod items;
#[cfg(all(feature = "native", target_os = "macos", not(target_arch = "wasm32")))]
mod macos_gamepad;
mod map_editor;
mod penguin_skills;
#[cfg(feature = "perf")]
pub mod performance;
mod reactions;
mod specials;
mod styles;
mod techniques;
mod tutorial;
mod user_mode;

#[cfg(target_arch = "wasm32")]
use bevy::asset::{AssetMetaCheck, AssetPlugin};
use bevy::prelude::*;
use bevy::window::{PresentMode, WindowResolution};

use crate::constants::{WINDOW_HEIGHT, WINDOW_WIDTH};

#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone)]
enum GameSet {
    Global,
    Input,
    Action,
    Movement,
    Combat,
    Items,
    Respawn,
    Presentation,
}

fn primary_present_mode() -> PresentMode {
    #[cfg(feature = "perf")]
    {
        PresentMode::AutoNoVsync
    }

    #[cfg(not(feature = "perf"))]
    {
        PresentMode::AutoVsync
    }
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
}

/// Builds the complete game application without starting its runner.
///
/// Keeping construction separate from execution lets benchmarks and future
/// integration tests configure the app before driving frames.
pub fn build_app() -> App {
    let default_plugins = DefaultPlugins.set(WindowPlugin {
        primary_window: Some(primary_window_config()),
        ..default()
    });

    #[cfg(all(feature = "native", target_os = "macos", not(target_arch = "wasm32")))]
    let default_plugins = default_plugins.disable::<bevy::gilrs::GilrsPlugin>();

    #[cfg(target_arch = "wasm32")]
    let default_plugins = default_plugins.set(AssetPlugin {
        meta_check: AssetMetaCheck::Never,
        ..default()
    });

    let mut app = App::new();

    app.add_plugins(default_plugins);
    app.add_plugins(controller_haptics::ControllerHapticsPlugin);
    app.add_message::<combat_sfx::SfxPreviewRequest>();

    #[cfg(all(feature = "native", target_os = "macos", not(target_arch = "wasm32")))]
    app.add_plugins(macos_gamepad::MacOsGamepadPlugin);

    #[cfg(feature = "perf")]
    app.add_plugins(performance::PerformancePlugin::default());

    app.insert_resource(ClearColor(Color::srgb(0.006, 0.006, 0.012)))
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
        .init_resource::<game_state::GameplayPauseOwners>()
        .init_resource::<combat::HitEffects>()
        .init_resource::<camera::CameraActionEffects>()
        .init_resource::<components::PlayerKeyBindings>()
        .init_resource::<control_settings::ControlPreferences>()
        .init_resource::<tutorial::TutorialProgress>()
        .init_resource::<tutorial::TutorialSession>()
        .init_resource::<game_transition::GameTransition>()
        .init_resource::<user_mode::UserModeState>()
        .init_resource::<user_mode::UserModeGameplayScene>()
        .init_resource::<user_mode::LocalControllerReconnect>()
        .configure_sets(
            Update,
            (
                GameSet::Global,
                GameSet::Input,
                GameSet::Action,
                GameSet::Movement,
                GameSet::Combat,
                GameSet::Items,
                GameSet::Respawn,
                GameSet::Presentation,
            )
                .chain(),
        )
        .add_systems(
            Startup,
            (
                control_settings::load_control_preferences,
                tutorial::load_tutorial_progress,
                effects::setup_effect_assets,
                combat::setup_combat_visual_assets,
                bee_skills::setup_bee_skill_assets,
                chick_skills::setup_chick_skill_assets,
                penguin_skills::setup_penguin_skill_assets,
                combat_sfx::setup_combat_sfx_assets,
                characters::setup_character_move_catalog,
                feel::setup_combat_feel_tuning,
                #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
                bot::setup_bot_action_control,
                #[cfg(target_arch = "wasm32")]
                map_editor::setup_map_overlay,
                #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
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
                #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
                map_editor::setup_map_editor_ui,
                user_mode::setup_user_mode_ui,
            )
                .chain(),
        )
        .add_systems(
            Startup,
            tutorial::setup_tutorial_ui.after(user_mode::setup_user_mode_ui),
        )
        .add_systems(
            Startup,
            game_transition::setup_game_transition_overlay.after(tutorial::setup_tutorial_ui),
        )
        .add_systems(
            Update,
            (
                control_settings::sync_controller_device_info,
                #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
                map_editor::toggle_map_editor,
                #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
                map_editor::map_editor_input,
                #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
                characters::reload_character_move_catalog,
                #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
                feel::reload_combat_feel_tuning,
                (
                    user_mode::sync_user_mode_pointer_hover,
                    user_mode::handle_local_controller_reconnect,
                    tutorial::handle_tutorial_input,
                    user_mode::handle_user_mode_input,
                    user_mode::sync_user_mode_controllers,
                )
                    .chain(),
                combat_sfx::handle_sfx_preview_requests,
                audio_settings::sync_audio_playback_gains,
                (
                    game_state::handle_global_input,
                    tutorial::observe_tutorial_objective,
                    tutorial::advance_tutorial_success,
                    game_transition::advance_game_transition
                        .run_if(game_transition::game_transition_active),
                    user_mode::commit_pending_user_mode_transition,
                    tutorial::commit_pending_tutorial_transition,
                    tutorial::reset_tutorial_step,
                    tutorial::cleanup_tutorial_session,
                    game_state::sync_virtual_time_pause,
                )
                    .chain(),
                user_mode::sync_user_mode_battle_bot,
                user_mode::sync_user_mode_battle_result,
                user_mode::sync_user_mode_menu_music,
                user_mode::sync_user_mode_battle_music,
                user_mode::sync_dev_mode_music,
                game_state::sync_setup_character_scene_models,
                game_state::tick_hitstop,
                game_state::tick_match_timer,
                game_state::tick_announcements,
            )
                .chain()
                .in_set(GameSet::Global),
        )
        .add_systems(
            Update,
            fighter::update_drunk_status
                .in_set(GameSet::Global)
                .after(game_state::tick_hitstop)
                .after(game_state::handle_global_input),
        )
        .add_systems(
            Update,
            (
                arena::setup_arena,
                items::setup_items,
                fighter::spawn_fighters,
                hud::setup_hud,
                #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
                map_editor::setup_map_editor_ui,
                user_mode::mark_web_gameplay_scene_loaded,
            )
                .chain()
                .run_if(user_mode::should_spawn_web_gameplay_scene)
                .in_set(GameSet::Global),
        )
        .add_systems(
            Update,
            (
                fighter::collect_player_input,
                #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
                bot::bot_action_control_input,
                bot::bot_input,
                tutorial::script_tutorial_dummy,
                fighter::apply_drunk_input_modifier,
            )
                .chain()
                .in_set(GameSet::Input)
                .run_if(game_state::match_accepts_gameplay),
        )
        .add_systems(
            Update,
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
                combat::resolve_hitboxes,
                controller_haptics::queue_fighter_action_haptics,
            )
                .chain()
                .in_set(GameSet::Action)
                .run_if(game_state::match_accepts_gameplay),
        )
        .add_systems(
            Update,
            (
                fighter::apply_fighter_movement,
                arena::update_arena_pipe_transits,
                fighter::separate_fighters,
            )
                .chain()
                .in_set(GameSet::Movement)
                .run_if(game_state::match_accepts_gameplay),
        )
        .add_systems(
            Update,
            (
                combat::update_hitboxes,
                specials::update_specials,
                bee_skills::update_bee_skills,
                chick_skills::update_chick_skills,
                penguin_skills::update_penguin_skills,
                penguin_skills::update_penguin_surfaces,
                arena::update_arena_hazards,
                arena::update_arena_hazard_visuals,
                arena::update_arena_pipe_visuals,
                arena::update_crank_yard_machinery,
                arena::update_powder_keg_cannons,
                arena::update_vent_spiral_machinery,
            )
                .chain()
                .in_set(GameSet::Combat)
                .run_if(game_state::match_accepts_gameplay),
        )
        .add_systems(
            Update,
            (
                items::drop_items_from_disabled_fighters,
                items::update_items,
                items::update_moving_items,
            )
                .chain()
                .in_set(GameSet::Items)
                .run_if(game_state::match_accepts_gameplay),
        )
        .add_systems(
            Update,
            (
                fighter::ringout_and_respawn,
                #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
                fighter::refill_depleted_practice_health,
            )
                .in_set(GameSet::Respawn)
                .run_if(game_state::match_accepts_gameplay),
        )
        .add_systems(
            Update,
            (
                fighter::sync_fighter_visuals.run_if(user_mode::gameplay_scene_loaded),
                fighter::sync_light_punch_corner_cues.run_if(user_mode::gameplay_scene_loaded),
                fighter::sync_fighter_tint_visuals.run_if(user_mode::gameplay_scene_loaded),
                fighter::sync_guard_shield_visuals.run_if(user_mode::gameplay_scene_loaded),
                fighter::sync_loadout_visuals.run_if(user_mode::gameplay_scene_loaded),
                items::sync_item_visuals.run_if(user_mode::gameplay_scene_loaded),
                effects::update_effects,
                (
                    arena::sync_arena_visuals.run_if(user_mode::gameplay_scene_loaded),
                    arena::sync_arena_preview_render_layers
                        .run_if(user_mode::gameplay_scene_loaded),
                )
                    .chain(),
                map_editor::sync_map_overlay_visuals.run_if(user_mode::gameplay_scene_loaded),
                #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
                map_editor::sync_map_editor_preview.run_if(user_mode::gameplay_scene_loaded),
                #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
                map_editor::draw_map_editor_gizmos.run_if(user_mode::gameplay_scene_loaded),
                #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
                combat::draw_debug_gizmos.run_if(user_mode::gameplay_scene_loaded),
                combat::tick_feedback_cues.run_if(user_mode::gameplay_scene_loaded),
                #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
                (
                    camera::update_gameplay_camera_controls,
                    camera::toggle_single_player_camera_follow_hotkey,
                    camera::load_single_player_camera_preset_hotkey,
                    camera::save_single_player_camera_preset_hotkey,
                    camera::toggle_camera_action_effects,
                )
                    .chain(),
                camera::follow_camera.run_if(user_mode::gameplay_scene_loaded),
                #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
                map_editor::update_map_editor_camera,
                hud::update_hud.run_if(user_mode::gameplay_scene_loaded),
                hud::update_hud_status_indicators.run_if(user_mode::gameplay_scene_loaded),
                #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
                map_editor::update_map_editor_ui.run_if(user_mode::gameplay_scene_loaded),
                (
                    fighter::update_aim_markers,
                    user_mode::update_user_mode_selection_previews,
                    user_mode::update_user_mode_character_select_background,
                    user_mode::update_user_mode_main_menu_backgrounds,
                    user_mode::update_user_mode_ui,
                    user_mode::update_user_mode_character_select_cards,
                    user_mode::update_user_mode_character_profile,
                    user_mode::update_user_mode_button_styles,
                    user_mode::update_controller_reconnect_overlay,
                    tutorial::update_tutorial_ui,
                    tutorial::update_tutorial_button_styles,
                    game_transition::update_game_transition_overlay,
                )
                    .chain(),
            )
                .chain()
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
            hud::update_dev_arena_label
                .in_set(GameSet::Presentation)
                .run_if(user_mode::gameplay_scene_loaded),
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
            (
                user_mode::update_user_mode_controls_ui,
                user_mode::update_key_settings_ui,
                user_mode::update_sound_settings_ui,
            )
                .in_set(GameSet::Presentation),
        )
        .add_systems(
            Update,
            (
                user_mode::sync_user_mode_ui_camera,
                tutorial::sync_tutorial_ui_camera,
            )
                .in_set(GameSet::Presentation),
        )
        .add_systems(
            Update,
            combat_sfx::play_combat_sfx.in_set(GameSet::Presentation),
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
