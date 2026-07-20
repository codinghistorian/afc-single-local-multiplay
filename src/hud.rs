use bevy::camera::{RenderTarget, visibility::RenderLayers};
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;

use crate::arena_defs::{active_arena_definition, arena_definitions};
use crate::characters::{CharacterKind, FighterCharacter, character_label};
use crate::combat::{HitEffects, ImpactSource};
use crate::components::{
    AnnouncementText, DebugOverlayPanel, DebugOverlayText, Fighter, FighterAction,
    FighterActionState, FighterSpecialState, FighterStats, HealthBar, Hitbox, ParticipantKind,
    PhaseText, ResultPanel, ResultText, StaminaBar, TeamScoreText, TimerText,
};
use crate::constants::{FIGHTER_COLORS, MAX_HEALTH, MAX_STAMINA};
use crate::equipment::{equipment_effect_label, equipment_label};
use crate::fighter::ringout_danger_level;
use crate::game_state::{
    LocalSetup, MatchAnnouncements, MatchPhase, MatchState, MatchTelemetry, TeamId,
};
use crate::items::{ArenaItem, ItemState};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::map_editor::MapEditorState;
use crate::specials::{ActiveSpecial, SpecialKind};
use crate::styles::{style_identity, style_label};
use crate::user_mode::UserModeState;

struct FighterHudSnapshot {
    id: usize,
    character: CharacterKind,
    name: &'static str,
    color: Color,
    health: f32,
    stamina: f32,
    score: i32,
    stock: Option<i32>,
    flash: f32,
    special_cooldown: f32,
    action: FighterAction,
    technique: &'static str,
    cancel_window_open: bool,
    branch_window_open: bool,
    ringout_danger: f32,
}

#[derive(Component)]
struct HudPortraitModel;

#[derive(Component)]
pub(crate) struct HudPortraitImage {
    fighter_id: usize,
    character: CharacterKind,
}

#[derive(Component)]
pub(crate) struct HudNameText {
    fighter_id: usize,
}

#[derive(Component)]
pub(crate) struct HudLifeText {
    fighter_id: usize,
}

const HUD_PORTRAIT_TEXTURE_SIZE: u32 = 128;
const HUD_PORTRAIT_LAYER_BASE: usize = 12;
const HUD_CAT_PORTRAIT_PATH: &str = "ui/hud/animal-cat.png";
const HUD_PIG_PORTRAIT_PATH: &str = "ui/hud/animal-pig.png";
const HUD_BEE_PORTRAIT_PATH: &str = "ui/hud/animal-bee.png";
const HUD_PENGUIN_PORTRAIT_PATH: &str = "ui/hud/animal-penguin.png";
const HUD_CHICK_PORTRAIT_PATH: &str = "ui/hud/animal-chick.png";

pub fn setup_hud(mut commands: Commands, asset_server: Res<AssetServer>) {
    let portrait_images = [
        asset_server.load(HUD_CAT_PORTRAIT_PATH),
        asset_server.load(HUD_PIG_PORTRAIT_PATH),
        asset_server.load(HUD_BEE_PORTRAIT_PATH),
        asset_server.load(HUD_PENGUIN_PORTRAIT_PATH),
        asset_server.load(HUD_CHICK_PORTRAIT_PATH),
    ];

    commands.spawn((
        Camera2d,
        Camera {
            order: 1,
            clear_color: ClearColorConfig::None,
            ..default()
        },
        IsDefaultUiCamera,
    ));

    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            Pickable::IGNORE,
        ))
        .with_children(|parent| {
            parent.spawn(top_timer());
            parent.spawn(phase_plate());
            parent.spawn(announcement_banner());
            #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
            {
                parent.spawn(controls_overlay());
                parent.spawn(debug_overlay());
            }
            parent.spawn(bottom_plate_row(
                portrait_images[0].clone(),
                portrait_images[1].clone(),
                portrait_images[2].clone(),
                portrait_images[3].clone(),
                portrait_images[4].clone(),
            ));
            parent.spawn(result_overlay());
        });
}

