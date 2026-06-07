use bevy::prelude::*;

use crate::characters::CharacterKind;
use crate::components::FighterAction;
use crate::reactions::ReactionFamilyId;
use crate::styles::FighterStyleKind;
use crate::techniques::AttackShapeId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EquipmentKind {
    DashCoil,
    AerialSpur,
    CounterCell,
    HeavySeal,
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub const EQUIPMENT_KINDS: [EquipmentKind; 4] = [
    EquipmentKind::DashCoil,
    EquipmentKind::AerialSpur,
    EquipmentKind::CounterCell,
    EquipmentKind::HeavySeal,
];

pub const DEFAULT_FIGHTER_EQUIPMENT: [EquipmentKind; 4] = [
    EquipmentKind::CounterCell,
    EquipmentKind::DashCoil,
    EquipmentKind::AerialSpur,
    EquipmentKind::HeavySeal,
];

#[derive(Component, Clone, Copy, Debug)]
pub struct FighterEquipment {
    pub kind: EquipmentKind,
    pub cooldown: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WeaponKind {
    TrainingBlade,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadoutContext {
    pub character: CharacterKind,
    pub style: FighterStyleKind,
    pub equipment: Option<EquipmentKind>,
    pub weapon: WeaponKind,
}

impl LoadoutContext {
    pub fn new(style: FighterStyleKind, equipment: EquipmentKind) -> Self {
        Self::for_character(CharacterKind::Cat, style, equipment)
    }

    pub fn for_character(
        character: CharacterKind,
        style: FighterStyleKind,
        equipment: EquipmentKind,
    ) -> Self {
        Self {
            character,
            style,
            equipment: Some(equipment),
            weapon: WeaponKind::TrainingBlade,
        }
    }

    pub fn from_style(style: FighterStyleKind) -> Self {
        Self {
            character: CharacterKind::Cat,
            style,
            equipment: None,
            weapon: WeaponKind::TrainingBlade,
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadoutTag {
    BraceArmor,
    DashFlow,
    SpecialFlow,
    DashBurst,
    AerialSpike,
    CounterCharge,
    HeavySeal,
    TrainingWeapon,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadoutModifierSource {
    Style(FighterStyleKind),
    Equipment(EquipmentKind),
    Weapon(WeaponKind),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoadoutAttackModifierDef {
    pub source: LoadoutModifierSource,
    pub tag: LoadoutTag,
    pub action: FighterAction,
    pub cue: &'static str,
    pub cooldown: f32,
    pub damage_scale: f32,
    pub knockback_scale: f32,
    pub vertical_knockback_scale: f32,
    pub hitstop_scale: f32,
    pub shake_scale: f32,
    pub feedback_priority_bonus: u8,
    pub shape_override: Option<AttackShapeId>,
    pub reaction_override: Option<ReactionFamilyId>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoadoutTechniqueModifierDef {
    pub source: LoadoutModifierSource,
    pub tag: LoadoutTag,
    pub action: FighterAction,
    pub duration_scale: f32,
    pub branch_window_secs: Option<(f32, f32)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoadoutHeavyArmorDef {
    pub source: LoadoutModifierSource,
    pub tag: LoadoutTag,
    pub stamina_cost: f32,
    pub invulnerability: f32,
}

const LOADOUT_ATTACK_MODIFIERS: &[LoadoutAttackModifierDef] = &[
    LoadoutAttackModifierDef {
        source: LoadoutModifierSource::Equipment(EquipmentKind::DashCoil),
        tag: LoadoutTag::DashBurst,
        action: FighterAction::DashAttack,
        cue: "equip_dash_coil",
        cooldown: 3.0,
        damage_scale: 1.0,
        knockback_scale: 1.18,
        vertical_knockback_scale: 1.0,
        hitstop_scale: 1.0,
        shake_scale: 1.0,
        feedback_priority_bonus: 0,
        shape_override: None,
        reaction_override: None,
    },
    LoadoutAttackModifierDef {
        source: LoadoutModifierSource::Equipment(EquipmentKind::AerialSpur),
        tag: LoadoutTag::AerialSpike,
        action: FighterAction::JumpAttack,
        cue: "equip_aerial_spur",
        cooldown: 3.4,
        damage_scale: 1.14,
        knockback_scale: 1.0,
        vertical_knockback_scale: 1.0,
        hitstop_scale: 1.0,
        shake_scale: 1.0,
        feedback_priority_bonus: 0,
        shape_override: None,
        reaction_override: None,
    },
    LoadoutAttackModifierDef {
        source: LoadoutModifierSource::Equipment(EquipmentKind::CounterCell),
        tag: LoadoutTag::CounterCharge,
        action: FighterAction::GuardCounter,
        cue: "equip_counter_cell",
        cooldown: 3.8,
        damage_scale: 1.18,
        knockback_scale: 1.0,
        vertical_knockback_scale: 1.0,
        hitstop_scale: 1.0,
        shake_scale: 1.0,
        feedback_priority_bonus: 0,
        shape_override: None,
        reaction_override: None,
    },
    LoadoutAttackModifierDef {
        source: LoadoutModifierSource::Equipment(EquipmentKind::HeavySeal),
        tag: LoadoutTag::HeavySeal,
        action: FighterAction::HeavyAttack,
        cue: "equip_heavy_seal",
        cooldown: 4.0,
        damage_scale: 1.0,
        knockback_scale: 1.12,
        vertical_knockback_scale: 1.0,
        hitstop_scale: 1.0,
        shake_scale: 1.0,
        feedback_priority_bonus: 0,
        shape_override: None,
        reaction_override: None,
    },
];

const VECTOR_DASH_FLOW: LoadoutTechniqueModifierDef = LoadoutTechniqueModifierDef {
    source: LoadoutModifierSource::Style(FighterStyleKind::Vector),
    tag: LoadoutTag::DashFlow,
    action: FighterAction::DashAttack,
    duration_scale: 0.82,
    branch_window_secs: Some((0.16, 0.3)),
};

const ANCHOR_HEAVY_ARMOR: LoadoutHeavyArmorDef = LoadoutHeavyArmorDef {
    source: LoadoutModifierSource::Style(FighterStyleKind::Anchor),
    tag: LoadoutTag::BraceArmor,
    stamina_cost: 18.0,
    invulnerability: crate::constants::HEAVY_STARTUP * 0.9,
};

#[allow(dead_code)]
#[derive(Clone, Copy)]
pub struct EquipmentTuning {
    pub label: &'static str,
    pub effect: &'static str,
    pub cue: &'static str,
    pub accent: Color,
    pub cooldown: f32,
    pub affected_action: FighterAction,
}

impl FighterEquipment {
    pub fn new(kind: EquipmentKind) -> Self {
        Self {
            kind,
            cooldown: 0.0,
        }
    }
}

pub fn equipment_for_fighter_id(id: usize) -> EquipmentKind {
    DEFAULT_FIGHTER_EQUIPMENT[id.min(DEFAULT_FIGHTER_EQUIPMENT.len() - 1)]
}

#[allow(dead_code)]
pub fn equipment_cooldown(kind: EquipmentKind) -> f32 {
    equipment_tuning(kind).cooldown
}

pub fn equipment_label(kind: EquipmentKind) -> &'static str {
    equipment_tuning(kind).label
}

pub fn equipment_effect_label(kind: EquipmentKind) -> &'static str {
    equipment_tuning(kind).effect
}

#[allow(dead_code)]
pub fn equipment_trigger_cue(kind: EquipmentKind) -> &'static str {
    equipment_tuning(kind).cue
}

pub fn equipment_identity(kind: EquipmentKind) -> EquipmentTuning {
    equipment_tuning(kind)
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn next_equipment_kind(kind: EquipmentKind) -> EquipmentKind {
    let index = EQUIPMENT_KINDS
        .iter()
        .position(|candidate| *candidate == kind)
        .unwrap_or(0);
    EQUIPMENT_KINDS[(index + 1) % EQUIPMENT_KINDS.len()]
}

pub fn equipment_tuning(kind: EquipmentKind) -> EquipmentTuning {
    match kind {
        EquipmentKind::DashCoil => EquipmentTuning {
            label: "Dash Coil",
            effect: "dash burst",
            cue: "equip_dash_coil",
            accent: Color::srgb(0.35, 0.9, 1.0),
            cooldown: 3.0,
            affected_action: FighterAction::DashAttack,
        },
        EquipmentKind::AerialSpur => EquipmentTuning {
            label: "Aerial Spur",
            effect: "air spike",
            cue: "equip_aerial_spur",
            accent: Color::srgb(1.0, 0.78, 0.28),
            cooldown: 3.4,
            affected_action: FighterAction::JumpAttack,
        },
        EquipmentKind::CounterCell => EquipmentTuning {
            label: "Counter Cell",
            effect: "guard counter",
            cue: "equip_counter_cell",
            accent: Color::srgb(0.64, 1.0, 0.52),
            cooldown: 3.8,
            affected_action: FighterAction::GuardCounter,
        },
        EquipmentKind::HeavySeal => EquipmentTuning {
            label: "Heavy Seal",
            effect: "heavy launch",
            cue: "equip_heavy_seal",
            accent: Color::srgb(1.0, 0.44, 0.3),
            cooldown: 4.0,
            affected_action: FighterAction::HeavyAttack,
        },
    }
}

#[allow(dead_code)]
pub fn equipment_triggers_on(kind: EquipmentKind, action: FighterAction) -> bool {
    loadout_attack_modifiers(
        LoadoutContext::new(FighterStyleKind::Anchor, kind),
        action,
        true,
    )
    .any(|modifier| modifier.source == LoadoutModifierSource::Equipment(kind))
}

pub fn loadout_has_tag(context: LoadoutContext, tag: LoadoutTag) -> bool {
    match tag {
        LoadoutTag::BraceArmor => context.style == FighterStyleKind::Anchor,
        LoadoutTag::DashFlow => context.style == FighterStyleKind::Vector,
        LoadoutTag::SpecialFlow => context.style == FighterStyleKind::Catalyst,
        LoadoutTag::DashBurst => context.equipment == Some(EquipmentKind::DashCoil),
        LoadoutTag::AerialSpike => context.equipment == Some(EquipmentKind::AerialSpur),
        LoadoutTag::CounterCharge => context.equipment == Some(EquipmentKind::CounterCell),
        LoadoutTag::HeavySeal => context.equipment == Some(EquipmentKind::HeavySeal),
        LoadoutTag::TrainingWeapon => context.weapon == WeaponKind::TrainingBlade,
    }
}

pub fn loadout_attack_modifiers(
    context: LoadoutContext,
    action: FighterAction,
    equipment_ready: bool,
) -> impl Iterator<Item = LoadoutAttackModifierDef> {
    LOADOUT_ATTACK_MODIFIERS
        .iter()
        .copied()
        .filter(move |modifier| {
            modifier.action == action
                && source_matches_loadout(modifier.source, context)
                && (!matches!(modifier.source, LoadoutModifierSource::Equipment(_))
                    || equipment_ready)
        })
}

pub fn loadout_technique_modifier(
    context: LoadoutContext,
    action: FighterAction,
) -> Option<LoadoutTechniqueModifierDef> {
    if action == FighterAction::DashAttack && loadout_has_tag(context, LoadoutTag::DashFlow) {
        Some(VECTOR_DASH_FLOW)
    } else {
        None
    }
}

pub fn loadout_heavy_armor(context: LoadoutContext) -> Option<LoadoutHeavyArmorDef> {
    loadout_has_tag(context, LoadoutTag::BraceArmor).then_some(ANCHOR_HEAVY_ARMOR)
}

pub fn loadout_heavy_whiff_recovery_scale(context: LoadoutContext) -> f32 {
    if loadout_has_tag(context, LoadoutTag::BraceArmor) {
        1.22
    } else {
        1.0
    }
}

#[allow(dead_code)]
pub fn loadout_special_cost_scale(context: LoadoutContext) -> f32 {
    if loadout_has_tag(context, LoadoutTag::SpecialFlow) {
        0.88
    } else {
        1.0
    }
}

pub fn loadout_special_cooldown_scale(context: LoadoutContext) -> f32 {
    if loadout_has_tag(context, LoadoutTag::SpecialFlow) {
        0.84
    } else {
        1.0
    }
}

pub fn loadout_special_stamina_disrupt(context: LoadoutContext) -> f32 {
    if loadout_has_tag(context, LoadoutTag::SpecialFlow) {
        12.0
    } else {
        0.0
    }
}

fn source_matches_loadout(source: LoadoutModifierSource, context: LoadoutContext) -> bool {
    match source {
        LoadoutModifierSource::Style(style) => context.style == style,
        LoadoutModifierSource::Equipment(equipment) => context.equipment == Some(equipment),
        LoadoutModifierSource::Weapon(weapon) => context.weapon == weapon,
    }
}

#[allow(dead_code)]
pub fn equipment_status_label(equipment: &FighterEquipment) -> String {
    if equipment.cooldown <= 0.0 {
        format!(
            "{} Ready ({})",
            equipment_label(equipment.kind),
            equipment_effect_label(equipment.kind)
        )
    } else {
        format!(
            "{} {:.1}s ({})",
            equipment_label(equipment.kind),
            equipment.cooldown,
            equipment_effect_label(equipment.kind)
        )
    }
}

pub fn tick_equipment_cooldowns(time: Res<Time>, mut equipment: Query<&mut FighterEquipment>) {
    let dt = time.delta_secs();
    for mut equipment in &mut equipment {
        equipment.cooldown = (equipment.cooldown - dt).max(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equipment_modifiers_are_single_move_hooks() {
        assert!(equipment_triggers_on(
            EquipmentKind::DashCoil,
            FighterAction::DashAttack
        ));
        assert!(!equipment_triggers_on(
            EquipmentKind::DashCoil,
            FighterAction::HeavyAttack
        ));
        assert_eq!(
            equipment_tuning(EquipmentKind::HeavySeal).affected_action,
            FighterAction::HeavyAttack
        );
    }

    #[test]
    fn loadout_tags_cover_style_equipment_and_weapon_hooks() {
        let vector_dash = LoadoutContext::new(FighterStyleKind::Vector, EquipmentKind::DashCoil);
        let anchor_heavy = LoadoutContext::new(FighterStyleKind::Anchor, EquipmentKind::HeavySeal);

        assert!(loadout_has_tag(vector_dash, LoadoutTag::DashFlow));
        assert!(loadout_has_tag(vector_dash, LoadoutTag::DashBurst));
        assert!(loadout_has_tag(vector_dash, LoadoutTag::TrainingWeapon));
        assert!(loadout_has_tag(anchor_heavy, LoadoutTag::BraceArmor));
        assert!(loadout_has_tag(anchor_heavy, LoadoutTag::HeavySeal));
    }

    #[test]
    fn loadout_attack_modifiers_are_data_driven() {
        let loadout = LoadoutContext::new(FighterStyleKind::Anchor, EquipmentKind::DashCoil);
        let modifier = loadout_attack_modifiers(loadout, FighterAction::DashAttack, true)
            .next()
            .expect("dash coil should modify dash attack");

        assert_eq!(modifier.tag, LoadoutTag::DashBurst);
        assert!(modifier.knockback_scale > 1.0);
        assert!(modifier.cooldown > 0.0);
        assert!(
            loadout_attack_modifiers(loadout, FighterAction::DashAttack, false)
                .next()
                .is_none()
        );
    }

    #[test]
    fn loadout_action_feel_modifiers_are_reusable_tags() {
        let vector = LoadoutContext::new(FighterStyleKind::Vector, EquipmentKind::CounterCell);
        let anchor = LoadoutContext::new(FighterStyleKind::Anchor, EquipmentKind::CounterCell);

        let dash_flow = loadout_technique_modifier(vector, FighterAction::DashAttack)
            .expect("vector should own dash flow");
        assert_eq!(dash_flow.tag, LoadoutTag::DashFlow);
        assert!(dash_flow.duration_scale < 1.0);
        assert!(dash_flow.branch_window_secs.is_some());
        assert!(loadout_technique_modifier(anchor, FighterAction::DashAttack).is_none());
        assert!(loadout_heavy_armor(anchor).is_some());
        assert!(loadout_heavy_armor(vector).is_none());
    }

    #[test]
    fn equipment_identity_covers_all_modifiers() {
        for kind in EQUIPMENT_KINDS {
            let tuning = equipment_identity(kind);
            assert!(!tuning.label.is_empty());
            assert!(!tuning.effect.is_empty());
            assert!(!tuning.cue.is_empty());
            assert!(tuning.cooldown > 0.0);
        }
    }
}
