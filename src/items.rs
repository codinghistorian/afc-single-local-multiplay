use bevy::gltf::GltfAssetLabel;
use bevy::prelude::*;
use std::f32::consts::PI;

use crate::arena::ground_height_at;
use crate::arena_defs::active_arena_definition;
use crate::combat::{
    DamageDefenderProfile, HitEffects, ImpactFeedbackIntensity, ImpactProfile, ImpactSource,
    apply_impact, can_receive_impact, impact_feedback_profile, impact_profile_from_payload,
    impact_profile_from_payload_with_feel, radial_falloff,
};
use crate::combat_sfx::{CombatSfxCue, CombatSfxKind};
use crate::components::{
    AttackKind, DrunkStatus, Fighter, FighterAction, FighterActionState, FighterInput,
    FighterInventory, FighterMotor, FighterStats, Hitbox,
};
use crate::constants::*;
use crate::effects::{
    EffectAssets, spawn_alcohol_spray, spawn_dust_puff, spawn_guard_flash, spawn_pop_bomb_blast,
};
use crate::equipment::FighterEquipment;
use crate::feel::CombatFeelTuning;
use crate::fighter::cancel_dash_slide_for_action;
use crate::game_state::{Hitstop, MatchAnnouncements, MatchState, MatchTelemetry};
use crate::reactions::ReactionFamilyId;
use crate::styles::FighterStyle;
use crate::techniques::{
    AttackPayloadId, AttackShapeId, DamageElement, DamageProfileId, attack_shape_definition,
};

const STEAMER_BLAST_ARC_MIN_PLANAR_SPEED: f32 = 18.0;
const STEAMER_BLAST_ARC_MAX_PLANAR_SPEED: f32 = 27.0;
const STEAMER_BLAST_ARC_MIN_VERTICAL_SPEED: f32 = 12.0;
const STEAMER_BLAST_ARC_MAX_VERTICAL_SPEED: f32 = 15.5;
const STEAMER_BLAST_ARC_SPEED_LIMIT_TIME: f32 = 1.25;
const ITEM_DROP_SFX_PRIORITY: u8 = 58;
const STEAMER_EXPLOSION_SFX_PRIORITY: u8 = 96;
const MUSHROOM_BIGGER_SFX_PRIORITY: u8 = 64;

fn pop_bomb_overlap_distance(flat_distance: f32, fighter_radius: f32) -> f32 {
    (flat_distance - fighter_radius).max(0.0)
}

fn pop_bomb_body_overlaps(flat_distance: f32, fighter_radius: f32) -> bool {
    pop_bomb_overlap_distance(flat_distance, fighter_radius) <= POP_BOMB_RADIUS
}

fn forced_item_drop_action(action: FighterAction) -> bool {
    visible_forced_item_drop_action(action) || hidden_forced_item_cleanup_action(action)
}

fn visible_forced_item_drop_action(action: FighterAction) -> bool {
    matches!(
        action,
        FighterAction::Knockdown | FighterAction::Grabbed | FighterAction::GuardBroken
    )
}

fn hidden_forced_item_cleanup_action(action: FighterAction) -> bool {
    matches!(action, FighterAction::RingOut | FighterAction::Respawning)
}

fn item_drop_sfx_cue(position: Vec3) -> CombatSfxCue {
    CombatSfxCue::new(CombatSfxKind::ItemDrop, position, ITEM_DROP_SFX_PRIORITY)
}

fn steamer_explosion_sfx_cue(position: Vec3) -> CombatSfxCue {
    CombatSfxCue::new(
        CombatSfxKind::SteamerExplosion,
        position,
        STEAMER_EXPLOSION_SFX_PRIORITY,
    )
}

fn item_use_sfx_cue(kind: ItemKind, position: Vec3) -> Option<CombatSfxCue> {
    match kind {
        ItemKind::Mushroom => Some(CombatSfxCue::new(
            CombatSfxKind::MushroomBigger,
            position,
            MUSHROOM_BIGGER_SFX_PRIORITY,
        )),
        _ => None,
    }
}

#[cfg(test)]
mod steamer_blast_overlap_tests {
    use super::*;

    #[test]
    fn pop_bomb_body_overlap_hits_when_body_touches_red_circle() {
        let fighter_radius = 0.5;
        assert!(pop_bomb_body_overlaps(
            POP_BOMB_RADIUS + fighter_radius,
            fighter_radius,
        ));
    }

    #[test]
    fn pop_bomb_body_overlap_rejects_when_body_is_outside_red_circle() {
        let fighter_radius = 0.5;
        assert!(!pop_bomb_body_overlaps(
            POP_BOMB_RADIUS + fighter_radius + 0.01,
            fighter_radius,
        ));
    }

    #[test]
    fn pop_bomb_falloff_distance_uses_body_edge_not_center() {
        let fighter_radius = 0.5;
        assert_eq!(
            pop_bomb_overlap_distance(POP_BOMB_RADIUS + fighter_radius, fighter_radius),
            POP_BOMB_RADIUS
        );
    }
}

#[derive(Clone, Copy)]
struct ItemDefinition {
    label: &'static str,
    role: ItemRole,
    portable: bool,
    loose_offset: f32,
    max_durability: i32,
    throw_speed: f32,
    throw_arc: f32,
    throw_lifetime: f32,
    throw_owner_grace: f32,
    pickup_lockout: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemRole {
    Recovery,
    Explosive,
    Utility,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Crate,
    Steamer,
    Apple,
    WineWhite,
    Turkey,
    Barrel,
    CupCoffee,
    Mushroom,
}

impl ItemKind {
    pub fn label(self) -> &'static str {
        self.definition().label
    }

    pub fn role(self) -> ItemRole {
        self.definition().role
    }

    pub fn bot_pickup_priority(self) -> f32 {
        match self.role() {
            ItemRole::Recovery => 0.64,
            ItemRole::Explosive => 0.88,
            ItemRole::Utility => 0.7,
        }
    }

    fn definition(self) -> ItemDefinition {
        match self {
            ItemKind::Crate => ItemDefinition {
                label: "Mystery Crate",
                role: ItemRole::Utility,
                portable: true,
                loose_offset: 0.5,
                max_durability: 1,
                throw_speed: ITEM_STONE_CRATE_THROW_SPEED,
                throw_arc: ITEM_STONE_CRATE_THROW_ARC,
                throw_lifetime: ITEM_THROW_LIFETIME,
                throw_owner_grace: ITEM_MALLET_THROW_GRACE,
                pickup_lockout: ITEM_STONE_CRATE_PICKUP_LOCKOUT,
            },
            ItemKind::Steamer => ItemDefinition {
                label: "Steamer",
                role: ItemRole::Explosive,
                portable: true,
                loose_offset: 0.46,
                max_durability: 1,
                throw_speed: ITEM_BOMB_THROW_SPEED,
                throw_arc: ITEM_BOMB_THROW_ARC,
                throw_lifetime: ITEM_THROW_LIFETIME,
                throw_owner_grace: ITEM_BOMB_THROW_GRACE,
                pickup_lockout: ITEM_BOMB_PICKUP_LOCKOUT,
            },
            ItemKind::Apple => ItemDefinition {
                label: "Apple",
                role: ItemRole::Recovery,
                portable: true,
                loose_offset: 0.44,
                max_durability: 1,
                throw_speed: 8.0,
                throw_arc: 0.8,
                throw_lifetime: ITEM_THROW_LIFETIME,
                throw_owner_grace: ITEM_MALLET_THROW_GRACE,
                pickup_lockout: 0.28,
            },
            ItemKind::WineWhite => ItemDefinition {
                label: "White Wine",
                role: ItemRole::Recovery,
                portable: true,
                loose_offset: 0.48,
                max_durability: 1,
                throw_speed: 7.4,
                throw_arc: 0.9,
                throw_lifetime: ITEM_THROW_LIFETIME,
                throw_owner_grace: ITEM_BOMB_THROW_GRACE,
                pickup_lockout: 0.32,
            },
            ItemKind::Turkey => ItemDefinition {
                label: "Turkey",
                role: ItemRole::Recovery,
                portable: true,
                loose_offset: 0.5,
                max_durability: 3,
                throw_speed: 8.6,
                throw_arc: 0.9,
                throw_lifetime: ITEM_THROW_LIFETIME,
                throw_owner_grace: ITEM_MALLET_THROW_GRACE,
                pickup_lockout: 0.34,
            },
            ItemKind::Barrel => ItemDefinition {
                label: "Barrel",
                role: ItemRole::Recovery,
                portable: true,
                loose_offset: 0.56,
                max_durability: 3,
                throw_speed: ITEM_STONE_CRATE_THROW_SPEED,
                throw_arc: ITEM_STONE_CRATE_THROW_ARC,
                throw_lifetime: ITEM_THROW_LIFETIME,
                throw_owner_grace: ITEM_MALLET_THROW_GRACE,
                pickup_lockout: ITEM_STONE_CRATE_PICKUP_LOCKOUT,
            },
            ItemKind::CupCoffee => ItemDefinition {
                label: "Coffee",
                role: ItemRole::Utility,
                portable: true,
                loose_offset: 0.42,
                max_durability: 1,
                throw_speed: 7.2,
                throw_arc: 1.0,
                throw_lifetime: ITEM_THROW_LIFETIME,
                throw_owner_grace: ITEM_MALLET_THROW_GRACE,
                pickup_lockout: 0.32,
            },
            ItemKind::Mushroom => ItemDefinition {
                label: "Mushroom",
                role: ItemRole::Utility,
                portable: true,
                loose_offset: 0.46,
                max_durability: 1,
                throw_speed: 7.6,
                throw_arc: 1.1,
                throw_lifetime: ITEM_THROW_LIFETIME,
                throw_owner_grace: ITEM_MALLET_THROW_GRACE,
                pickup_lockout: 0.36,
            },
        }
    }