#[allow(dead_code)]
fn spawn_hud_portrait(
    commands: &mut Commands,
    images: &mut Assets<Image>,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    fighter_id: usize,
) -> Handle<Image> {
    #[cfg(target_arch = "wasm32")]
    let portrait_view_format = None;
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    let portrait_view_format = Some(TextureFormat::Rgba8UnormSrgb);

    let image = Image::new_target_texture(
        HUD_PORTRAIT_TEXTURE_SIZE,
        HUD_PORTRAIT_TEXTURE_SIZE,
        TextureFormat::Rgba8Unorm,
        portrait_view_format,
    );
    let image_handle = images.add(image);
    let layer = RenderLayers::layer(hud_portrait_layer(fighter_id));
    let origin = hud_portrait_origin(fighter_id);
    let color = FIGHTER_COLORS[fighter_id % FIGHTER_COLORS.len()];

    let head_mesh = meshes.add(Cuboid::new(0.96, 0.78, 0.72));
    let ear_mesh = meshes.add(Sphere::new(0.18).mesh().uv(12, 6));
    let eye_mesh = meshes.add(Sphere::new(0.045).mesh().uv(8, 4));
    let muzzle_mesh = meshes.add(Cuboid::new(0.36, 0.18, 0.08));
    let nose_mesh = meshes.add(Cuboid::new(0.09, 0.065, 0.045));

    let head_material = materials.add(StandardMaterial {
        base_color: color.lighter(0.08),
        perceptual_roughness: 0.62,
        ..default()
    });
    let ear_material = materials.add(StandardMaterial {
        base_color: color.lighter(0.2),
        perceptual_roughness: 0.66,
        ..default()
    });
    let muzzle_material = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.82, 0.62),
        perceptual_roughness: 0.72,
        ..default()
    });
    let face_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.055, 0.045, 0.035),
        emissive: LinearRgba::from(Color::srgb(0.055, 0.045, 0.035).to_linear()) * 0.03,
        ..default()
    });

    commands.spawn((
        Mesh3d(head_mesh),
        MeshMaterial3d(head_material),
        Transform::from_translation(origin),
        HudPortraitModel,
        layer.clone(),
    ));
    for x in [-0.34, 0.34] {
        commands.spawn((
            Mesh3d(ear_mesh.clone()),
            MeshMaterial3d(ear_material.clone()),
            Transform::from_translation(origin + Vec3::new(x, 0.48, -0.03))
                .with_scale(Vec3::new(1.0, 1.35, 0.75)),
            HudPortraitModel,
            layer.clone(),
        ));
    }
    commands.spawn((
        Mesh3d(muzzle_mesh),
        MeshMaterial3d(muzzle_material),
        Transform::from_translation(origin + Vec3::new(0.0, -0.11, 0.39)),
        HudPortraitModel,
        layer.clone(),
    ));
    for x in [-0.18, 0.18] {
        commands.spawn((
            Mesh3d(eye_mesh.clone()),
            MeshMaterial3d(face_material.clone()),
            Transform::from_translation(origin + Vec3::new(x, 0.16, 0.4)),
            HudPortraitModel,
            layer.clone(),
        ));
    }
    commands.spawn((
        Mesh3d(nose_mesh),
        MeshMaterial3d(face_material),
        Transform::from_translation(origin + Vec3::new(0.0, -0.02, 0.45)),
        HudPortraitModel,
        layer.clone(),
    ));
    commands.spawn((
        PointLight {
            intensity: 2300.0,
            range: 5.0,
            shadows_enabled: false,
            ..default()
        },
        Transform::from_translation(origin + Vec3::new(-0.6, 0.9, 2.1)),
        layer.clone(),
    ));
    commands.spawn((
        Camera3d::default(),
        Camera {
            order: -2,
            clear_color: Color::srgba(0.015, 0.018, 0.028, 1.0).into(),
            ..default()
        },
        RenderTarget::Image(image_handle.clone().into()),
        Transform::from_translation(origin + Vec3::new(0.0, 0.06, 2.85))
            .looking_at(origin + Vec3::new(0.0, 0.05, 0.12), Vec3::Y),
        layer,
    ));

    image_handle
}

fn hud_portrait_layer(fighter_id: usize) -> usize {
    HUD_PORTRAIT_LAYER_BASE + fighter_id
}

fn hud_portrait_origin(fighter_id: usize) -> Vec3 {
    Vec3::new(-90.0 + fighter_id as f32 * 3.0, -90.0, 0.0)
}

