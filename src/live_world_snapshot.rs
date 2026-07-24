//! Production composition root for canonical snapshots of the live game world.
//!
//! Authority, prediction, rollback, replay, and reconnect code must all use this
//! adapter rather than assembling a subset of the live codecs independently.
//! Keeping the registry construction in one place makes an omitted stable entity
//! kind a startup/test failure instead of a late restore failure.

use bevy::prelude::World;

use crate::determinism::SimEntityKind;
use crate::live_character_skill_snapshot::{
    LiveBeeSkillSnapshotCodec, LiveChickSkillSnapshotCodec,
};
use crate::live_dynamic_snapshot::{LiveArenaOrdnanceSnapshotCodec, LiveItemSnapshotCodec};
use crate::live_hitbox_snapshot::LiveHitboxSnapshotCodec;
use crate::live_match_snapshot::LiveMatchSnapshotCodec;
use crate::live_penguin_snapshot::{
    LivePenguinSkillSnapshotCodec, LivePenguinSurfaceSnapshotCodec,
};
use crate::live_snapshot::LiveFighterSnapshotCodec;
use crate::live_special_snapshot::LiveSpecialSnapshotCodec;
use crate::snapshot::{CanonicalSnapshot, DynamicObjectSnapshot, SnapshotError};
use crate::snapshot_ecs::{
    DynamicSnapshotCodecRegistry, EcsSnapshotAdapter, EcsSnapshotError, EcsSnapshotRestoreReport,
};

/// Builds the complete, closed codec registry for the current live simulation.
///
/// The stable kind catalog is itself part of the network/replay contract. This
/// function intentionally names every mapping explicitly so adding a new kind
/// requires updating this composition root and its completeness test.
pub fn live_dynamic_snapshot_registry() -> DynamicSnapshotCodecRegistry {
    let mut registry = DynamicSnapshotCodecRegistry::new();
    registry
        .register(SimEntityKind::Hitbox, LiveHitboxSnapshotCodec)
        .expect("the live hitbox codec is registered exactly once");
    registry
        .register(SimEntityKind::Item, LiveItemSnapshotCodec)
        .expect("the live item codec is registered exactly once");
    registry
        .register(SimEntityKind::Special, LiveSpecialSnapshotCodec)
        .expect("the live special codec is registered exactly once");
    registry
        .register(SimEntityKind::BeeSkill, LiveBeeSkillSnapshotCodec)
        .expect("the live Bee-skill codec is registered exactly once");
    registry
        .register(SimEntityKind::ChickSkill, LiveChickSkillSnapshotCodec)
        .expect("the live Chick-skill codec is registered exactly once");
    registry
        .register(SimEntityKind::PenguinSkill, LivePenguinSkillSnapshotCodec)
        .expect("the live Penguin-skill codec is registered exactly once");
    registry
        .register(
            SimEntityKind::PenguinSurface,
            LivePenguinSurfaceSnapshotCodec,
        )
        .expect("the live Penguin-surface codec is registered exactly once");
    registry
        .register(SimEntityKind::ArenaOrdnance, LiveArenaOrdnanceSnapshotCodec)
        .expect("the live arena-ordnance codec is registered exactly once");
    registry
}

/// One production snapshot boundary shared by authority and predicted worlds.
///
/// The wrapper owns no simulation state. It maps the existing live ECS and
/// resources directly into the canonical schema, and delegates to the atomic
/// prepare/commit restore transaction in [`EcsSnapshotAdapter`].
pub struct LiveWorldSnapshotAdapter {
    ecs: EcsSnapshotAdapter,
}

impl Default for LiveWorldSnapshotAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveWorldSnapshotAdapter {
    pub fn new() -> Self {
        Self {
            ecs: EcsSnapshotAdapter::new(live_dynamic_snapshot_registry()),
        }
    }

    /// Captures one completed canonical live tick.
    pub fn capture(&self, world: &World) -> Result<CanonicalSnapshot, EcsSnapshotError> {
        self.capture_reusing(world, None)
    }

    /// Captures one completed tick while retaining storage from an expired
    /// bounded-history entry.
    pub fn capture_reusing(
        &self,
        world: &World,
        reusable: Option<CanonicalSnapshot>,
    ) -> Result<CanonicalSnapshot, EcsSnapshotError> {
        let snapshot = self.ecs.capture_with_non_fighter_reusing(
            world,
            &LiveMatchSnapshotCodec,
            &LiveFighterSnapshotCodec,
            reusable,
        )?;
        validate_live_item_provenance(&snapshot.dynamic_objects)?;
        Ok(snapshot)
    }