    fn is_portable(self) -> bool {
        self.definition().portable
    }

    fn loose_offset(self) -> f32 {
        self.definition().loose_offset
    }

    fn max_durability(self) -> i32 {
        self.definition().max_durability
    }

    fn throw_speed(self) -> f32 {
        self.definition().throw_speed
    }

    fn throw_arc(self) -> f32 {
        self.definition().throw_arc
    }

    fn throw_lifetime(self) -> f32 {
        self.definition().throw_lifetime
    }

    fn throw_owner_grace(self) -> f32 {
        self.definition().throw_owner_grace
    }

    fn pickup_lockout(self) -> f32 {
        self.definition().pickup_lockout
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ItemState {
    Loose,
    Held {
        holder: Entity,
    },
    Thrown {
        owner: Entity,
        owner_id: usize,
        lifetime: f32,
        grace: f32,
    },
    Armed {
        owner: Entity,
        owner_id: usize,
        timer: f32,
        grace: f32,
    },
    Spraying {
        owner: Entity,
        owner_id: usize,
        lifetime: f32,
        spray_timer: f32,
        spiral_phase: f32,
        spiral_radius: f32,
    },
    Rolling {
        lifetime: f32,
    },
    Respawning,
}

#[derive(Component)]
pub struct ArenaItem {
    pub kind: ItemKind,
    pub state: ItemState,
    pub respawn_timer: f32,
    pub durability: i32,
    pub max_durability: i32,
    pub pickup_lockout: f32,
    pub anchor: Vec3,
    pub velocity: Vec3,
    pub already_hit: Vec<Entity>,
    pub base_y: f32,
    phase: f32,
}

impl ArenaItem {
    pub fn new(kind: ItemKind, anchor: Vec3, phase: f32) -> Self {
        Self {
            kind,
            state: ItemState::Loose,
            respawn_timer: 0.0,
            durability: kind.max_durability(),
            max_durability: kind.max_durability(),
            pickup_lockout: 0.0,
            anchor,
            velocity: Vec3::ZERO,
            already_hit: Vec::new(),
            base_y: anchor.y,
            phase,
        }
    }

    pub fn is_held_by(&self, holder: Entity) -> bool {
        matches!(self.state, ItemState::Held { holder: active_holder } if active_holder == holder)
    }

    pub fn reset_for_match(&mut self) {
        self.state = ItemState::Loose;
        self.respawn_timer = 0.0;
        self.durability = self.max_durability;
        self.pickup_lockout = 0.0;
        self.velocity = Vec3::ZERO;
        self.already_hit.clear();
        self.base_y = self.anchor.y;
    }

    pub fn retarget_for_anchor(&mut self, kind: ItemKind, anchor: Vec3, phase: f32) {
        self.kind = kind;
        self.anchor = anchor;
        self.phase = phase;
        self.max_durability = kind.max_durability();
        self.reset_for_match();
    }

    pub fn deactivate_for_match(&mut self) {
        self.state = ItemState::Respawning;
        self.respawn_timer = f32::MAX;
        self.velocity = Vec3::ZERO;
        self.pickup_lockout = 0.0;
        self.already_hit.clear();
    }

    #[allow(dead_code)]
    pub fn status_label(&self) -> String {
        match self.kind {
            ItemKind::Turkey | ItemKind::Barrel => {
                format!(
                    "{} {}/{}",
                    self.kind.label(),
                    self.durability.max(0),
                    self.max_durability
                )
            }
            _ => self.kind.label().to_string(),
        }
    }

    fn loose_pickup_ready(&self) -> bool {
        matches!(self.state, ItemState::Loose)
            && self.pickup_lockout <= 0.0
            && self.kind.is_portable()
    }

    pub fn pickup_as(&mut self, holder: Entity) {
        self.state = ItemState::Held { holder };
        self.velocity = Vec3::ZERO;
        self.already_hit.clear();
        self.pickup_lockout = 0.0;
    }

    pub fn launch_as_thrown(&mut self, owner: Entity, owner_id: usize, velocity: Vec3) {
        self.velocity = velocity;
        self.already_hit.clear();
        self.state = ItemState::Thrown {
            owner,
            owner_id,
            lifetime: self.kind.throw_lifetime(),
            grace: self.kind.throw_owner_grace(),
        };
        self.pickup_lockout = self.kind.pickup_lockout();
    }

    pub fn arm_as_bomb(&mut self, owner: Entity, owner_id: usize, velocity: Vec3) {
        self.velocity = velocity;
        self.already_hit.clear();
        self.state = ItemState::Armed {
            owner,
            owner_id,
            timer: POP_BOMB_FUSE,
            grace: self.kind.throw_owner_grace(),
        };
        self.pickup_lockout = self.kind.pickup_lockout();
    }

    pub fn start_barrel_spray(&mut self, owner: Entity, owner_id: usize) {
        let planar_speed = Vec2::new(self.velocity.x, self.velocity.z).length();
        self.state = ItemState::Spraying {
            owner,
            owner_id,
            lifetime: BARREL_SPRAY_DURATION,
            spray_timer: 0.0,
            spiral_phase: 0.0,
            spiral_radius: planar_speed.max(0.2),
        };
        self.velocity.y = 0.0;
        self.pickup_lockout = BARREL_SPRAY_DURATION;
        self.already_hit.clear();
    }

    pub fn roll_loose(&mut self, velocity: Vec3) {
        self.velocity = velocity;
        self.already_hit.clear();
        self.pickup_lockout = ITEM_DROP_ROLL_PICKUP_LOCKOUT;
        self.state = ItemState::Rolling {
            lifetime: ITEM_DROP_ROLL_LIFETIME,
        };
    }

    fn set_respawning(&mut self) {
        self.state = ItemState::Respawning;
        self.respawn_timer = ITEM_RESPAWN_SECONDS;
        self.velocity = Vec3::ZERO;
        self.pickup_lockout = 0.0;
        self.already_hit.clear();
    }
}

#[derive(Resource)]
pub struct ItemAssets {
    item_mesh: Handle<Mesh>,
    steamer_scene: Handle<Scene>,
    apple_scene: Handle<Scene>,
    wine_white_scene: Handle<Scene>,
    turkey_scene: Handle<Scene>,
    barrel_scene: Handle<Scene>,
    cup_coffee_scene: Handle<Scene>,
    mushroom_scene: Handle<Scene>,
    crate_scene: Handle<Scene>,
    steamer_material: Handle<StandardMaterial>,
    apple_material: Handle<StandardMaterial>,
    wine_white_material: Handle<StandardMaterial>,
    turkey_material: Handle<StandardMaterial>,
    barrel_material: Handle<StandardMaterial>,
    coffee_material: Handle<StandardMaterial>,
    mushroom_material: Handle<StandardMaterial>,
    crate_material: Handle<StandardMaterial>,
    live_bomb_material: Handle<StandardMaterial>,
}

impl ItemAssets {
    pub fn material_for(&self, kind: ItemKind, live_bomb: bool) -> Handle<StandardMaterial> {
        match kind {
            ItemKind::Crate => self.crate_material.clone(),
            ItemKind::Steamer if live_bomb => self.live_bomb_material.clone(),
            ItemKind::Steamer => self.steamer_material.clone(),
            ItemKind::Apple => self.apple_material.clone(),
            ItemKind::WineWhite => self.wine_white_material.clone(),
            ItemKind::Turkey => self.turkey_material.clone(),
            ItemKind::Barrel => self.barrel_material.clone(),
            ItemKind::CupCoffee => self.coffee_material.clone(),
            ItemKind::Mushroom => self.mushroom_material.clone(),
        }
    }

    pub fn scene_for(&self, kind: ItemKind) -> Handle<Scene> {
        match kind {
            ItemKind::Crate => self.crate_scene.clone(),
            ItemKind::Steamer => self.steamer_scene.clone(),
            ItemKind::Apple => self.apple_scene.clone(),
            ItemKind::WineWhite => self.wine_white_scene.clone(),
            ItemKind::Turkey => self.turkey_scene.clone(),
            ItemKind::Barrel => self.barrel_scene.clone(),
            ItemKind::CupCoffee => self.cup_coffee_scene.clone(),
            ItemKind::Mushroom => self.mushroom_scene.clone(),
        }
    }
}

pub fn setup_items(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let assets = ItemAssets {
        item_mesh: meshes.add(Cuboid::new(0.01, 0.01, 0.01)),
        steamer_scene: food_scene(&asset_server, "steamer.glb"),
        apple_scene: food_scene(&asset_server, "apple.glb"),
        wine_white_scene: food_scene(&asset_server, "wine-white.glb"),
        turkey_scene: food_scene(&asset_server, "turkey.glb"),
        barrel_scene: food_scene(&asset_server, "barrel.glb"),
        cup_coffee_scene: food_scene(&asset_server, "cup-coffee.glb"),
        mushroom_scene: food_scene(&asset_server, "mushroom.glb"),
        crate_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset("arena/kits/platformer/crate-strong.glb")),
        steamer_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.72, 0.62, 0.52),
            perceptual_roughness: 0.46,
            ..default()
        }),
        apple_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.95, 0.08, 0.04),
            perceptual_roughness: 0.44,
            ..default()
        }),
        wine_white_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.94, 0.88, 0.58),
            emissive: LinearRgba::rgb(0.06, 0.05, 0.01),
            perceptual_roughness: 0.34,
            ..default()
        }),
        turkey_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.8, 0.45, 0.22),
            perceptual_roughness: 0.52,
            ..default()
        }),
        barrel_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.48, 0.28, 0.14),
            perceptual_roughness: 0.7,
            ..default()
        }),
        coffee_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.94, 0.72, 0.48),
            emissive: LinearRgba::rgb(0.08, 0.04, 0.01),
            perceptual_roughness: 0.38,
            ..default()
        }),
        mushroom_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.86, 0.24, 0.18),
            emissive: LinearRgba::rgb(0.08, 0.01, 0.01),
            perceptual_roughness: 0.42,
            ..default()
        }),
        crate_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.54, 0.31, 0.12),
            perceptual_roughness: 0.78,
            ..default()
        }),
        live_bomb_material: materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.44, 0.08),
            emissive: LinearRgba::rgb(0.24, 0.08, 0.01),
            perceptual_roughness: 0.32,
            ..default()
        }),
    };

    for anchor in active_arena_definition().item_anchors {
        spawn_pickup(
            &mut commands,
            &assets,
            anchor.kind,
            anchor.position,
            anchor.phase,
        );
    }

    commands.insert_resource(assets);
}