fn announcement_banner() -> impl Bundle {
    (
        GameplayHudPanel,
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(66.0),
            left: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Px(34.0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        Pickable::IGNORE,
        children![(
            Text::new(""),
            text_style(21.0, Color::srgb(1.0, 0.9, 0.42)),
            TextShadow::default(),
            AnnouncementText,
        )],
    )
}

fn text_style(size: f32, color: Color) -> (TextFont, TextColor) {
    (
        TextFont {
            font_size: size,
            ..default()
        },
        TextColor(color),
    )
}

fn top_timer() -> impl Bundle {
    (
        GameplayHudPanel,
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(8.0),
            left: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Px(52.0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        children![(
            Text::new("120"),
            text_style(44.0, Color::srgb(0.95, 0.95, 0.88)),
            TextShadow::default(),
            TimerText,
        )],
    )
}

fn phase_plate() -> impl Bundle {
    (
        GameplayHudPanel,
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(58.0),
            left: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Px(24.0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        children![(
            Text::new("Team Score (No Timer)"),
            text_style(16.0, Color::srgb(0.72, 0.92, 1.0)),
            TextShadow::default(),
            PhaseText,
        )],
    )
}

#[derive(Component)]
pub(crate) struct GameplayHudPanel;

#[derive(Component)]
pub(crate) struct ControlsOverlayPanel;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn controls_overlay() -> impl Bundle {
    (
        ControlsOverlayPanel,
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(104.0),
            left: Val::Px(18.0),
            padding: UiRect::all(Val::Px(7.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.02, 0.025, 0.035, 0.62)),
        children![(
            Text::new(
                "Move Arrows | Shift+U user mode | Shift+Arrows camera | Shift+R camera reset | Shift+C filter cycle | Z aim/grab | X strong/throw | C light/pickup | V jump | double-tap dash | X+C guard | H debug | F2 map editor"
            ),
            text_style(14.0, Color::srgb(0.88, 0.88, 0.82)),
        )],
    )
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn debug_overlay() -> impl Bundle {
    (
        Node {
            display: Display::None,
            position_type: PositionType::Absolute,
            top: Val::Px(140.0),
            right: Val::Px(18.0),
            width: Val::Px(390.0),
            padding: UiRect::all(Val::Px(8.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.72)),
        DebugOverlayPanel,
        children![(
            Text::new(""),
            text_style(12.0, Color::srgb(0.78, 0.95, 0.86)),
            DebugOverlayText,
        )],
    )
}

fn result_overlay() -> impl Bundle {
    (
        Node {
            display: Display::None,
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.48)),
        ResultPanel,
        children![(
            Node {
                width: Val::Px(520.0),
                padding: UiRect::all(Val::Px(24.0)),
                border: UiRect::all(Val::Px(2.0)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.055, 0.055, 0.07, 0.96)),
            BorderColor::all(Color::srgb(0.92, 0.82, 0.38)),
            children![(
                Text::new("Results"),
                text_style(24.0, Color::srgb(0.98, 0.92, 0.66)),
                TextLayout::new_with_justify(Justify::Center),
                ResultText,
            )],
        )],
    )
}

fn bottom_plate_row(
    left_portrait: Handle<Image>,
    right_portrait: Handle<Image>,
    bee_portrait: Handle<Image>,
    penguin_portrait: Handle<Image>,
    chick_portrait: Handle<Image>,
) -> impl Bundle {
    (
        GameplayHudPanel,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(18.0),
            right: Val::Px(18.0),
            bottom: Val::Px(18.0),
            height: Val::Px(120.0),
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            ..default()
        },
        children![
            fighter_plate(
                0,
                Color::srgb(0.95, 0.12, 0.11),
                left_portrait.clone(),
                right_portrait.clone(),
                bee_portrait.clone(),
                penguin_portrait.clone(),
                chick_portrait.clone()
            ),
            fighter_plate(
                1,
                Color::srgb(0.12, 0.42, 1.0),
                left_portrait,
                right_portrait,
                bee_portrait,
                penguin_portrait,
                chick_portrait
            ),
        ],
    )
}

fn fighter_plate(
    id: usize,
    color: Color,
    cat_portrait: Handle<Image>,
    pig_portrait: Handle<Image>,
    bee_portrait: Handle<Image>,
    penguin_portrait: Handle<Image>,
    chick_portrait: Handle<Image>,
) -> impl Bundle {
    (
        Node {
            width: Val::Px(286.0),
            height: Val::Px(88.0),
            border: UiRect::all(Val::Px(2.0)),
            padding: UiRect::all(Val::Px(6.0)),
            column_gap: Val::Px(6.0),
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(Color::srgba(0.055, 0.055, 0.065, 0.9)),
        BorderColor::all(Color::srgb(0.63, 0.61, 0.56)),
        children![
            (
                Node {
                    width: Val::Px(54.0),
                    height: Val::Px(54.0),
                    border: UiRect::all(Val::Px(2.0)),
                    overflow: Overflow::clip(),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },
                BackgroundColor(color.darker(0.32)),
                BorderColor::all(color.lighter(0.28)),
                children![
                    hud_portrait_image(id, CharacterKind::Cat, cat_portrait),
                    hud_portrait_image(id, CharacterKind::Pig, pig_portrait),
                    hud_portrait_image(id, CharacterKind::Bee, bee_portrait),
                    hud_portrait_image(id, CharacterKind::Penguin, penguin_portrait),
                    hud_portrait_image(id, CharacterKind::Chick, chick_portrait),
                ]
            ),
            (
                Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(3.0),
                    flex_grow: 1.0,
                    ..default()
                },
                children![
                    (
                        Node {
                            width: Val::Percent(100.0),
                            justify_content: JustifyContent::SpaceBetween,
                            align_items: AlignItems::Center,
                            ..default()
                        },
                        children![
                            (
                                HudNameText { fighter_id: id },
                                Text::new(character_label(default_hud_character_for_fighter(id))),
                                text_style(15.0, Color::srgb(0.96, 0.96, 0.88))
                            ),
                            (
                                HudLifeText { fighter_id: id },
                                Text::new("Life --"),
                                text_style(13.0, Color::srgb(0.94, 0.82, 0.35)),
                            )
                        ]
                    ),
                    meter_row(
                        "HP",
                        Color::srgb(0.9, 0.12, 0.1),
                        HealthBar { fighter_id: id }
                    ),
                    meter_row(
                        "MP",
                        Color::srgb(0.13, 0.72, 1.0),
                        StaminaBar { fighter_id: id }
                    ),
                ]
            )
        ],
    )
}

fn hud_portrait_image(
    fighter_id: usize,
    character: CharacterKind,
    image: Handle<Image>,
) -> impl Bundle {
    (
        HudPortraitImage {
            fighter_id,
            character,
        },
        Node {
            display: if hud_portrait_character(default_hud_character_for_fighter(fighter_id))
                == character
            {
                Display::Flex
            } else {
                Display::None
            },
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        ImageNode::new(image),
    )
}

fn default_hud_character_for_fighter(fighter_id: usize) -> CharacterKind {
    if fighter_id == 1 {
        CharacterKind::Pig
    } else {
        CharacterKind::Cat
    }
}

fn hud_portrait_character(character: CharacterKind) -> CharacterKind {
    match character {
        CharacterKind::Pig => CharacterKind::Pig,
        CharacterKind::Bee => CharacterKind::Bee,
        CharacterKind::Penguin => CharacterKind::Penguin,
        CharacterKind::Chick => CharacterKind::Chick,
        _ => CharacterKind::Cat,
    }
}

fn life_label(stock: Option<i32>) -> String {
    stock.map_or_else(
        || "Life --".to_string(),
        |stock| format!("Life {}", stock.max(0)),
    )
}

fn meter_row(label: &'static str, color: Color, marker: impl Component) -> impl Bundle {
    (
        Node {
            width: Val::Percent(100.0),
            height: Val::Px(11.0),
            column_gap: Val::Px(5.0),
            align_items: AlignItems::Center,
            ..default()
        },
        children![
            (
                Node {
                    width: Val::Px(21.0),
                    ..default()
                },
                Text::new(label),
                text_style(10.0, Color::srgb(0.94, 0.9, 0.78)),
            ),
            (
                Node {
                    flex_grow: 1.0,
                    height: Val::Px(10.0),
                    padding: UiRect::all(Val::Px(2.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.65)),
                children![(
                    Node {
                        width: Val::Percent(100.0),
                        height: Val::Percent(100.0),
                        ..default()
                    },
                    BackgroundColor(color),
                    marker,
                )],
            ),
        ],
    )
}

pub fn update_hud(
    state: Res<MatchState>,
    setup: Res<LocalSetup>,
    user_mode: Res<UserModeState>,
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))] editor: Option<
        Res<MapEditorState>,
    >,
    announcements: Res<MatchAnnouncements>,
    telemetry: Res<MatchTelemetry>,
    fighters: Query<(
        &Fighter,
        &FighterStats,
        &FighterCharacter,
        &FighterSpecialState,
        &FighterActionState,
        &Transform,
    )>,
    items: Query<&ArenaItem>,
    specials: Query<&ActiveSpecial>,
    hitboxes: Query<&Hitbox>,
    feedback: Res<HitEffects>,
    mut bar_queries: ParamSet<(
        Query<(&HealthBar, &mut Node, &mut BackgroundColor)>,
        Query<(&StaminaBar, &mut Node)>,
        Query<&mut Node, With<ResultPanel>>,
        Query<&mut Node, With<DebugOverlayPanel>>,
        Query<&mut Node, With<ControlsOverlayPanel>>,
        Query<&mut Node, With<GameplayHudPanel>>,
        Query<(&HudPortraitImage, &mut Node)>,
    )>,
    mut text_queries: ParamSet<(
        Query<&mut Text, With<TimerText>>,
        Query<(&HudNameText, &mut Text)>,
        Query<(&TeamScoreText, &mut Text)>,
        Query<(&HudLifeText, &mut Text)>,
        Query<&mut Text, With<AnnouncementText>>,
        Query<&mut Text, With<PhaseText>>,
        Query<&mut Text, With<ResultText>>,
        Query<&mut Text, With<DebugOverlayText>>,
    )>,
) {
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    let editor_active = editor.as_ref().is_some_and(|editor| editor.active());
    #[cfg(target_arch = "wasm32")]
    let editor_active = false;

    {
        let mut gameplay_panels = bar_queries.p5();
        for mut node in &mut gameplay_panels {
            node.display = if user_mode.active() {
                Display::None
            } else {
                Display::Flex
            };
        }
    }

    {
        let mut controls_panels = bar_queries.p4();
        for mut node in &mut controls_panels {
            node.display = if user_mode.hides_dev_controls() {
                Display::None
            } else {
                Display::Flex
            };
        }
    }

    {
        let mut timer_text = text_queries.p0();
        for mut text in &mut timer_text {
            **text = timer_label(&state);
        }
    }

    {
        let phase = phase_label(&state, &setup);
        let mut phase_text = text_queries.p5();
        for mut text in &mut phase_text {
            **text = phase.clone();
        }
    }

    let snapshots: Vec<FighterHudSnapshot> = fighters
        .iter()
        .filter(|(fighter, _, _, _, _, _)| state.fighter_active(fighter.id))
        .map(
            |(fighter, stats, character, special_state, action, transform)| FighterHudSnapshot {
                id: fighter.id,
                character: character.kind,
                name: character_label(character.kind),
                color: fighter.color,
                health: stats.health,
                stamina: stats.stamina,
                score: stats.score,
                stock: state.stock_for(fighter.id),
                flash: stats.hud_flash,
                special_cooldown: special_state.cooldown,
                action: action.action,
                technique: action
                    .technique_id
                    .map_or("--", |technique| technique.label()),
                cancel_window_open: action.cancel_window_open,
                branch_window_open: action.branch_window_open,
                ringout_danger: ringout_danger_level(
                    transform.translation,
                    active_arena_definition(),
                ),
            },
        )
        .collect();

    {
        let mut health_bars = bar_queries.p0();
        for (bar, mut node, mut background) in &mut health_bars {
            if let Some(snapshot) = snapshots
                .iter()
                .find(|snapshot| snapshot.id == bar.fighter_id)
            {
                node.width = Val::Percent((snapshot.health / MAX_HEALTH * 100.0).clamp(0.0, 100.0));
                *background = BackgroundColor(if snapshot.flash > 0.0 {
                    Color::srgb(1.0, 0.9, 0.3)
                } else if snapshot.ringout_danger > 0.66 {
                    Color::srgb(1.0, 0.38, 0.08)
                } else if snapshot.ringout_danger > 0.0 {
                    Color::srgb(0.95, 0.66, 0.16)
                } else {
                    snapshot.color
                });
            }
        }
    }

    {
        let mut stamina_bars = bar_queries.p1();
        for (bar, mut node) in &mut stamina_bars {
            if let Some(snapshot) = snapshots
                .iter()
                .find(|snapshot| snapshot.id == bar.fighter_id)
            {
                node.width =
                    Val::Percent((snapshot.stamina / MAX_STAMINA * 100.0).clamp(0.0, 100.0));
            }
        }
    }

    {
        let mut portrait_images = bar_queries.p6();
        for (portrait, mut node) in &mut portrait_images {
            if let Some(snapshot) = snapshots
                .iter()
                .find(|snapshot| snapshot.id == portrait.fighter_id)
            {
                node.display = if portrait.character == hud_portrait_character(snapshot.character) {
                    Display::Flex
                } else {
                    Display::None
                };
            }
        }
    }

    {
        let mut name_texts = text_queries.p1();
        for (name, mut text) in &mut name_texts {
            if let Some(snapshot) = snapshots
                .iter()
                .find(|snapshot| snapshot.id == name.fighter_id)
            {
                **text = snapshot.name.to_string();
            }
        }
    }

    let (red_score, blue_score) = team_scores_for_snapshots(&state, &snapshots);

    {
        let mut team_scores = text_queries.p2();
        for (team, mut text) in &mut team_scores {
            **text = if team.team == 0 {
                red_score.to_string()
            } else {
                blue_score.to_string()
            };
        }
    }

    {
        let mut life_texts = text_queries.p3();
        for (life, mut text) in &mut life_texts {
            if let Some(snapshot) = snapshots
                .iter()
                .find(|snapshot| snapshot.id == life.fighter_id)
            {
                **text = life_label(snapshot.stock);
            }
        }
    }

    {
        let mut announcement_texts = text_queries.p4();
        for mut text in &mut announcement_texts {
            **text = if announcements.timer > 0.0 {
                announcements.message.clone()
            } else {
                phase_message(&state, &snapshots)
            };
        }
    }

    {
        let mut result_panels = bar_queries.p2();
        for mut node in &mut result_panels {
            node.display = if setup_result_panel_visible(&state, editor_active, user_mode.active())
            {
                Display::Flex
            } else {
                Display::None
            };
        }
    }

    {
        let mut result_texts = text_queries.p6();
        let result = result_screen_message(&state, &setup, &snapshots, &telemetry);
        for mut text in &mut result_texts {
            **text = result.clone();
        }
    }

    {
        let mut debug_panels = bar_queries.p3();
        for mut node in &mut debug_panels {
            node.display = if state.debug_hitboxes && !user_mode.active() {
                Display::Flex
            } else {
                Display::None
            };
        }
    }

    {
        let debug = if state.debug_hitboxes {
            debug_overlay_message(
                &state, &setup, &snapshots, &items, &specials, &hitboxes, &telemetry, &feedback,
            )
        } else {
            String::new()
        };
        let mut debug_texts = text_queries.p7();
        for mut text in &mut debug_texts {
            **text = debug.clone();
        }
    }
}

fn timer_label(state: &MatchState) -> String {
    match state.phase {
        MatchPhase::Setup => "SETUP".to_string(),
        MatchPhase::Fighting if state.rules.uses_timer() => {
            format!("{:03}", state.timer.ceil() as i32)
        }
        MatchPhase::Fighting => "--".to_string(),
        MatchPhase::TimeUp => "TIME".to_string(),
        MatchPhase::Results => "RESULTS".to_string(),
        MatchPhase::Resetting => "RESET".to_string(),
    }
}

fn phase_label(state: &MatchState, setup: &LocalSetup) -> String {
    let phase = match state.phase {
        MatchPhase::Setup => "Setup",
        MatchPhase::Fighting => "Fighting",
        MatchPhase::TimeUp => "Time Up",
        MatchPhase::Results => "Results",
        MatchPhase::Resetting => "Resetting",
    };
    let rule = if state.phase == MatchPhase::Setup {
        setup.active_rule_label()
    } else {
        state.active_rule_label()
    };
    format!("{phase} | {rule}")
}

fn phase_message(state: &MatchState, snapshots: &[FighterHudSnapshot]) -> String {
    match state.phase {
        MatchPhase::Setup => "Press Enter to start".to_string(),
        MatchPhase::TimeUp => "Time up".to_string(),
        MatchPhase::Results => result_message(state, snapshots),
        MatchPhase::Resetting => "Resetting match".to_string(),
        MatchPhase::Fighting => String::new(),
    }
}

fn result_screen_message(
    state: &MatchState,
    setup: &LocalSetup,
    snapshots: &[FighterHudSnapshot],
    telemetry: &MatchTelemetry,
) -> String {
    if state.phase == MatchPhase::Setup {
        return setup_screen_message(setup);
    }

    let mut lines = vec![
        result_message(state, snapshots),
        format!(
            "{} | Arena {} | Bots {} active",
            state.active_rule_label(),
            arena_definitions()[state.arena_index.min(arena_definitions().len() - 1)].name,
            setup.active_bot_count()
        ),
        format!(
            "Seed {:08X} | ring-outs {} | falls {} | item hits {} | throws {} | guard breaks {} | damage {:.0}",
            telemetry.replay_seed,
            telemetry.ring_outs,
            telemetry.falls,
            telemetry.item_hits,
            telemetry.throws,
            telemetry.guard_breaks,
            telemetry.total_damage()
        ),
        String::new(),
    ];
    for snapshot in snapshots {
        let stock = snapshot
            .stock
            .map(|stock| format!(" | life {stock}"))
            .unwrap_or_default();
        lines.push(format!(
            "{}: score {}{} | {:?}",
            snapshot.name, snapshot.score, stock, snapshot.action
        ));
    }
    lines.push(String::new());
    lines.push(setup_summary_line(setup));
    lines.push("Press R for rematch | Enter for setup".to_string());
    lines.join("\n")
}

fn setup_result_panel_visible(
    state: &MatchState,
    editor_active: bool,
    user_mode_active: bool,
) -> bool {
    if user_mode_active {
        return false;
    }
    matches!(state.phase, MatchPhase::Results)
        || (matches!(state.phase, MatchPhase::Setup) && !editor_active)
}

fn setup_summary_line(setup: &LocalSetup) -> String {
    let characters = setup
        .slots
        .iter()
        .enumerate()
        .map(|(id, slot)| format!("P{} {}", id + 1, character_label(slot.character)))
        .collect::<Vec<_>>()
        .join(" / ");
    let styles = setup
        .slots
        .iter()
        .enumerate()
        .map(|(id, slot)| format!("P{} {}", id + 1, style_label(slot.style)))
        .collect::<Vec<_>>()
        .join(" / ");
    let equipment = setup
        .slots
        .iter()
        .enumerate()
        .map(|(id, slot)| format!("P{} {}", id + 1, equipment_label(slot.equipment)))
        .collect::<Vec<_>>()
        .join(" / ");
    format!("Characters: {characters}\nStyles: {styles}\nEquipment: {equipment}")
}

fn participant_label(participant: ParticipantKind) -> &'static str {
    match participant {
        ParticipantKind::Human => "Player",
        ParticipantKind::Bot => "Bot",
        ParticipantKind::Closed => "Closed",
    }
}

fn setup_screen_message(setup: &LocalSetup) -> String {
    let arena_names = arena_definitions()
        .iter()
        .map(|arena| arena.name)
        .collect::<Vec<_>>()
        .join(" / ");
    let selected_arena = &arena_definitions()[setup.arena_index.min(arena_definitions().len() - 1)];
    let player_character = setup.slots[0].character;
    let bot_character = setup.slots[1].character;
    let characters = setup
        .slots
        .iter()
        .enumerate()
        .map(|(id, slot)| {
            let marker = if id == setup.selected_character_fighter() {
                ">"
            } else {
                " "
            };
            format!(
                "{marker}P{} {} {}",
                id + 1,
                participant_label(slot.participant),
                character_label(slot.character)
            )
        })
        .collect::<Vec<_>>()
        .join("  ");
    let selected_id = setup.selected_character_fighter();
    let selected_label = participant_label(setup.slots[selected_id].participant);
    let styles = setup
        .slots
        .iter()
        .enumerate()
        .map(|(id, slot)| {
            format!(
                "P{} {} {}",
                id + 1,
                style_label(slot.style),
                style_identity(slot.style).tagline
            )
        })
        .collect::<Vec<_>>()
        .join("  ");
    let equipment = setup
        .slots
        .iter()
        .enumerate()
        .map(|(id, slot)| {
            format!(
                "P{} {} {}",
                id + 1,
                equipment_label(slot.equipment),
                equipment_effect_label(slot.equipment)
            )
        })
        .collect::<Vec<_>>()
        .join("  ");
    format!(
        "Local Setup\nChoose Player Character: < {} >\nBot Character: {}\nMode: {}\nArena: {} (available: {})\nBots: {} active\nCharacter Focus: {} P{}  |  Characters: {}\nStyles: {}\nEquipment: {}\nReplay seed: {:08X}\n\nQ/E player previous-next  |  P direct Pig  |  Enter start match\nV bot character  |  Tab focus player/bot  |  C player quick cycle\nZ/X player-bot styles  |  T/Y player-bot equipment\n1 Team  2 FFA  3 Life  |  A next arena  Shift+A previous  |  R reroll seed",
        character_label(player_character),
        character_label(bot_character),
        setup.active_rule_label(),
        selected_arena.name,
        arena_names,
        setup.active_bot_count(),
        selected_label,
        selected_id + 1,
        characters,
        styles,
        equipment,
        setup.replay_seed
    )
}

fn team_scores_for_snapshots(state: &MatchState, snapshots: &[FighterHudSnapshot]) -> (i32, i32) {
    let red_score = snapshots
        .iter()
        .filter(|snapshot| state.fighter_team(snapshot.id) == Some(TeamId::Red))
        .map(|snapshot| snapshot.score)
        .sum();
    let blue_score = snapshots
        .iter()
        .filter(|snapshot| state.fighter_team(snapshot.id) == Some(TeamId::Blue))
        .map(|snapshot| snapshot.score)
        .sum();
    (red_score, blue_score)
}

fn result_message(state: &MatchState, snapshots: &[FighterHudSnapshot]) -> String {
    if state.rules.team_scoring {
        let (red_score, blue_score) = team_scores_for_snapshots(state, snapshots);
        return match red_score.cmp(&blue_score) {
            std::cmp::Ordering::Greater => format!("Red squad wins {red_score}-{blue_score}"),
            std::cmp::Ordering::Less => format!("Blue squad wins {blue_score}-{red_score}"),
            std::cmp::Ordering::Equal => format!("Draw {red_score}-{blue_score}"),
        };
    }

    if state.rules.uses_stocks() {
        let mut survivors = snapshots
            .iter()
            .filter(|snapshot| snapshot.stock.unwrap_or(1) > 0);
        if let Some(winner) = survivors.next() {
            if survivors.next().is_none() {
                return format!("{} survives", winner.name);
            }
        }
    }

    let Some(best_score) = snapshots.iter().map(|snapshot| snapshot.score).max() else {
        return "Results".to_string();
    };
    let leaders: Vec<_> = snapshots
        .iter()
        .filter(|snapshot| snapshot.score == best_score)
        .collect();
    if leaders.len() == 1 {
        format!("{} wins with {}", leaders[0].name, best_score)
    } else {
        format!("Free-for-all draw at {best_score}")
    }
}

fn cooldown_label(seconds: f32) -> String {
    if seconds <= 0.0 {
        "Ready".to_string()
    } else {
        format!("{seconds:.1}s")
    }
}

fn debug_overlay_message(
    state: &MatchState,
    setup: &LocalSetup,
    snapshots: &[FighterHudSnapshot],
    items: &Query<&ArenaItem>,
    specials: &Query<&ActiveSpecial>,
    hitboxes: &Query<&Hitbox>,
    telemetry: &MatchTelemetry,
    feedback: &HitEffects,
) -> String {
    let mut strike_sources = 0;
    let mut grab_sources = 0;
    let mut melee_sources = 0;
    for hitbox in hitboxes {
        match ImpactSource::from_attack_kind(hitbox.kind) {
            ImpactSource::FighterStrike => strike_sources += 1,
            ImpactSource::GrabThrow => grab_sources += 1,
            ImpactSource::ItemMelee => melee_sources += 1,
            _ => {}
        }
    }

    let mut loose_items = 0;
    let mut held_items = 0;
    let mut thrown_items = 0;
    let mut armed_items = 0;
    let mut respawning_items = 0;
    for item in items {
        match item.state {
            ItemState::Loose => loose_items += 1,
            ItemState::Held { .. } => held_items += 1,
            ItemState::Thrown { .. } => thrown_items += 1,
            ItemState::Armed { .. } => armed_items += 1,
            ItemState::Rolling { .. } => thrown_items += 1,
            ItemState::Respawning => respawning_items += 1,
        }
    }

    let mut projectiles = 0;
    let mut traps = 0;
    let mut shockwaves = 0;
    let mut hazards = 0;
    for special in specials {
        match special.kind {
            SpecialKind::Projectile => projectiles += 1,
            SpecialKind::Trap => traps += 1,
            SpecialKind::Shockwave => shockwaves += 1,
            SpecialKind::Hazard => hazards += 1,
        }
    }

    let fighter_line = snapshots
        .iter()
        .map(|snapshot| {
            format!(
                "{} {:?} tech {} c{} b{} hp {:.0} sp {} edge {:.0}%",
                snapshot.name,
                snapshot.action,
                snapshot.technique,
                if snapshot.cancel_window_open {
                    "Y"
                } else {
                    "-"
                },
                if snapshot.branch_window_open {
                    "Y"
                } else {
                    "-"
                },
                snapshot.health,
                cooldown_label(snapshot.special_cooldown),
                snapshot.ringout_danger * 100.0
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let cue = feedback.cue_label().unwrap_or_else(|| "none".to_string());
    let reaction = feedback
        .reaction_label()
        .unwrap_or_else(|| "none".to_string());

    format!(
        "DEBUG\n{}\nCue: {} | Reaction: {}\nSeed {:08X} | RO {} falls {} item {} throws {} breaks {} dmg {:.0}\nHitboxes: {} | strike {} grab {} item-melee {}\nActive sources: throw {} blast {} projectile {} trap {} shockwave {} hazard {}\nItems: loose {} held {} thrown {} armed {} respawn {}\nSpecials: P{} T{} W{} H{}\n{}",
        phase_label(state, setup),
        cue,
        reaction,
        telemetry.replay_seed,
        telemetry.ring_outs,
        telemetry.falls,
        telemetry.item_hits,
        telemetry.throws,
        telemetry.guard_breaks,
        telemetry.total_damage(),
        strike_sources + grab_sources + melee_sources,
        strike_sources,
        grab_sources,
        melee_sources,
        thrown_items,
        armed_items,
        projectiles,
        traps,
        shockwaves,
        hazards,
        loose_items,
        held_items,
        thrown_items,
        armed_items,
        respawning_items,
        projectiles,
        traps,
        shockwaves,
        hazards,
        fighter_line
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(id: usize, name: &'static str, score: i32) -> FighterHudSnapshot {
        FighterHudSnapshot {
            id,
            character: CharacterKind::Cat,
            name,
            color: Color::WHITE,
            health: 100.0,
            stamina: 100.0,
            score,
            stock: None,
            flash: 0.0,
            special_cooldown: 0.0,
            action: FighterAction::Idle,
            technique: "--",
            cancel_window_open: false,
            branch_window_open: false,
            ringout_danger: 0.0,
        }
    }

    #[test]
    fn hud_portraits_use_non_world_render_layers() {
        assert_ne!(hud_portrait_layer(0), 0);
        assert_ne!(hud_portrait_layer(1), hud_portrait_layer(0));
        assert_ne!(hud_portrait_origin(0), hud_portrait_origin(1));
    }

    #[test]
    fn hud_portrait_selection_supports_cat_pig_bee_penguin_and_chick() {
        assert_eq!(
            hud_portrait_character(CharacterKind::Cat),
            CharacterKind::Cat
        );
        assert_eq!(
            hud_portrait_character(CharacterKind::Pig),
            CharacterKind::Pig
        );
        assert_eq!(
            hud_portrait_character(CharacterKind::Bee),
            CharacterKind::Bee
        );
        assert_eq!(
            hud_portrait_character(CharacterKind::Penguin),
            CharacterKind::Penguin
        );
        assert_eq!(
            hud_portrait_character(CharacterKind::Chick),
            CharacterKind::Chick
        );
    }

    #[test]
    fn life_label_uses_stock_when_available() {
        assert_eq!(life_label(Some(2)), "Life 2");
        assert_eq!(life_label(None), "Life --");
    }

    #[test]
    fn timer_label_surfaces_match_phase() {
        let mut state = MatchState::default();
        assert_eq!(timer_label(&state), "SETUP");
        state.reset_for_new_match();
        assert_eq!(timer_label(&state), "--");
        state.phase = MatchPhase::Results;
        assert_eq!(timer_label(&state), "RESULTS");
    }

    #[test]
    fn setup_panel_hides_while_map_editor_is_active() {
        let mut state = MatchState::default();
        state.phase = MatchPhase::Setup;

        assert!(setup_result_panel_visible(&state, false, false));
        assert!(!setup_result_panel_visible(&state, true, false));
        assert!(!setup_result_panel_visible(&state, false, true));

        state.phase = MatchPhase::Results;
        assert!(setup_result_panel_visible(&state, true, false));
        assert!(!setup_result_panel_visible(&state, true, true));

        state.phase = MatchPhase::Fighting;
        assert!(!setup_result_panel_visible(&state, false, false));
    }

    #[test]
    fn setup_screen_uses_local_setup_choices() {
        let mut setup = LocalSetup::default();
        setup.set_rule(2);
        setup.cycle_arena(arena_definitions().len());
        setup.cycle_bot_count();
        setup.cycle_style(0);
        setup.cycle_equipment(0);
        let text = setup_screen_message(&setup);

        assert!(text.contains("Life Ring-Out"));
        assert!(text.contains("Split Causeway"));
        assert!(text.contains("Bots: 1 active"));
        assert!(text.contains("Choose Player Character: < Cat >"));
        assert!(text.contains("Bot Character: Pig"));
        assert!(text.contains("Character Focus: Player P1"));
        assert!(text.contains(">P1 Player Cat"));
        assert!(text.contains("Q/E player previous-next"));
        assert!(text.contains("P direct Pig"));
        assert!(text.contains("Enter start match"));
        assert!(text.contains("P1 Vector"));
        assert!(text.contains("P1 Heavy Seal"));
        assert!(text.contains("rush pressure"));
        assert!(text.contains("heavy launch"));
    }

    #[test]
    fn result_message_reports_team_winner() {
        let state = MatchState::default();
        let snapshots = vec![
            snapshot(0, "Red", 2),
            snapshot(1, "Blue", 1),
            snapshot(2, "Mint", 1),
            snapshot(3, "Pink", 0),
        ];
        assert_eq!(result_message(&state, &snapshots), "Red squad wins 3-1");
    }

    #[test]
    fn result_message_uses_match_state_slot_teams() {
        let mut state = MatchState::default();
        state.teams[2] = TeamId::Blue;
        let snapshots = vec![
            snapshot(0, "Red", 2),
            snapshot(1, "Blue", 1),
            snapshot(2, "Mint", 1),
            snapshot(3, "Pink", 0),
        ];

        assert_eq!(team_scores_for_snapshots(&state, &snapshots), (2, 2));
        assert_eq!(result_message(&state, &snapshots), "Draw 2-2");
    }

    #[test]
    fn result_screen_reports_setup_and_navigation() {
        let mut state = MatchState::default();
        let setup = LocalSetup::default();
        let telemetry = MatchTelemetry::default();
        let snapshots = vec![snapshot(0, "Red", 1), snapshot(1, "Blue", 0)];
        state.phase = MatchPhase::Results;
        state.active_fighter_count = 2;

        let text = result_screen_message(&state, &setup, &snapshots, &telemetry);
        assert!(text.contains("Arena Crown Ring"));
        assert!(text.contains("Bots 1 active"));
        assert!(text.contains("Styles:"));
        assert!(text.contains("Equipment:"));
        assert!(text.contains("Enter for setup"));
    }
}