    /// Atomically restores a canonical tick into an existing live world.
    ///
    /// Every snapshot, match, fighter, dynamic-object, and allocator check
    /// completes before the adapter replaces authoritative state.
    pub fn restore(
        &self,
        world: &mut World,
        snapshot: &CanonicalSnapshot,
    ) -> Result<EcsSnapshotRestoreReport, EcsSnapshotError> {
        snapshot.validate()?;
        validate_live_item_provenance(&snapshot.dynamic_objects)?;
        self.ecs.restore_with_non_fighter(
            world,
            snapshot,
            &LiveMatchSnapshotCodec,
            &LiveFighterSnapshotCodec,
        )
    }

    pub fn dynamic_codecs(&self) -> &DynamicSnapshotCodecRegistry {
        self.ecs.dynamic_codecs()
    }
}

fn validate_live_item_provenance(objects: &[DynamicObjectSnapshot]) -> Result<(), SnapshotError> {
    for (index, reward) in objects.iter().enumerate() {
        if reward.id.kind() != SimEntityKind::Item {
            continue;
        }
        let Some(source_id) = reward.related_entity else {
            continue;
        };
        if reward.definition_id == 0 {
            return Err(SnapshotError::InvariantViolation(
                "crate reward provenance is attached to another crate",
            ));
        }
        let source = objects
            .binary_search_by_key(&source_id, |object| object.id)
            .ok()
            .and_then(|source_index| objects.get(source_index))
            .ok_or(SnapshotError::InvariantViolation(
                "crate reward provenance source is absent",
            ))?;
        if source.id.kind() != SimEntityKind::Item
            || source.definition_id != 0
            || source.related_entity.is_some()
        {
            return Err(SnapshotError::InvariantViolation(
                "crate reward provenance source is not an arena crate",
            ));
        }
        if objects[..index].iter().any(|prior| {
            prior.id.kind() == SimEntityKind::Item && prior.related_entity == Some(source_id)
        }) {
            return Err(SnapshotError::InvariantViolation(
                "multiple live crate rewards reference the same source",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::prelude::*;

    use crate::arena::{ArenaCannonBomb, ArenaHazardState, ArenaPipeState, PowderKegCannonState};
    use crate::arena_defs::ActiveArena;
    use crate::bee_skills::{ActiveBeeSkill, BeeSkillKind};
    use crate::characters::{CharacterKind, FighterCharacter};
    use crate::chick_skills::{ActiveChickSkill, ChickSkillKind};
    use crate::combat::ImpactSource;
    use crate::components::{
        DrunkStatus, Fighter, FighterActionState, FighterGrabState, FighterInput, FighterInventory,
        FighterMotor, FighterSpecialState, FighterStats, FighterUltimateState, Hitbox, SimPosition,
    };
    use crate::determinism::{
        DEFAULT_F32_QUANTIZATION, FighterHitMask, FighterId, SimEntityId, canonicalize_f32,
    };
    use crate::ecs_identity::{
        SIM_ENTITY_POOL_CAPACITIES, SimulationIdentityAllocator, StableSimEntity,
    };
    use crate::effects::{FeedbackPackageId, HitImpactEffectId};
    use crate::equipment::{EquipmentKind, FighterEquipment};
    use crate::game_state::{Hitstop, MatchPhase, MatchState, MatchTelemetry};
    use crate::items::{ArenaItem, ItemKind};
    use crate::network_protocol::MAX_RESYNC_SNAPSHOT_BYTES;
    use crate::penguin_skills::{
        ActivePenguinSkill, ActivePenguinSurface, PenguinSkillKind, PenguinSurfaceKind,
    };
    use crate::simulation::{
        ElapsedTicks, SimTick, TickTimer, milliseconds_to_ticks_ceil, seconds_to_ticks_ceil,
    };
    use crate::snapshot_ecs::SnapshotContract;
    use crate::specials::{ActiveSpecial, SpecialKind};
    use crate::styles::{FighterStyle, FighterStyleKind};
    use crate::techniques::{
        AttackPayloadId, AttackShapeId, MsTimingWindow, TechniqueId, attack_payload_definition,
        attack_shape_definition,
    };

    const TEST_SEED: u64 = 0xAFC0_1200_3400_5600;

    fn q(value: f32) -> f32 {
        canonicalize_f32(value, DEFAULT_F32_QUANTIZATION)
    }

    fn qv(x: f32, y: f32, z: f32) -> Vec3 {
        Vec3::new(q(x), q(y), q(z))
    }

    fn recycle_first_slot(world: &mut World, kind: SimEntityKind) {
        let entity = world.spawn_empty().id();
        let stable = world
            .resource_mut::<SimulationIdentityAllocator>()
            .try_allocate(kind, entity)
            .unwrap();
        assert_eq!(stable.id().index(), 0);
        assert!(
            world
                .resource_mut::<SimulationIdentityAllocator>()
                .release(entity, stable)
        );
        assert!(world.despawn(entity));
    }

    fn spawn_stable<B: Bundle>(
        world: &mut World,
        kind: SimEntityKind,
        bundle: B,
    ) -> (Entity, SimEntityId) {
        let entity = world.spawn(bundle).id();
        let stable = world
            .resource_mut::<SimulationIdentityAllocator>()
            .try_allocate(kind, entity)
            .unwrap();
        world.entity_mut(entity).insert(stable);
        (entity, stable.id())
    }

    fn spawn_fighter_slots(world: &mut World) -> [Entity; 4] {
        std::array::from_fn(|index| {
            world
                .spawn((
                    Fighter {
                        id: index,
                        name: ["cat", "pig", "penguin", "chick"][index],
                        color: Color::WHITE,
                        spawn: qv(index as f32 - 2.0, 0.5, 1.0),
                    },
                    SimPosition::new(qv(index as f32, 1.0, -2.0)),
                    FighterInput::default(),
                    FighterStats::default(),
                    FighterMotor::default(),
                    FighterActionState::default(),
                    DrunkStatus::default(),
                    FighterInventory::default(),
                    FighterGrabState::default(),
                    FighterUltimateState::default(),
                    FighterSpecialState::default(),
                    FighterCharacter::new(
                        [
                            CharacterKind::Cat,
                            CharacterKind::Pig,
                            CharacterKind::Penguin,
                            CharacterKind::Chick,
                        ][index],
                    ),
                    FighterStyle {
                        kind: [
                            FighterStyleKind::Anchor,
                            FighterStyleKind::Vector,
                            FighterStyleKind::Catalyst,
                            FighterStyleKind::Vector,
                        ][index],
                    },
                    FighterEquipment::new(
                        [
                            EquipmentKind::DashCoil,
                            EquipmentKind::AerialSpur,
                            EquipmentKind::CounterCell,
                            EquipmentKind::HeavySeal,
                        ][index],
                    ),
                ))
                .id()
        })
    }

    fn fixture_hitbox() -> Hitbox {
        let payload = attack_payload_definition(AttackPayloadId::AsBeat1);
        let shape = attack_shape_definition(payload.shape_id);
        let total_lifetime = milliseconds_to_ticks_ceil(payload.time_ms);
        let elapsed = 2;
        Hitbox {
            owner: FighterId::ZERO,
            kind: payload.kind,
            payload_id: Some(AttackPayloadId::AsBeat1),
            attacker_character: Some(CharacterKind::Cat),
            technique_id: Some(TechniqueId::CatLight1),
            hit_effect: Some(HitImpactEffectId::GenericLight),
            shape_id: payload.shape_id,
            reaction_family: payload.reaction_family,
            damage_profile: payload.damage_profile,
            element: payload.element,
            attacker_equipment: Some(EquipmentKind::DashCoil),
            attacker_style: Some(FighterStyleKind::Vector),
            power: q(payload.power),
            str_scale: q(payload.str_scale),
            damage: q(payload.damage * q(1.125)),
            knockback: q(payload.knockback * q(1.25)),
            vertical_knockback: q(payload.vertical_knockback),
            guardable: payload.guardable,
            base_radius: q(shape.radius),
            radius: q(shape.radius * q(1.25)),
            lifetime: TickTimer::from_ticks(total_lifetime - elapsed),
            elapsed: ElapsedTicks::from_ticks(elapsed),
            total_lifetime,
            spawn_origin: qv(1.25, 2.5, -3.75),
            facing: Vec3::Z,
            base_range: q(shape.range),
            range: q(shape.range * q(1.25)),
            scales_with_owner_size: true,
            vertical_offset_scale: q(shape.vertical_offset_scale),
            parented: shape.parented,
            path: shape.path,
            expires_on_owner_landing: false,
            landing_linger: TickTimer::ZERO,
            landing_linger_started: false,
            ground_path_end: false,
            ground_path_clearance: 0.0,
            impact_cue: payload.impact_cue,
            hitstop_scale: q(payload.hitstop_scale),
            shake_scale: q(payload.shake_scale),
            feedback_priority_bonus: payload.feedback_priority_bonus,
            already_hit: FighterHitMask::from_bits(0b0010).unwrap(),
        }
    }

    fn fixture_special() -> ActiveSpecial {
        let age_ticks = 2;
        let total_lifetime_ms = 1_500;
        ActiveSpecial {
            kind: SpecialKind::Projectile,
            owner: FighterId::new(2).unwrap(),
            owner_style: FighterStyleKind::Catalyst,
            payload_id: AttackPayloadId::SpecialProjectile,
            shape_id: AttackShapeId::ProjectileBolt,
            source: ImpactSource::Projectile,
            facing: qv(1.0, 0.0, -0.25),
            velocity: qv(3.5, 0.25, -1.0),
            lifetime: TickTimer::from_ticks(
                milliseconds_to_ticks_ceil(total_lifetime_ms) - age_ticks,
            ),
            age: ElapsedTicks::from_ticks(age_ticks),
            total_lifetime_ms,
            radius: q(0.75),
            grace: TickTimer::from_ticks(3),
            launch_ms: 90,
            active_window: MsTimingWindow::closed(120, 980),
            repeat_ms: None,
            next_repeat_ms: None,
            active_feedback_sent: false,
            aftermath_ms: Some(1_120),
            aftermath_feedback_sent: false,
            active_cue: "release_special_projectile",
            aftermath_cue: "recover_special_projectile",
            active_package: FeedbackPackageId::SpecialProjectileRelease,
            repeat_package: None,
            impact_package: FeedbackPackageId::SpecialProjectileImpact,
            aftermath_package: FeedbackPackageId::SpecialProjectileRecover,
            despawn_package: FeedbackPackageId::SpecialProjectileRecover,
            stamina_disrupt: q(12.0),
            guard_stamina_damage: q(18.0),
            already_hit: FighterHitMask::from_bits(0b0010).unwrap(),
        }
    }

    fn fixture_bee_skill() -> ActiveBeeSkill {
        let age = 3;
        ActiveBeeSkill {
            kind: BeeSkillKind::HoneyGlob,
            owner: FighterId::ZERO,
            owner_style: FighterStyleKind::Vector,
            payload_id: AttackPayloadId::BeeHoneyGlob,
            shape_id: AttackShapeId::ProjectileBolt,
            source: ImpactSource::Projectile,
            facing: qv(0.75, 0.0, 0.5),
            velocity: qv(4.25, -0.5, 1.75),
            target: None,
            lifetime: TickTimer::from_ticks(seconds_to_ticks_ceil(1.15) - age),
            age: ElapsedTicks::from_ticks(age),
            radius: q(0.42),
            guard_stamina_damage: q(12.0),
            repeat_interval: None,
            repeat_timer: None,
            already_hit: FighterHitMask::from_bits(0b0100).unwrap(),
            size_scale: q(1.0),
        }
    }

    fn fixture_chick_skill() -> ActiveChickSkill {
        let age = 3;
        ActiveChickSkill {
            kind: ChickSkillKind::ShellChip,
            owner: FighterId::ZERO,
            owner_style: FighterStyleKind::Catalyst,
            payload_id: Some(AttackPayloadId::ChickShellChip),
            shape_id: AttackShapeId::ProjectileBolt,
            source: ImpactSource::Projectile,
            facing: qv(-0.5, 0.0, 0.75),
            velocity: qv(2.0, 3.25, -0.5),
            lifetime: TickTimer::from_ticks(seconds_to_ticks_ceil(0.62) - age),
            age: ElapsedTicks::from_ticks(age),
            radius: q(0.25),
            guard_stamina_damage: q(5.0),
            repeat_interval: None,
            repeat_timer: None,
            already_hit: FighterHitMask::from_bits(0b0100).unwrap(),
            size_scale: q(1.0),
        }
    }

    fn fixture_penguin_skill() -> ActivePenguinSkill {
        let age = 3;
        ActivePenguinSkill {
            kind: PenguinSkillKind::FishTorpedo,
            owner: FighterId::new(2).unwrap(),
            owner_style: FighterStyleKind::Catalyst,
            payload_id: AttackPayloadId::PenguinFishTorpedo,
            shape_id: AttackShapeId::ProjectileBolt,
            source: ImpactSource::Projectile,
            facing: Vec3::Z,
            velocity: qv(3.25, -0.5, 1.75),
            target: Some(FighterId::ZERO),
            lifetime: TickTimer::from_ticks(TickTimer::from_seconds_ceil(0.86).remaining() - age),
            age: ElapsedTicks::from_ticks(age),
            radius: q(0.38),
            guard_stamina_damage: q(8.0),
            repeat_interval: None,
            repeat_timer: None,
            already_hit: FighterHitMask::from_bits(0b0010).unwrap(),
            size_scale: q(1.0),
        }
    }

    fn fixture_penguin_surface() -> ActivePenguinSurface {
        let age = 4;
        ActivePenguinSurface {
            kind: PenguinSurfaceKind::SnowHillRamp,
            owner: FighterId::new(2).unwrap(),
            facing: Vec3::Z,
            lifetime: TickTimer::from_ticks(TickTimer::from_seconds_ceil(6.5).remaining() - age),
            age: ElapsedTicks::from_ticks(age),
            radius: q(1.08),
            next_tick: TickTimer::ZERO,
            already_touched: FighterHitMask::from_bits(0b0010).unwrap(),
            size_scale: q(1.0),
        }
    }

    fn fixture_world() -> (World, [Entity; 4], [SimEntityId; 8]) {
        let mut world = World::new();
        let active_arena = ActiveArena::new(3);
        let mut match_state = MatchState::default();
        match_state.phase = MatchPhase::Fighting;
        match_state.set_active_slots([true, false, true, false]);
        match_state.arena_index = active_arena.index();
        match_state.replay_seed = TEST_SEED;

        world.insert_resource(SimTick(77));
        world.insert_resource(SnapshotContract {
            simulation_version: 2,
            protocol_version: 1,
            gameplay_content_hash: 0x11AA_22BB_33CC_44DD,
            match_id: *b"live-world-test!",
            master_seed: TEST_SEED,
            pool_capacities: SIM_ENTITY_POOL_CAPACITIES,
        });
        world.insert_resource(active_arena);
        world.insert_resource(match_state);
        world.insert_resource(MatchTelemetry {
            replay_seed: TEST_SEED,
            ring_outs: 3,
            falls: 2,
            item_hits: 4,
            throws: 5,
            guard_breaks: 6,
            damage_by_fighter: [q(1.25), 0.0, q(3.5), 0.0],
        });
        world.insert_resource(Hitstop { remaining_ticks: 4 });
        world.insert_resource(ArenaHazardState::new(
            active_arena.index(),
            active_arena.definition().hazards.len(),
        ));
        world.insert_resource(ArenaPipeState::new(active_arena.index()));
        world.insert_resource(PowderKegCannonState::new(active_arena.index()));
        world.insert_resource(SimulationIdentityAllocator::default());

        let fighters = spawn_fighter_slots(&mut world);

        for kind in SimEntityKind::ALL {
            recycle_first_slot(&mut world, kind);
        }

        let (_, hitbox_id) = spawn_stable(
            &mut world,
            SimEntityKind::Hitbox,
            (fixture_hitbox(), SimPosition::new(qv(4.0, 5.0, 6.0))),
        );
        let (_, item_id) = spawn_stable(
            &mut world,
            SimEntityKind::Item,
            ArenaItem::new(ItemKind::Apple, qv(1.25, 0.5, -2.0), q(0.25)),
        );
        let (_, special_id) = spawn_stable(
            &mut world,
            SimEntityKind::Special,
            (fixture_special(), SimPosition::new(qv(1.25, 2.5, -3.75))),
        );
        let (_, bee_id) = spawn_stable(
            &mut world,
            SimEntityKind::BeeSkill,
            (fixture_bee_skill(), SimPosition::new(qv(-1.0, 1.5, 2.0))),
        );
        let (_, chick_id) = spawn_stable(
            &mut world,
            SimEntityKind::ChickSkill,
            (fixture_chick_skill(), SimPosition::new(qv(2.0, 2.25, -1.5))),
        );
        let (_, penguin_skill_id) = spawn_stable(
            &mut world,
            SimEntityKind::PenguinSkill,
            (
                fixture_penguin_skill(),
                SimPosition::new(qv(3.0, 1.25, -4.5)),
            ),
        );
        let (_, penguin_surface_id) = spawn_stable(
            &mut world,
            SimEntityKind::PenguinSurface,
            (
                fixture_penguin_surface(),
                SimPosition::new(qv(-3.0, 0.25, 4.5)),
            ),
        );
        let (_, ordnance_id) = spawn_stable(
            &mut world,
            SimEntityKind::ArenaOrdnance,
            (
                ArenaCannonBomb {
                    velocity: qv(2.0, 3.0, -1.0),
                    lifetime: TickTimer::from_ticks(25),
                },
                SimPosition::new(qv(5.0, 1.0, -6.0)),
            ),
        );

        world.get_mut::<FighterInventory>(fighters[0]).unwrap().held = Some(item_id);

        (
            world,
            fighters,
            [
                hitbox_id,
                item_id,
                special_id,
                bee_id,
                chick_id,
                penguin_skill_id,
                penguin_surface_id,
                ordnance_id,
            ],
        )
    }

    fn fixture_world_with_crate_relationships() -> (
        World,
        SimEntityId,
        SimEntityId,
        SimEntityId,
        SimEntityId,
        SimEntityId,
    ) {
        let (mut world, _, ids) = fixture_world();
        let first_source_id = ids[1];
        let first_source_entity = world
            .resource::<SimulationIdentityAllocator>()
            .mapped_entity(first_source_id)
            .unwrap();
        world.entity_mut(first_source_entity).insert(ArenaItem::new(
            ItemKind::Crate,
            qv(1.25, 0.5, -2.0),
            q(0.25),
        ));

        let (_, first_reward_id) = spawn_stable(
            &mut world,
            SimEntityKind::Item,
            ArenaItem::new_crate_reward(
                ItemKind::Apple,
                qv(2.0, 0.5, -2.0),
                q(0.5),
                first_source_id,
            ),
        );
        let (_, second_source_id) = spawn_stable(
            &mut world,
            SimEntityKind::Item,
            ArenaItem::new(ItemKind::Crate, qv(-2.0, 0.5, 2.0), q(0.75)),
        );
        let (_, second_reward_id) = spawn_stable(
            &mut world,
            SimEntityKind::Item,
            ArenaItem::new_crate_reward(
                ItemKind::Turkey,
                qv(-2.5, 0.5, 2.0),
                q(1.0),
                second_source_id,
            ),
        );
        let (_, noncrate_id) = spawn_stable(
            &mut world,
            SimEntityKind::Item,
            ArenaItem::new(ItemKind::Apple, qv(0.0, 0.5, 3.0), q(1.25)),
        );
        (
            world,
            first_source_id,
            first_reward_id,
            second_source_id,
            second_reward_id,
            noncrate_id,
        )
    }

    fn fill_production_stable_pools(world: &mut World) {
        for kind in SimEntityKind::ALL {
            let capacity = world
                .resource::<SimulationIdentityAllocator>()
                .capacity(kind);
            let occupied = world
                .resource::<SimulationIdentityAllocator>()
                .live_count(kind);
            for index in occupied..capacity {
                let position = qv(index as f32 * 0.125, 1.0, -(index as f32) * 0.125);
                match kind {
                    SimEntityKind::Hitbox => {
                        spawn_stable(world, kind, (fixture_hitbox(), SimPosition::new(position)));
                    }
                    SimEntityKind::Item => {
                        spawn_stable(
                            world,
                            kind,
                            ArenaItem::new(ItemKind::Apple, position, q(index as f32 * 0.01)),
                        );
                    }
                    SimEntityKind::Special => {
                        spawn_stable(world, kind, (fixture_special(), SimPosition::new(position)));
                    }
                    SimEntityKind::BeeSkill => {
                        spawn_stable(
                            world,
                            kind,
                            (fixture_bee_skill(), SimPosition::new(position)),
                        );
                    }
                    SimEntityKind::ChickSkill => {
                        spawn_stable(
                            world,
                            kind,
                            (fixture_chick_skill(), SimPosition::new(position)),
                        );
                    }
                    SimEntityKind::PenguinSkill => {
                        spawn_stable(
                            world,
                            kind,
                            (fixture_penguin_skill(), SimPosition::new(position)),
                        );
                    }
                    SimEntityKind::PenguinSurface => {
                        spawn_stable(
                            world,
                            kind,
                            (fixture_penguin_surface(), SimPosition::new(position)),
                        );
                    }
                    SimEntityKind::ArenaOrdnance => {
                        spawn_stable(
                            world,
                            kind,
                            (
                                ArenaCannonBomb {
                                    velocity: qv(2.0, 3.0, -1.0),
                                    lifetime: TickTimer::from_ticks(25),
                                },
                                SimPosition::new(position),
                            ),
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn production_registry_covers_the_closed_stable_kind_catalog() {
        let registry = live_dynamic_snapshot_registry();
        for kind in SimEntityKind::ALL {
            assert!(registry.contains(kind), "missing live codec for {kind:?}");
        }
    }

    #[test]
    fn full_production_pool_snapshot_has_resync_headroom() {
        const FULL_POOL_FIXTURE_ENCODED_BYTES: usize = 91_921;
        const DYNAMIC_OPTIONAL_FIELD_MAX_GROWTH: usize = 11;
        const CONSERVATIVE_NON_DYNAMIC_HEADROOM: usize = 1_024;

        let adapter = LiveWorldSnapshotAdapter::new();
        let (mut world, _, _) = fixture_world();
        fill_production_stable_pools(&mut world);

        let snapshot = adapter.capture(&world).unwrap();
        let expected_objects = SIM_ENTITY_POOL_CAPACITIES
            .into_iter()
            .map(|capacity| capacity as usize)
            .sum::<usize>();
        assert_eq!(snapshot.dynamic_objects.len(), expected_objects);
        let encoded_bytes = snapshot.encode().unwrap().len();
        assert_eq!(encoded_bytes, FULL_POOL_FIXTURE_ENCODED_BYTES);

        // Dynamic payloads have fixed width. Relative to this valid, fully
        // occupied fixture, making both fighter relationships present can add
        // at most one byte each and a related SimEntityId can add nine. The
        // extra non-dynamic allowance comfortably covers every optional fighter
        // relationship without relying on today's exact byte count.
        let conservative_schema_bound = encoded_bytes
            + expected_objects * DYNAMIC_OPTIONAL_FIELD_MAX_GROWTH
            + CONSERVATIVE_NON_DYNAMIC_HEADROOM;
        assert!(
            conservative_schema_bound <= MAX_RESYNC_SNAPSHOT_BYTES,
            "full live snapshot uses {encoded_bytes} bytes; conservative schema bound \
             {conservative_schema_bound} exceeds the {MAX_RESYNC_SNAPSHOT_BYTES}-byte resync cap"
        );
    }

    #[test]
    fn live_item_provenance_requires_one_reward_and_an_arena_crate_source() {
        let source_id = SimEntityId::new(SimEntityKind::Item, 0, 1);
        let mut source = DynamicObjectSnapshot::empty(source_id);
        source.definition_id = 0;
        let mut first_reward =
            DynamicObjectSnapshot::empty(SimEntityId::new(SimEntityKind::Item, 1, 1));
        first_reward.definition_id = 2;
        first_reward.related_entity = Some(source_id);
        assert!(validate_live_item_provenance(&[source.clone(), first_reward.clone()]).is_ok());

        let mut duplicate =
            DynamicObjectSnapshot::empty(SimEntityId::new(SimEntityKind::Item, 2, 1));
        duplicate.definition_id = 4;
        duplicate.related_entity = Some(source_id);
        assert!(
            validate_live_item_provenance(&[source.clone(), first_reward.clone(), duplicate,])
                .is_err()
        );

        source.definition_id = 2;
        assert!(validate_live_item_provenance(&[source, first_reward]).is_err());
    }

    #[test]
    fn hostile_item_provenance_restore_is_rejected_atomically() {
        let adapter = LiveWorldSnapshotAdapter::new();
        let (
            mut world,
            first_source_id,
            first_reward_id,
            _second_source_id,
            second_reward_id,
            noncrate_id,
        ) = fixture_world_with_crate_relationships();
        let baseline = adapter.capture(&mut world).unwrap();
        let baseline_bytes = baseline.encode().unwrap();

        let mut self_related = baseline.clone();
        self_related
            .dynamic_objects
            .iter_mut()
            .find(|object| object.id == first_reward_id)
            .unwrap()
            .related_entity = Some(first_reward_id);

        let mut noncrate_source = baseline.clone();
        noncrate_source
            .dynamic_objects
            .iter_mut()
            .find(|object| object.id == first_reward_id)
            .unwrap()
            .related_entity = Some(noncrate_id);

        let mut duplicate_source = baseline.clone();
        duplicate_source
            .dynamic_objects
            .iter_mut()
            .find(|object| object.id == second_reward_id)
            .unwrap()
            .related_entity = Some(first_source_id);

        for mut hostile in [self_related, noncrate_source, duplicate_source] {
            hostile.match_state.match_ticks_remaining =
                hostile.match_state.match_ticks_remaining.saturating_sub(1);
            assert!(adapter.restore(&mut world, &hostile).is_err());
            assert_eq!(
                adapter.capture(&mut world).unwrap().encode().unwrap(),
                baseline_bytes
            );
        }
    }

    #[test]
    fn full_live_world_round_trip_restores_bytes_hash_roster_and_allocator_layout() {
        let adapter = LiveWorldSnapshotAdapter::new();
        let (mut world, fighters, expected_ids) = fixture_world();

        let captured = adapter.capture(&mut world).unwrap();
        assert_eq!(captured.header.tick, SimTick(77));
        assert_eq!(captured.match_state.active_slots_mask, 0b0101);
        assert!(captured.fighters[0].active);
        assert!(!captured.fighters[1].occupied);
        assert!(captured.fighters[2].active);
        assert!(!captured.fighters[3].occupied);
        assert_eq!(
            captured
                .allocators
                .iter()
                .map(|allocator| allocator.capacity)
                .collect::<Vec<_>>(),
            SIM_ENTITY_POOL_CAPACITIES.to_vec()
        );
        assert_eq!(captured.dynamic_objects.len(), SimEntityKind::ALL.len());
        assert_eq!(
            captured
                .dynamic_objects
                .iter()
                .map(|object| object.id)
                .collect::<Vec<_>>(),
            expected_ids
        );
        assert!(expected_ids.iter().all(|id| id.generation() == 2));

        let encoded = captured.encode().unwrap();
        let hash = captured.canonical_hash().unwrap();
        let decoded = CanonicalSnapshot::decode(&encoded).unwrap();
        assert_eq!(decoded.canonical_hash().unwrap(), hash);

        world.insert_resource(SimTick(999));
        world.resource_mut::<MatchState>().timer_ticks = 123_456;
        world.resource_mut::<MatchTelemetry>().ring_outs = 999;
        for (index, entity) in fighters.into_iter().enumerate() {
            world.get_mut::<SimPosition>(entity).unwrap().translation =
                qv(90.0 + index as f32, 80.0, 70.0);
            world.get_mut::<FighterStats>(entity).unwrap().health = q(1.0);
        }
        let stable_entities = {
            let mut query = world.query_filtered::<Entity, With<StableSimEntity>>();
            query.iter(&world).collect::<Vec<_>>()
        };
        for entity in stable_entities {
            if let Some(mut position) = world.get_mut::<SimPosition>(entity) {
                position.translation = qv(33.0, 44.0, 55.0);
            } else if let Some(mut item) = world.get_mut::<ArenaItem>(entity) {
                item.position = qv(33.0, 44.0, 55.0);
            } else {
                panic!("stable entity has no canonical position component");
            }
        }
        // Occupy a formerly free slot to prove restore replaces the exact
        // allocator generations, occupancy bits, and free-list order.
        let (_, extra_item_id) = spawn_stable(&mut world, SimEntityKind::Item, ());
        assert_eq!(extra_item_id.index(), 1);

        let report = adapter.restore(&mut world, &decoded).unwrap();
        assert_eq!(report.restored_tick, SimTick(77));
        assert_eq!(report.reused_dynamic_entities, 8);
        assert_eq!(report.created_dynamic_entities, 0);
        assert_eq!(report.removed_dynamic_entities, 1);
        assert_eq!(report.restored_dynamic_entities, 8);

        let recaptured = adapter.capture(&mut world).unwrap();
        assert_eq!(recaptured.allocators, captured.allocators);
        assert_eq!(recaptured.dynamic_objects, captured.dynamic_objects);
        assert_eq!(recaptured.encode().unwrap(), encoded);
        assert_eq!(recaptured.canonical_hash().unwrap(), hash);
        let identities = world.resource::<SimulationIdentityAllocator>();
        for id in expected_ids {
            let entity = identities.mapped_entity(id).unwrap();
            assert_eq!(world.get::<StableSimEntity>(entity).unwrap().id(), id);
        }
        assert!(identities.mapped_entity(extra_item_id).is_none());
    }

    #[test]
    fn compatible_live_recapture_preserves_bytes_hash_and_backing_storage() {
        let adapter = LiveWorldSnapshotAdapter::new();
        let (world, _, _) = fixture_world();
        let captured = adapter.capture(&world).unwrap();
        let expected_bytes = captured.encode().unwrap();
        let expected_hash = captured.canonical_hash().unwrap();
        let dynamic_storage = captured.dynamic_objects.as_ptr();
        let allocator_storage = captured.allocators.as_ptr();
        let generation_storage = captured
            .allocators
            .iter()
            .map(|allocator| allocator.generations.as_ptr())
            .collect::<Vec<_>>();

        let recaptured = adapter.capture_reusing(&world, Some(captured)).unwrap();

        assert_eq!(recaptured.dynamic_objects.as_ptr(), dynamic_storage);
        assert_eq!(recaptured.allocators.as_ptr(), allocator_storage);
        for (allocator, expected) in recaptured.allocators.iter().zip(generation_storage) {
            assert_eq!(allocator.generations.as_ptr(), expected);
        }
        assert_eq!(recaptured.encode().unwrap(), expected_bytes);
        assert_eq!(recaptured.canonical_hash().unwrap(), expected_hash);
    }

    #[test]
    fn invalid_dynamic_payload_is_rejected_before_any_live_state_changes() {
        let adapter = LiveWorldSnapshotAdapter::new();
        let (mut world, _, _) = fixture_world();
        let baseline = adapter.capture(&mut world).unwrap();
        let baseline_bytes = baseline.encode().unwrap();

        let mut hostile = baseline.clone();
        let bee = hostile
            .dynamic_objects
            .iter_mut()
            .find(|object| object.id.kind() == SimEntityKind::BeeSkill)
            .unwrap();
        bee.payload[127] = 1;
        assert!(adapter.restore(&mut world, &hostile).is_err());

        let after_rejection = adapter.capture(&mut world).unwrap();
        assert_eq!(after_rejection.encode().unwrap(), baseline_bytes);
    }
}