fn food_scene(asset_server: &AssetServer, file: &str) -> Handle<Scene> {
    asset_server.load(GltfAssetLabel::Scene(0).from_asset(format!("food/kenney_food_kit/{file}")))
}

fn spawn_pickup(
    commands: &mut Commands,
    assets: &ItemAssets,
    kind: ItemKind,
    position: Vec3,
    phase: f32,
) -> Entity {
    let (mesh, material, scale) = item_visuals(assets, kind, false);

    commands
        .spawn((
            Mesh3d(mesh),
            MeshMaterial3d(material),
            SceneRoot(assets.scene_for(kind)),
            Transform::from_translation(position).with_scale(scale),
            ArenaItem::new(kind, position, phase),
            Name::new(kind.label()),
        ))
        .id()
}

fn item_visuals(
    assets: &ItemAssets,
    kind: ItemKind,
    live_bomb: bool,
) -> (Handle<Mesh>, Handle<StandardMaterial>, Vec3) {
    match kind {
        ItemKind::Crate
        | ItemKind::Steamer
        | ItemKind::Apple
        | ItemKind::WineWhite
        | ItemKind::Turkey
        | ItemKind::Barrel
        | ItemKind::CupCoffee
        | ItemKind::Mushroom => (
            assets.item_mesh.clone(),
            assets.material_for(kind, live_bomb),
            item_scale(kind),
        ),
    }
}

pub fn handle_item_inputs(
    mut commands: Commands,
    hitstop: Res<Hitstop>,
    effect_assets: Res<EffectAssets>,
    mut feedback: ResMut<HitEffects>,
    mut announcements: ResMut<MatchAnnouncements>,
    mut fighters: Query<
        (
            Entity,
            &Fighter,
            &mut FighterInput,
            &mut FighterMotor,
            &mut FighterStats,
            &mut FighterInventory,
            &mut FighterActionState,
            &Transform,
        ),
        Without<ArenaItem>,
    >,
    mut items: Query<(
        Entity,
        &mut ArenaItem,
        &mut Transform,
        &mut Visibility,
        &mut MeshMaterial3d<StandardMaterial>,
    )>,
    assets: Res<ItemAssets>,
) {
    if hitstop.active() {
        return;
    }

    for (
        fighter_entity,
        fighter,
        mut input,
        mut motor,
        mut stats,
        mut inventory,
        mut action,
        fighter_transform,
    ) in &mut fighters
    {
        if !can_use_item_input(action.action) {
            continue;
        }

        if let Some(held_entity) = inventory.held {
            let Ok((_, mut item, mut item_transform, mut visibility, mut material)) =
                items.get_mut(held_entity)
            else {
                inventory.held = None;
                continue;
            };
            if held_reference_is_stale(Some(&*item), fighter_entity) {
                inventory.held = None;
                continue;
            }

            let command = held_item_command(&input, item.kind);
            sanitize_held_item_inputs(&mut input);

            let facing = motor.facing.normalize_or_zero();
            match command {
                HeldItemCommand::Throw => {
                    cancel_dash_slide_for_action(&mut motor);
                    let throw_velocity = facing * item.kind.throw_speed()
                        + motor.velocity * 0.45
                        + Vec3::Y * item.kind.throw_arc();
                    throw_item(
                        fighter_entity,
                        fighter.id,
                        &mut item,
                        &mut item_transform,
                        &mut visibility,
                        &mut material,
                        &assets,
                        fighter_transform.translation + Vec3::Y * 0.82 + facing * 0.65,
                        throw_velocity,
                    );
                    inventory.held = None;
                    set_item_action(&mut action, FighterAction::ItemThrow);
                    input.light = false;
                    input.heavy = false;
                    announcements
                        .show(format!("{} threw {}", fighter.name, item.kind.label()), 1.0);
                    feedback.push_feedback_cue("item_throw", ImpactSource::ItemThrow, 24);
                    continue;
                }
                HeldItemCommand::Use => {
                    cancel_dash_slide_for_action(&mut motor);
                    if use_held_item(
                        &mut commands,
                        &effect_assets,
                        &mut feedback,
                        &mut announcements,
                        fighter,
                        &mut stats,
                        &mut item,
                        fighter_transform.translation,
                    ) {
                        item.durability -= 1;
                        if item.durability <= 0 {
                            item.set_respawning();
                            *visibility = Visibility::Hidden;
                            inventory.held = None;
                        }
                        set_item_action(&mut action, FighterAction::ItemSwing);
                        input.light = false;
                    } else {
                        set_item_action(&mut action, FighterAction::ItemSwing);
                        input.light = false;
                    }
                    continue;
                }
                HeldItemCommand::None => {}
            }

            continue;
        }

        if !input.light || !motor.grounded || portable_pickup_blocked(action.action, &motor) {
            continue;
        }

        let Some(item_entity) = nearest_portable_item(
            fighter_transform.translation,
            motor.facing,
            stats.item_size_multiplier(),
            &mut items,
        ) else {
            continue;
        };
        let Ok((_, mut item, mut item_transform, mut visibility, _)) = items.get_mut(item_entity)
        else {
            continue;
        };
        item.pickup_as(fighter_entity);
        inventory.held = Some(item_entity);
        *visibility = Visibility::Visible;
        item_transform.translation = fighter_transform.translation + Vec3::Y * 0.95;
        cancel_dash_slide_for_action(&mut motor);
        set_item_action(&mut action, FighterAction::ItemPickup);
        input.light = false;
        announcements.show(
            format!("{} picked up {}", fighter.name, item.kind.label()),
            1.0,
        );
        feedback.push_feedback_cue("item_pickup", ImpactSource::ItemUtility, 18);
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum HeldItemCommand {
    Use,
    Throw,
    None,
}

fn held_item_command(input: &FighterInput, kind: ItemKind) -> HeldItemCommand {
    if matches!(kind, ItemKind::Steamer | ItemKind::Crate) {
        if input.heavy || input.light {
            return HeldItemCommand::Throw;
        }
        return HeldItemCommand::None;
    }

    if input.heavy {
        return HeldItemCommand::Throw;
    }

    if input.light {
        return HeldItemCommand::Use;
    }

    HeldItemCommand::None
}

fn use_held_item(
    commands: &mut Commands,
    effect_assets: &EffectAssets,
    feedback: &mut HitEffects,
    announcements: &mut MatchAnnouncements,
    fighter: &Fighter,
    stats: &mut FighterStats,
    item: &mut ArenaItem,
    position: Vec3,
) -> bool {
    let message = match item.kind {
        ItemKind::Crate => return false,
        ItemKind::Apple => {
            stats.health = (stats.health + ITEM_APPLE_HEALTH).min(MAX_HEALTH);
            "ate Apple"
        }
        ItemKind::WineWhite => {
            stats.stamina = (stats.stamina + ITEM_WINE_WHITE_STAMINA).min(MAX_STAMINA);
            "drank White Wine"
        }
        ItemKind::Turkey => {
            stats.health = (stats.health + ITEM_TURKEY_HEALTH).min(MAX_HEALTH);
            "ate Turkey"
        }
        ItemKind::Barrel => {
            stats.stamina = (stats.stamina + ITEM_BARREL_STAMINA).min(MAX_STAMINA);
            "drank Barrel"
        }
        ItemKind::CupCoffee => {
            stats.item_speed_timer = ITEM_COFFEE_SPEED_SECONDS;
            "drank Coffee"
        }
        ItemKind::Mushroom => {
            stats.item_giant_timer = ITEM_MUSHROOM_GIANT_SECONDS;
            "ate Mushroom"
        }
        ItemKind::Steamer => return false,
    };

    stats.hud_flash = stats.hud_flash.max(0.28);
    spawn_guard_flash(commands, effect_assets, position + Vec3::Y * 1.05);
    feedback.push_feedback_cue("item_use", ImpactSource::ItemUtility, 22);
    if let Some(cue) = item_use_sfx_cue(item.kind, position + Vec3::Y * 1.05) {
        feedback.push_combat_sfx(cue);
    }
    announcements.show(format!("{} {}", fighter.name, message), 1.0);
    true
}

fn sanitize_held_item_inputs(input: &mut FighterInput) {
    input.grab = false;
    input.ultimate = false;
    input.special = false;
    input.light = false;
    input.light_held = false;
    input.raw_light_pressed = false;
    input.heavy_held = false;
    input.raw_heavy_pressed = false;
    input.heavy_released = false;
    input.heavy = false;
}

fn can_use_item_input(action: FighterAction) -> bool {
    matches!(
        action,
        FighterAction::Idle
            | FighterAction::Moving
            | FighterAction::Jumping
            | FighterAction::Dashing
            | FighterAction::Guarding
    )
}

fn portable_pickup_blocked(action: FighterAction, motor: &FighterMotor) -> bool {
    action == FighterAction::Dashing || motor.dash_slide_timer > 0.0
}

fn set_item_action(action: &mut FighterActionState, next: FighterAction) {
    action.action = next;
    action.elapsed = 0.0;
    action.hitbox_spawned = false;
    action.queued_combo = false;
    action.queued_technique = None;
    action.queued_button = None;
    action.buffered_button = None;
    action.buffered_button_elapsed = 0.0;
    action.timeline_events_fired = 0;
    action.reaction_getup_ms = None;
    action.reaction_recover_ms = None;
    action.clear_reaction_visual();
}

fn nearest_portable_item(
    fighter_pos: Vec3,
    facing: Vec3,
    fighter_size: f32,
    items: &mut Query<(
        Entity,
        &mut ArenaItem,
        &mut Transform,
        &mut Visibility,
        &mut MeshMaterial3d<StandardMaterial>,
    )>,
) -> Option<Entity> {
    let mut best: Option<(Entity, f32)> = None;
    let facing = facing.normalize_or_zero();
    for (entity, item, transform, _, _) in items.iter_mut() {
        if !item.loose_pickup_ready() {
            continue;
        }
        let delta = transform.translation - fighter_pos;
        let flat = Vec2::new(delta.x, delta.z);
        let distance = flat.length();
        let contact_range = FIGHTER_RADIUS * fighter_size + item_pickup_radius(item.kind);
        let pickup_range = ITEM_PICKUP_RANGE.max(contact_range);
        if distance > pickup_range {
            continue;
        }
        let dir = Vec3::new(delta.x, 0.0, delta.z).normalize_or_zero();
        if distance > contact_range && facing.dot(dir) < ITEM_PICKUP_CONE_DOT {
            continue;
        }
        if best.map_or(true, |(_, best_dist)| distance < best_dist) {
            best = Some((entity, distance));
        }
    }
    best.map(|(entity, _)| entity)
}

fn item_pickup_radius(kind: ItemKind) -> f32 {
    item_scale(kind).x.max(item_scale(kind).z) * 0.34
}

fn throw_item(
    owner: Entity,
    owner_id: usize,
    item: &mut ArenaItem,
    transform: &mut Transform,
    visibility: &mut Visibility,
    material: &mut MeshMaterial3d<StandardMaterial>,
    assets: &ItemAssets,
    position: Vec3,
    velocity: Vec3,
) {
    *visibility = Visibility::Visible;
    let position = if velocity.length_squared() < 0.01 {
        let ground = ground_height_at(position.x, position.z).unwrap_or(ARENA_TOP_Y);
        Vec3::new(position.x, ground + item.kind.loose_offset(), position.z)
    } else {
        position
    };
    transform.translation = position;
    transform.scale = item_scale(item.kind);
    if item.kind == ItemKind::Steamer {
        material.0 = assets.live_bomb_material.clone();
        arm_bomb(
            item, transform, visibility, position, velocity, owner, owner_id,
        );
    } else {
        item.launch_as_thrown(owner, owner_id, velocity);
    }
}

fn arm_bomb(
    item: &mut ArenaItem,
    transform: &mut Transform,
    visibility: &mut Visibility,
    position: Vec3,
    velocity: Vec3,
    owner: Entity,
    owner_id: usize,
) {
    *visibility = Visibility::Visible;
    let position = if velocity.length_squared() < 0.01 {
        let ground = ground_height_at(position.x, position.z).unwrap_or(ARENA_TOP_Y);
        Vec3::new(position.x, ground + item.kind.loose_offset(), position.z)
    } else {
        position
    };
    transform.translation = position;
    transform.scale = item_scale(item.kind);
    item.arm_as_bomb(owner, owner_id, velocity);
}

fn place_loose(
    item: &mut ArenaItem,
    transform: &mut Transform,
    visibility: &mut Visibility,
    position: Vec3,
) {
    let ground = ground_height_at(position.x, position.z).unwrap_or(ARENA_TOP_Y);
    transform.translation = Vec3::new(position.x, ground + item.kind.loose_offset(), position.z);
    transform.scale = item_scale(item.kind);
    item.base_y = transform.translation.y;
    item.velocity = Vec3::ZERO;
    item.pickup_lockout = item.kind.pickup_lockout();
    item.already_hit.clear();
    item.state = ItemState::Loose;
    *visibility = Visibility::Visible;
}

fn begin_barrel_spray(
    item: &mut ArenaItem,
    transform: &mut Transform,
    owner: Entity,
    owner_id: usize,
) {
    if let Some(ground_y) = ground_height_at(transform.translation.x, transform.translation.z) {
        transform.translation.y = ground_y + item.kind.loose_offset();
    }
    transform.rotation = Quat::IDENTITY;
    item.start_barrel_spray(owner, owner_id);
}

fn advance_barrel_spray_timer(timer: f32, dt: f32) -> (bool, f32) {
    let timer = timer - dt;
    if timer <= 0.0 {
        (true, timer + BARREL_SPRAY_CADENCE)
    } else {
        (false, timer)
    }
}

pub fn spawn_item_hitboxes(
    mut commands: Commands,
    hitstop: Res<Hitstop>,
    mut fighters: Query<
        (
            Entity,
            &Fighter,
            &FighterMotor,
            &FighterInventory,
            &mut FighterActionState,
            &Transform,
        ),
        Without<ArenaItem>,
    >,
    mut items: Query<&mut ArenaItem>,
) {
    if hitstop.active() {
        return;
    }

    for (entity, fighter, motor, inventory, mut action, transform) in &mut fighters {
        if action.action != FighterAction::ItemSwing
            || action.hitbox_spawned
            || action.elapsed < ITEM_SWING_STARTUP
        {
            continue;
        }

        action.hitbox_spawned = true;
        let Some(item_entity) = inventory.held else {
            continue;
        };
        let Ok(mut item) = items.get_mut(item_entity) else {
            continue;
        };
        let Some(config) = item_swing_config(item.kind) else {
            continue;
        };

        item.durability -= 1;
        let facing = motor.facing.normalize_or_zero();
        let shape = attack_shape_definition(AttackShapeId::ItemMelee);
        let center =
            transform.translation + Vec3::Y * (FIGHTER_HEIGHT * 0.62) + facing * config.range;
        commands.spawn((
            Hitbox {
                owner: entity,
                owner_id: fighter.id,
                kind: AttackKind::ItemSwing,
                payload_id: None,
                attacker_character: None,
                technique_id: None,
                hit_effect: None,
                shape_id: AttackShapeId::ItemMelee,
                reaction_family: ReactionFamilyId::GroundedDownGetup,
                damage_profile: DamageProfileId::ItemHeavy,
                element: DamageElement::Earth,
                attacker_equipment: None,
                attacker_style: None,
                power: config.damage,
                str_scale: 1.0,
                damage: config.damage,
                knockback: config.knockback,
                vertical_knockback: 1.2,
                guardable: true,
                base_radius: config.radius,
                radius: config.radius,
                lifetime: ITEM_SWING_ACTIVE,
                elapsed: 0.0,
                total_lifetime: ITEM_SWING_ACTIVE,
                spawn_origin: transform.translation,
                facing,
                base_range: config.range,
                range: config.range,
                scales_with_owner_size: false,
                vertical_offset_scale: shape.vertical_offset_scale,
                parented: shape.parented,
                path: shape.path,
                expires_on_owner_landing: false,
                landing_linger: 0.0,
                landing_linger_started: false,
                ground_path_end: false,
                ground_path_clearance: 0.0,
                impact_cue: "impact_item_swing",
                hitstop_scale: 1.1,
                shake_scale: 1.1,
                feedback_priority_bonus: 4,
                already_hit: Vec::new(),
            },
            Transform::from_translation(center),
        ));
    }
}

struct ItemSwingConfig {
    damage: f32,
    knockback: f32,
    range: f32,
    radius: f32,
}

fn item_swing_config(kind: ItemKind) -> Option<ItemSwingConfig> {
    match kind {
        ItemKind::Steamer
        | ItemKind::Crate
        | ItemKind::Apple
        | ItemKind::WineWhite
        | ItemKind::Turkey
        | ItemKind::Barrel
        | ItemKind::CupCoffee
        | ItemKind::Mushroom => None,
    }
}

pub fn update_items(
    time: Res<Time>,
    mut items: Query<(&mut ArenaItem, &mut Transform, &mut Visibility)>,
) {
    let dt = time.delta_secs();
    let elapsed = time.elapsed_secs();

    for (mut item, mut transform, mut visibility) in &mut items {
        match item.state {
            ItemState::Respawning => {
                item.respawn_timer -= dt;
                if item.respawn_timer <= 0.0 {
                    item.reset_for_match();
                    transform.translation = item.anchor;
                    transform.scale = item_scale(item.kind);
                    *visibility = Visibility::Visible;
                }
            }
            ItemState::Loose => {
                item.pickup_lockout = (item.pickup_lockout - dt).max(0.0);
                transform.translation.y = item.base_y + (elapsed * 2.8 + item.phase).sin() * 0.08;
                transform.rotate_y(dt * 1.6);
            }
            _ => {}
        }
    }
}

pub fn drop_items_from_disabled_fighters(
    mut feedback: ResMut<HitEffects>,
    mut fighters: Query<
        (
            Entity,
            &mut FighterInventory,
            &FighterActionState,
            &FighterMotor,
            &Transform,
        ),
        Without<ArenaItem>,
    >,
    mut items: Query<(&mut ArenaItem, &mut Transform, &mut Visibility), Without<Fighter>>,
) {
    for (fighter_entity, mut inventory, action, motor, fighter_transform) in &mut fighters {
        let Some(item_entity) = inventory.held else {
            continue;
        };
        if action.action != FighterAction::ItemSwing {
            if let Ok((mut item, _, mut visibility)) = items.get_mut(item_entity) {
                if held_reference_is_stale(Some(&*item), fighter_entity) {
                    inventory.held = None;
                    continue;
                }
                if item.durability <= 0 {
                    inventory.held = None;
                    item.set_respawning();
                    *visibility = Visibility::Hidden;
                    continue;
                }
            } else {
                inventory.held = None;
                continue;
            }
        }
        if !forced_item_drop_action(action.action) {
            continue;
        }
        let Ok((mut item, mut transform, mut visibility)) = items.get_mut(item_entity) else {
            inventory.held = None;
            continue;
        };
        if held_reference_is_stale(Some(&*item), fighter_entity) {
            inventory.held = None;
            continue;
        }
        inventory.held = None;
        if hidden_forced_item_cleanup_action(action.action) {
            item.set_respawning();
            *visibility = Visibility::Hidden;
        } else {
            let facing = motor.facing.normalize_or_zero();
            place_rolling(
                &mut item,
                &mut transform,
                &mut visibility,
                fighter_transform.translation + facing * 0.45,
                dropped_item_roll_velocity(fighter_transform.translation, facing),
            );
            feedback.push_combat_sfx(item_drop_sfx_cue(transform.translation));
        }
    }
}

fn place_rolling(
    item: &mut ArenaItem,
    transform: &mut Transform,
    visibility: &mut Visibility,
    position: Vec3,
    velocity: Vec3,
) {
    let ground = ground_height_at(position.x, position.z).unwrap_or(ARENA_TOP_Y);
    transform.translation = Vec3::new(position.x, ground + item.kind.loose_offset(), position.z);
    transform.scale = item_scale(item.kind);
    item.base_y = transform.translation.y;
    item.roll_loose(velocity);
    *visibility = Visibility::Visible;
}

fn dropped_item_roll_velocity(position: Vec3, facing: Vec3) -> Vec3 {
    let seed = (position.x * 12.9898 + position.z * 78.233).sin();
    let angle = seed * PI;
    let random_dir = Quat::from_rotation_y(angle) * facing.normalize_or_zero();
    let fallback = if random_dir.length_squared() > 0.01 {
        random_dir
    } else {
        Vec3::X
    };
    let speed = 2.0 + seed.abs() * 2.2;
    Vec3::new(fallback.x, 0.0, fallback.z).normalize_or_zero() * speed + Vec3::Y * 1.15
}

pub fn update_moving_items(
    time: Res<Time>,
    mut commands: Commands,
    effect_assets: Res<EffectAssets>,
    assets: Res<ItemAssets>,
    state: Res<MatchState>,
    feel: Res<CombatFeelTuning>,
    mut hitstop: ResMut<Hitstop>,
    mut camera_effects: ResMut<HitEffects>,
    mut telemetry: ResMut<MatchTelemetry>,
    mut items: Query<(
        Entity,
        &mut ArenaItem,
        &mut Transform,
        &mut Visibility,
        &mut MeshMaterial3d<StandardMaterial>,
    )>,
    mut fighters: Query<
        (
            Entity,
            &Fighter,
            &mut FighterStats,
            &mut FighterMotor,
            &mut FighterActionState,
            &mut DrunkStatus,
            &FighterStyle,
            &FighterEquipment,
            &Transform,
        ),
        Without<ArenaItem>,
    >,
) {
    if hitstop.active() {
        return;
    }

    let dt = time.delta_secs();
    for (_item_entity, mut item, mut transform, mut visibility, mut material) in &mut items {
        match item.state {
            ItemState::Thrown {
                owner,
                owner_id,
                mut lifetime,
                mut grace,
            } => {
                lifetime -= dt;
                grace = (grace - dt).max(0.0);
                item.velocity.y -= GRAVITY * dt;
                transform.translation += item.velocity * dt;
                transform.rotate_x(dt * 9.0);
                transform.rotate_z(dt * 4.0);

                let mut impacted = false;
                for (
                    target_entity,
                    target,
                    mut stats,
                    mut motor,
                    mut action,
                    _drunk,
                    target_style,
                    target_equipment,
                    target_transform,
                ) in &mut fighters
                {
                    if target_entity == owner && grace > 0.0 {
                        continue;
                    }
                    if !state.combat_target_allowed_for_state(owner_id, target.id) {
                        continue;
                    }
                    if item.already_hit.contains(&target_entity)
                        || !can_receive_impact(&stats, &action)
                    {
                        continue;
                    }
                    let hurt_center =
                        target_transform.translation + Vec3::Y * (FIGHTER_HEIGHT * 0.58);
                    if hurt_center.distance(transform.translation)
                        > ITEM_THROW_RADIUS + FIGHTER_RADIUS * stats.item_size_multiplier()
                    {
                        continue;
                    }
                    apply_impact(
                        &mut commands,
                        &effect_assets,
                        &mut camera_effects,
                        &mut hitstop,
                        &state,
                        &mut stats,
                        &mut motor,
                        &mut action,
                        target_transform,
                        None,
                        transform.translation,
                        item_throw_profile_with_feel(item.kind, owner_id, &feel),
                        DamageDefenderProfile::from_loadout(target_style, target_equipment),
                        &mut telemetry,
                    );
                    item.already_hit.push(target_entity);
                    item.durability -= 1;
                    impacted = true;
                    break;
                }

                if impacted {
                    if item.kind == ItemKind::Crate {
                        spawn_dust_puff(&mut commands, &effect_assets, transform.translation);
                        open_mystery_crate(
                            &mut commands,
                            &assets,
                            &mut item,
                            &mut visibility,
                            transform.translation,
                            time.elapsed_secs(),
                        );
                        continue;
                    }
                    if item.kind == ItemKind::Barrel {
                        begin_barrel_spray(&mut item, &mut transform, owner, owner_id);
                        continue;
                    }
                    if item.durability <= 0 {
                        spawn_dust_puff(&mut commands, &effect_assets, transform.translation);
                        item.set_respawning();
                        *visibility = Visibility::Hidden;
                    } else {
                        let settle_position = transform.translation;
                        place_loose(&mut item, &mut transform, &mut visibility, settle_position);
                    }
                    continue;
                }

                if should_respawn_item(transform.translation) || lifetime <= 0.0 {
                    item.set_respawning();
                    *visibility = Visibility::Hidden;
                    continue;
                }

                if let Some(ground_y) =
                    ground_height_at(transform.translation.x, transform.translation.z)
                {
                    if transform.translation.y <= ground_y + item.kind.loose_offset()
                        && item.velocity.y <= 0.0
                    {
                        if item.kind == ItemKind::Crate {
                            spawn_dust_puff(&mut commands, &effect_assets, transform.translation);
                            open_mystery_crate(
                                &mut commands,
                                &assets,
                                &mut item,
                                &mut visibility,
                                transform.translation,
                                time.elapsed_secs(),
                            );
                            continue;
                        }
                        if item.kind == ItemKind::Barrel {
                            item.durability -= 1;
                            begin_barrel_spray(&mut item, &mut transform, owner, owner_id);
                            continue;
                        }
                        let settle_position = transform.translation;
                        place_loose(&mut item, &mut transform, &mut visibility, settle_position);
                        continue;
                    }
                }

                item.state = ItemState::Thrown {
                    owner,
                    owner_id,
                    lifetime,
                    grace,
                };
            }
            ItemState::Rolling { mut lifetime } => {
                lifetime -= dt;
                item.pickup_lockout = (item.pickup_lockout - dt).max(0.0);
                item.velocity.y -= GRAVITY * dt;
                transform.translation += item.velocity * dt;
                transform.rotate_x(dt * 8.0);
                transform.rotate_z(dt * 5.0);

                if should_respawn_item(transform.translation) {
                    item.set_respawning();
                    *visibility = Visibility::Hidden;
                    continue;
                }

                if let Some(ground_y) =
                    ground_height_at(transform.translation.x, transform.translation.z)
                {
                    if transform.translation.y <= ground_y + item.kind.loose_offset()
                        && item.velocity.y <= 0.0
                    {
                        transform.translation.y = ground_y + item.kind.loose_offset();
                        item.velocity.x *= 0.76;
                        item.velocity.z *= 0.76;
                        item.velocity.y = 0.0;
                    }
                }

                let rolling_speed = Vec2::new(item.velocity.x, item.velocity.z).length();
                if lifetime <= 0.0 || rolling_speed <= 0.18 {
                    let settle_position = transform.translation;
                    place_loose(&mut item, &mut transform, &mut visibility, settle_position);
                    continue;
                }

                item.state = ItemState::Rolling { lifetime };
            }
            ItemState::Spraying {
                owner,
                owner_id,
                mut lifetime,
                mut spray_timer,
                mut spiral_phase,
                mut spiral_radius,
            } => {
                lifetime -= dt;
                let (spray_due, next_spray_timer) = advance_barrel_spray_timer(spray_timer, dt);
                spray_timer = next_spray_timer;
                spiral_phase += dt * (5.0 + spiral_radius * 1.8);
                spiral_radius = (spiral_radius - dt * 0.9).max(0.16);

                let planar = Vec2::new(item.velocity.x, item.velocity.z);
                let speed = planar.length() * (-2.2 * dt).exp();
                let direction = if planar.length_squared() > 0.0001 {
                    planar / planar.length()
                } else {
                    Vec2::new(spiral_phase.cos(), spiral_phase.sin())
                };
                let turn = dt * (4.0 + spiral_radius * 3.0);
                let (sin_turn, cos_turn) = turn.sin_cos();
                let turned = Vec2::new(
                    direction.x * cos_turn - direction.y * sin_turn,
                    direction.x * sin_turn + direction.y * cos_turn,
                );
                item.velocity = Vec3::new(turned.x * speed, 0.0, turned.y * speed);
                transform.translation += item.velocity * dt;
                transform.rotate_y(dt * 24.0);
                transform.rotate_x((spiral_phase * 2.4).sin() * dt * 0.7);
                transform.rotate_z((spiral_phase * 1.7).cos() * dt * 0.55);

                if should_respawn_item(transform.translation) {
                    item.set_respawning();
                    material.0 = assets.barrel_material.clone();
                    *visibility = Visibility::Hidden;
                    continue;
                }

                if spray_due {
                    spawn_alcohol_spray(
                        &mut commands,
                        &effect_assets,
                        transform.translation,
                        spiral_phase,
                    );
                    for (
                        target_entity,
                        fighter,
                        mut stats,
                        _motor,
                        action,
                        mut drunk,
                        _target_style,
                        _target_equipment,
                        fighter_transform,
                    ) in &mut fighters
                    {
                        if target_entity == owner
                            || !state.combat_target_allowed_for_state(owner_id, fighter.id)
                            || !can_receive_impact(&stats, &action)
                        {
                            continue;
                        }
                        let delta = fighter_transform.translation - transform.translation;
                        let flat_distance = Vec2::new(delta.x, delta.z).length();
                        if flat_distance > BARREL_SPRAY_RADIUS {
                            continue;
                        }
                        drunk.refresh();
                        stats.hud_flash = stats.hud_flash.max(0.12);
                    }
                }

                if lifetime <= 0.0 {
                    if item.durability <= 0 {
                        item.set_respawning();
                        material.0 = assets.barrel_material.clone();
                        *visibility = Visibility::Hidden;
                    } else {
                        let settle_position = transform.translation;
                        place_loose(&mut item, &mut transform, &mut visibility, settle_position);
                    }
                    continue;
                }

                item.state = ItemState::Spraying {
                    owner,
                    owner_id,
                    lifetime,
                    spray_timer,
                    spiral_phase,
                    spiral_radius,
                };
            }
            ItemState::Armed {
                owner,
                owner_id,
                mut timer,
                mut grace,
            } => {
                timer -= dt;
                grace = (grace - dt).max(0.0);
                item.velocity.y -= GRAVITY * dt;
                transform.translation += item.velocity * dt;
                transform.scale =
                    item_scale(item.kind) * (1.0 + (time.elapsed_secs() * 28.0).sin().abs() * 0.18);
                transform.rotate_y(dt * 5.0);
                if let Some(ground_y) =
                    ground_height_at(transform.translation.x, transform.translation.z)
                {
                    if transform.translation.y <= ground_y + item.kind.loose_offset()
                        && item.velocity.y <= 0.0
                    {
                        transform.translation.y = ground_y + item.kind.loose_offset();
                        item.velocity.x *= 0.82;
                        item.velocity.z *= 0.82;
                        item.velocity.y = 0.0;
                    }
                }

                if should_respawn_item(transform.translation) {
                    item.set_respawning();
                    material.0 = assets.steamer_material.clone();
                    *visibility = Visibility::Hidden;
                    continue;
                }

                if timer > 0.0 {
                    item.state = ItemState::Armed {
                        owner,
                        owner_id,
                        timer,
                        grace,
                    };
                    continue;
                }

                let origin = transform.translation;
                spawn_pop_bomb_blast(&mut commands, &effect_assets, origin);
                camera_effects.push_combat_sfx(steamer_explosion_sfx_cue(origin));
                let blast_feedback = impact_feedback_profile(
                    ImpactSource::ItemBlast,
                    ImpactFeedbackIntensity::Heavy,
                );
                hitstop.trigger(blast_feedback.hitstop);
                camera_effects.shake = camera_effects.shake.max(blast_feedback.shake);

                for (
                    target_entity,
                    fighter,
                    mut stats,
                    mut motor,
                    mut action,
                    mut _drunk,
                    target_style,
                    target_equipment,
                    fighter_transform,
                ) in &mut fighters
                {
                    if target_entity == owner && grace > 0.0 {
                        continue;
                    }
                    if !state.combat_target_allowed_for_state(owner_id, fighter.id) {
                        continue;
                    }
                    if !can_receive_impact(&stats, &action) {
                        continue;
                    }
                    let hurt_center = fighter_transform.translation + Vec3::Y * 0.82;
                    let delta = hurt_center - origin;
                    let flat_distance = Vec2::new(delta.x, delta.z).length();
                    let fighter_radius = FIGHTER_RADIUS * stats.item_size_multiplier();
                    if !pop_bomb_body_overlaps(flat_distance, fighter_radius) {
                        continue;
                    }
                    let blast_distance = pop_bomb_overlap_distance(flat_distance, fighter_radius);

                    let falloff = radial_falloff(blast_distance, POP_BOMB_RADIUS);
                    let mut blast_profile = impact_profile_from_payload_with_feel(
                        owner_id,
                        ImpactSource::ItemBlast,
                        AttackPayloadId::BombBlast,
                        falloff.max(0.45),
                        falloff.max(0.55),
                        1.0,
                        28.0,
                        &feel,
                    );
                    let radial = Vec3::new(delta.x, 0.0, delta.z).normalize_or_zero();
                    blast_profile.knockback_direction = Some(if radial.length_squared() > 0.01 {
                        radial
                    } else {
                        Vec3::Z
                    });
                    let proximity = 1.0 - (blast_distance / POP_BOMB_RADIUS).clamp(0.0, 1.0);
                    let arc_planar_speed = STEAMER_BLAST_ARC_MIN_PLANAR_SPEED
                        .lerp(STEAMER_BLAST_ARC_MAX_PLANAR_SPEED, proximity);
                    let arc_vertical_speed = STEAMER_BLAST_ARC_MIN_VERTICAL_SPEED
                        .lerp(STEAMER_BLAST_ARC_MAX_VERTICAL_SPEED, proximity);
                    blast_profile.knockback =
                        arc_planar_speed / blast_profile.reaction.horizontal_scale.max(0.01);
                    blast_profile.vertical_knockback =
                        if blast_profile.reaction.vertical_scale > 0.01 {
                            arc_vertical_speed / blast_profile.reaction.vertical_scale
                        } else {
                            arc_vertical_speed
                        };
                    apply_impact(
                        &mut commands,
                        &effect_assets,
                        &mut camera_effects,
                        &mut hitstop,
                        &state,
                        &mut stats,
                        &mut motor,
                        &mut action,
                        fighter_transform,
                        None,
                        origin,
                        blast_profile,
                        DamageDefenderProfile::from_loadout(target_style, target_equipment),
                        &mut telemetry,
                    );
                    let launched_planar_speed =
                        Vec2::new(motor.velocity.x, motor.velocity.z).length();
                    motor.impact_speed_limit_timer = motor
                        .impact_speed_limit_timer
                        .max(STEAMER_BLAST_ARC_SPEED_LIMIT_TIME);
                    motor.impact_speed_limit = motor.impact_speed_limit.max(launched_planar_speed);
                    if fighter.id == owner_id {
                        stats.stamina = (stats.stamina - 8.0).max(0.0);
                    }
                }

                item.set_respawning();
                material.0 = assets.steamer_material.clone();
                *visibility = Visibility::Hidden;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
fn item_throw_profile(kind: ItemKind, owner_id: usize) -> ImpactProfile {
    item_throw_profile_from_payload(kind, owner_id, None)
}

fn item_throw_profile_with_feel(
    kind: ItemKind,
    owner_id: usize,
    feel: &CombatFeelTuning,
) -> ImpactProfile {
    item_throw_profile_from_payload(kind, owner_id, Some(feel))
}

fn item_throw_profile_from_payload(
    kind: ItemKind,
    owner_id: usize,
    feel: Option<&CombatFeelTuning>,
) -> ImpactProfile {
    let (payload_id, damage_scale, knockback_scale, vertical_scale) = match kind {
        ItemKind::Apple | ItemKind::WineWhite | ItemKind::CupCoffee | ItemKind::Mushroom => {
            (AttackPayloadId::ItemThrowLight, 0.55, 0.72, 0.86)
        }
        ItemKind::Turkey | ItemKind::Barrel | ItemKind::Crate => {
            (AttackPayloadId::ItemThrowHeavy, 1.05, 0.98, 0.96)
        }
        ItemKind::Steamer => (AttackPayloadId::ItemThrowHeavy, 1.0, 1.0, 1.0),
    };

    if let Some(feel) = feel {
        return impact_profile_from_payload_with_feel(
            owner_id,
            ImpactSource::ItemThrow,
            payload_id,
            damage_scale,
            knockback_scale,
            vertical_scale,
            24.0,
            feel,
        );
    }

    impact_profile_from_payload(
        owner_id,
        ImpactSource::ItemThrow,
        payload_id,
        damage_scale,
        knockback_scale,
        vertical_scale,
        24.0,
    )
}

fn should_respawn_item(position: Vec3) -> bool {
    let arena = active_arena_definition();
    position.y < arena.ringout_y
        || Vec2::new(position.x, position.z).length() > arena.ringout_radius
}

pub fn item_scale(kind: ItemKind) -> Vec3 {
    match kind {
        ItemKind::Crate => Vec3::splat(1.7),
        ItemKind::Steamer => Vec3::splat(0.72 * 2.0),
        ItemKind::Apple => Vec3::splat(0.82 * 3.0),
        ItemKind::WineWhite => Vec3::splat(0.78 * 2.0),
        ItemKind::Turkey => Vec3::splat(0.85 * 2.0),
        ItemKind::Barrel => Vec3::splat(0.72 * 2.0),
        ItemKind::CupCoffee => Vec3::splat(0.7 * 3.0),
        ItemKind::Mushroom => Vec3::splat(0.78 * 4.5),
    }
}

pub fn sync_item_visuals(
    assets: Res<ItemAssets>,
    fighters: Query<
        (
            Entity,
            &FighterInventory,
            &FighterMotor,
            &FighterActionState,
            &Transform,
        ),
        Without<ArenaItem>,
    >,
    mut items: Query<
        (
            &ArenaItem,
            &mut Transform,
            &mut Visibility,
            &mut MeshMaterial3d<StandardMaterial>,
        ),
        Without<Fighter>,
    >,
) {
    for (fighter_entity, inventory, motor, action, fighter_transform) in &fighters {
        let Some(item_entity) = inventory.held else {
            continue;
        };
        let Ok((item, mut item_transform, mut visibility, mut material)) =
            items.get_mut(item_entity)
        else {
            continue;
        };
        if !matches!(item.state, ItemState::Held { holder } if holder == fighter_entity) {
            continue;
        }

        let facing = motor.facing.normalize_or_zero();
        let right = Vec3::new(facing.z, 0.0, -facing.x).normalize_or_zero();
        let swing_forward = if action.action == FighterAction::ItemSwing {
            0.32
        } else {
            0.0
        };
        item_transform.translation = fighter_transform.translation
            + Vec3::Y * 0.88
            + right * 0.42
            + facing * (0.36 + swing_forward);
        let yaw = facing.x.atan2(facing.z);
        item_transform.rotation = match item.kind {
            ItemKind::WineWhite | ItemKind::CupCoffee => Quat::from_rotation_y(yaw),
            _ => Quat::from_rotation_y(yaw),
        };
        item_transform.scale = match item.kind {
            ItemKind::Steamer => {
                let pulse = if action.action == FighterAction::ItemThrow {
                    1.12
                } else {
                    1.0
                };
                item_scale(item.kind) * pulse
            }
            ItemKind::Crate
            | ItemKind::Apple
            | ItemKind::WineWhite
            | ItemKind::Turkey
            | ItemKind::Barrel
            | ItemKind::CupCoffee
            | ItemKind::Mushroom => item_scale(item.kind),
        };
        material.0 = assets.material_for(item.kind, false);
        *visibility = Visibility::Visible;
    }
}

fn open_mystery_crate(
    commands: &mut Commands,
    assets: &ItemAssets,
    crate_item: &mut ArenaItem,
    visibility: &mut Visibility,
    position: Vec3,
    randomizer: f32,
) {
    let reward = mystery_crate_reward(position, randomizer);
    let ground_y = ground_height_at(position.x, position.z).unwrap_or(ARENA_TOP_Y);
    let reward_position = Vec3::new(position.x, ground_y + reward.loose_offset(), position.z);
    let reward_entity = spawn_pickup(commands, assets, reward, reward_position, randomizer);
    commands
        .entity(reward_entity)
        .insert(crate::arena::ArenaGeometry);
    crate_item.set_respawning();
    *visibility = Visibility::Hidden;
}

fn mystery_crate_reward(position: Vec3, randomizer: f32) -> ItemKind {
    const REWARDS: [ItemKind; 7] = [
        ItemKind::Steamer,
        ItemKind::Apple,
        ItemKind::WineWhite,
        ItemKind::Turkey,
        ItemKind::Barrel,
        ItemKind::CupCoffee,
        ItemKind::Mushroom,
    ];
    let noise = (position.x * 12.9898 + position.z * 78.233 + randomizer * 37.719)
        .sin()
        .abs();
    REWARDS[((noise * 10_000.0) as usize) % REWARDS.len()]
}

#[allow(dead_code)]
pub fn held_item_label(inventory: &FighterInventory, items: &Query<&ArenaItem>) -> Option<String> {
    let item_entity = inventory.held?;
    let item = items.get(item_entity).ok()?;
    Some(item.status_label())
}

fn held_reference_is_stale(item: Option<&ArenaItem>, fighter_entity: Entity) -> bool {
    match item {
        Some(item) => !item.is_held_by(fighter_entity),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(index: u32) -> Entity {
        Entity::from_raw_u32(index).expect("test entity index should be valid")
    }

    #[test]
    fn item_state_transitions_reset_transient_state() {
        let holder = entity(1);
        let owner = entity(2);
        let mut item = ArenaItem::new(ItemKind::Apple, Vec3::new(1.0, 0.5, 0.0), 0.0);
        item.velocity = Vec3::new(9.0, 0.0, 0.0);
        item.already_hit.push(entity(7));
        item.pickup_lockout = 0.4;

        item.pickup_as(holder);
        assert_eq!(item.state, ItemState::Held { holder });
        assert_eq!(item.velocity, Vec3::ZERO);
        assert!(item.already_hit.is_empty());
        assert_eq!(item.pickup_lockout, 0.0);

        item.already_hit.push(entity(8));
        item.launch_as_thrown(owner, 4, Vec3::new(3.0, 1.0, 0.0));
        assert_eq!(item.velocity, Vec3::new(3.0, 1.0, 0.0));
        assert!(item.already_hit.is_empty());
        assert_eq!(item.pickup_lockout, item.kind.pickup_lockout());
        assert_eq!(
            item.state,
            ItemState::Thrown {
                owner,
                owner_id: 4,
                lifetime: ITEM_THROW_LIFETIME,
                grace: ITEM_MALLET_THROW_GRACE,
            }
        );
    }

    #[test]
    fn barrel_activation_consumes_once_and_enters_four_second_spray() {
        let owner = entity(2);
        let mut item = ArenaItem::new(ItemKind::Barrel, Vec3::ZERO, 0.0);
        item.velocity = Vec3::new(4.0, -1.0, 1.0);
        item.durability -= 1;
        item.start_barrel_spray(owner, 3);

        assert_eq!(item.durability, 2);
        assert!(matches!(
            item.state,
            ItemState::Spraying {
                owner: active_owner,
                owner_id: 3,
                lifetime,
                spray_timer: 0.0,
                ..
            } if active_owner == owner && (lifetime - BARREL_SPRAY_DURATION).abs() < 0.001
        ));
        assert_eq!(item.velocity.y, 0.0);
    }

    #[test]
    fn barrel_spray_cadence_fires_immediately_then_every_quarter_second() {
        let (first, timer) = advance_barrel_spray_timer(0.0, 1.0 / 60.0);
        assert!(first);
        assert!((timer - (BARREL_SPRAY_CADENCE - 1.0 / 60.0)).abs() < 0.001);

        let (second, timer) = advance_barrel_spray_timer(timer, 0.1);
        assert!(!second);
        let (third, _) = advance_barrel_spray_timer(timer, 0.2);
        assert!(third);
    }

    #[test]
    fn armed_bomb_uses_bomb_tuning() {
        let owner = entity(3);
        let mut item = ArenaItem::new(ItemKind::Steamer, Vec3::ZERO, 0.0);

        item.arm_as_bomb(owner, 5, Vec3::new(1.0, 2.0, 0.0));

        assert_eq!(item.velocity, Vec3::new(1.0, 2.0, 0.0));
        assert_eq!(item.pickup_lockout, ITEM_BOMB_PICKUP_LOCKOUT);
        assert_eq!(
            item.state,
            ItemState::Armed {
                owner,
                owner_id: 5,
                timer: POP_BOMB_FUSE,
                grace: ITEM_BOMB_THROW_GRACE,
            }
        );
    }

    #[test]
    fn stale_held_reference_detects_missing_or_wrong_holder() {
        let holder = entity(11);
        let other = entity(12);
        let mut item = ArenaItem::new(ItemKind::Apple, Vec3::ZERO, 0.0);

        assert!(held_reference_is_stale(None, holder));
        assert!(held_reference_is_stale(Some(&item), holder));

        item.pickup_as(holder);
        assert!(!held_reference_is_stale(Some(&item), holder));
        assert!(held_reference_is_stale(Some(&item), other));
    }

    #[test]
    fn expanded_items_have_distinct_roles() {
        assert_eq!(ItemKind::Steamer.role(), ItemRole::Explosive);
        assert_eq!(ItemKind::Crate.role(), ItemRole::Utility);
        assert_eq!(ItemKind::CupCoffee.role(), ItemRole::Utility);
        assert_eq!(ItemKind::Apple.role(), ItemRole::Recovery);
        assert_eq!(ItemKind::Mushroom.role(), ItemRole::Utility);
        assert!(item_swing_config(ItemKind::Apple).is_none());
        let barrel_throw = item_throw_profile(ItemKind::Barrel, 0);
        assert!(barrel_throw.knockback > 0.0);
        assert_eq!(
            barrel_throw.payload_id,
            Some(AttackPayloadId::ItemThrowHeavy)
        );
        assert_eq!(barrel_throw.shape_id, Some(AttackShapeId::ItemLob));
        assert_eq!(
            ArenaItem::new(ItemKind::Turkey, Vec3::ZERO, 0.0).max_durability,
            3
        );
        assert_eq!(
            ArenaItem::new(ItemKind::Barrel, Vec3::ZERO, 0.0).max_durability,
            3
        );
        assert_ne!(mystery_crate_reward(Vec3::ZERO, 1.0), ItemKind::Crate);
    }

    #[test]
    fn white_wine_restores_one_ultimate_cost_and_caps_mp() {
        assert_eq!(
            ITEM_WINE_WHITE_STAMINA,
            crate::constants::ULTIMATE_STAMINA_COST
        );

        let near_full_stamina = MAX_STAMINA - 1.0;
        assert_eq!(
            (near_full_stamina + ITEM_WINE_WHITE_STAMINA).min(MAX_STAMINA),
            MAX_STAMINA
        );
    }

    #[test]
    fn item_role_priorities_keep_recovery_and_utility_distinct() {
        assert!(ItemKind::Steamer.bot_pickup_priority() > ItemKind::Apple.bot_pickup_priority());
        assert!(ItemKind::CupCoffee.bot_pickup_priority() > ItemKind::Apple.bot_pickup_priority());
        assert_eq!(ItemKind::Turkey.max_durability(), 3);
    }

    #[test]
    fn forced_item_drop_sfx_only_targets_visible_combat_drops() {
        assert!(forced_item_drop_action(FighterAction::Knockdown));
        assert!(forced_item_drop_action(FighterAction::Grabbed));
        assert!(forced_item_drop_action(FighterAction::GuardBroken));
        assert!(forced_item_drop_action(FighterAction::RingOut));
        assert!(forced_item_drop_action(FighterAction::Respawning));
        assert!(visible_forced_item_drop_action(FighterAction::Knockdown));
        assert!(!visible_forced_item_drop_action(FighterAction::RingOut));
        assert!(!forced_item_drop_action(FighterAction::Idle));

        assert_eq!(
            item_drop_sfx_cue(Vec3::X),
            CombatSfxCue::new(CombatSfxKind::ItemDrop, Vec3::X, ITEM_DROP_SFX_PRIORITY)
        );
    }

    #[test]
    fn item_specific_sfx_helpers_route_mushroom_and_steamer() {
        assert_eq!(
            item_use_sfx_cue(ItemKind::Mushroom, Vec3::Y),
            Some(CombatSfxCue::new(
                CombatSfxKind::MushroomBigger,
                Vec3::Y,
                MUSHROOM_BIGGER_SFX_PRIORITY,
            ))
        );
        assert_eq!(item_use_sfx_cue(ItemKind::Apple, Vec3::Y), None);
        assert_eq!(
            steamer_explosion_sfx_cue(Vec3::Z),
            CombatSfxCue::new(
                CombatSfxKind::SteamerExplosion,
                Vec3::Z,
                STEAMER_EXPLOSION_SFX_PRIORITY,
            )
        );
    }

    #[test]
    fn portable_pickup_is_blocked_by_active_dash_and_dash_slide() {
        let idle_motor = FighterMotor::default();
        assert!(!portable_pickup_blocked(FighterAction::Idle, &idle_motor));
        assert!(portable_pickup_blocked(FighterAction::Dashing, &idle_motor));

        let sliding_motor = FighterMotor {
            dash_slide_timer: 0.1,
            ..default()
        };
        assert!(portable_pickup_blocked(FighterAction::Idle, &sliding_motor));
    }

    #[test]
    fn held_item_inputs_are_sanitized_to_item_only_commands() {
        let mut input = FighterInput {
            movement: Vec2::new(0.5, -0.25),
            aim: true,
            jump: true,
            dash: true,
            light: true,
            light_held: true,
            heavy: false,
            heavy_held: true,
            heavy_released: true,
            grab: true,
            guard: true,
            ultimate: true,
            special: true,
            ..default()
        };

        sanitize_held_item_inputs(&mut input);

        assert_eq!(input.movement, Vec2::new(0.5, -0.25));
        assert!(input.aim);
        assert!(input.jump);
        assert!(input.dash);
        assert!(!input.grab);
        assert!(input.guard);
        assert!(!input.ultimate);
        assert!(!input.special);
        assert!(!input.light);
        assert!(!input.light_held);
        assert!(!input.heavy_held);
        assert!(!input.heavy_released);
        assert!(!input.heavy);
    }

    #[test]
    fn held_item_command_routes_pop_bomb_inputs_to_throw() {
        assert_eq!(
            held_item_command(
                &FighterInput {
                    light: true,
                    ..default()
                },
                ItemKind::Steamer
            ),
            HeldItemCommand::Throw
        );

        assert_eq!(
            held_item_command(
                &FighterInput {
                    heavy: true,
                    ..default()
                },
                ItemKind::Steamer
            ),
            HeldItemCommand::Throw
        );

        assert_eq!(
            held_item_command(
                &FighterInput {
                    light: true,
                    heavy: true,
                    ..default()
                },
                ItemKind::Apple
            ),
            HeldItemCommand::Throw
        );
    }

    #[test]
    fn guard_and_grab_are_not_used_to_drop_item_or_block_guard() {
        let mut input = FighterInput {
            guard: true,
            grab: true,
            ..default()
        };

        assert_eq!(
            held_item_command(&input, ItemKind::Apple),
            HeldItemCommand::None
        );
        assert_eq!(
            held_item_command(&input, ItemKind::Steamer),
            HeldItemCommand::None
        );

        sanitize_held_item_inputs(&mut input);
        assert!(input.guard);
        assert!(!input.grab);
        assert!(!input.special);
        assert!(!input.ultimate);
    }

    #[test]
    fn held_item_inputs_prevent_skill_routing_controls() {
        let mut input = FighterInput {
            movement: Vec2::new(-0.75, 0.2),
            aim: true,
            jump: true,
            dash: true,
            special: true,
            ultimate: true,
            light: true,
            ..default()
        };

        let command = held_item_command(&input, ItemKind::Apple);
        sanitize_held_item_inputs(&mut input);

        assert_eq!(command, HeldItemCommand::Use);
        assert_eq!(input.movement, Vec2::new(-0.75, 0.2));
        assert!(input.aim);
        assert!(input.jump);
        assert!(input.dash);
        assert!(!input.guard);
        assert!(!input.grab);
        assert!(!input.special);
        assert!(!input.ultimate);
        assert!(!input.light);
    }
}
