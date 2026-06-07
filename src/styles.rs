use bevy::prelude::*;

use crate::constants::HEAVY_STARTUP;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FighterStyleKind {
    Anchor,
    Vector,
    Catalyst,
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub const FIGHTER_STYLE_KINDS: [FighterStyleKind; 3] = [
    FighterStyleKind::Anchor,
    FighterStyleKind::Vector,
    FighterStyleKind::Catalyst,
];

pub const DEFAULT_FIGHTER_STYLES: [FighterStyleKind; 4] = [
    FighterStyleKind::Anchor,
    FighterStyleKind::Vector,
    FighterStyleKind::Catalyst,
    FighterStyleKind::Vector,
];

#[derive(Component, Clone, Copy, Debug)]
pub struct FighterStyle {
    pub kind: FighterStyleKind,
}

#[allow(dead_code)]
#[derive(Clone, Copy)]
pub struct StyleTuning {
    pub ground_speed: f32,
    pub air_speed: f32,
    pub dash_impulse: f32,
    pub dash_cost: f32,
    pub stamina_regen: f32,
    pub guard_drain: f32,
    pub attack_startup: f32,
    pub attack_recovery: f32,
    pub damage: f32,
    pub knockback: f32,
    pub throw_knockback: f32,
    pub bot_preferred_range: f32,
    pub bot_special_bias: f32,
}

#[derive(Clone, Copy)]
pub struct StyleIdentity {
    pub tagline: &'static str,
    pub accent: Color,
    pub marker_scale: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StyleMechanics {
    pub hook_label: &'static str,
    pub guard_step_invulnerability: f32,
    pub dash_attack_duration: f32,
    pub special_cost: f32,
    pub special_cooldown: f32,
    pub heavy_armor_cost: f32,
    pub heavy_armor_invulnerability: f32,
    pub heavy_whiff_recovery_scale: f32,
    pub dash_light_branch: Option<(f32, f32)>,
    pub special_stamina_disrupt: f32,
}

pub fn style_tuning(kind: FighterStyleKind) -> StyleTuning {
    match kind {
        FighterStyleKind::Anchor => StyleTuning {
            ground_speed: 0.9,
            air_speed: 0.86,
            dash_impulse: 0.9,
            dash_cost: 0.84,
            stamina_regen: 1.12,
            guard_drain: 0.78,
            attack_startup: 1.08,
            attack_recovery: 1.04,
            damage: 1.08,
            knockback: 1.08,
            throw_knockback: 1.14,
            bot_preferred_range: 1.25,
            bot_special_bias: 0.8,
        },
        FighterStyleKind::Vector => StyleTuning {
            ground_speed: 1.12,
            air_speed: 1.1,
            dash_impulse: 1.12,
            dash_cost: 1.08,
            stamina_regen: 0.92,
            guard_drain: 1.22,
            attack_startup: 0.9,
            attack_recovery: 0.92,
            damage: 0.94,
            knockback: 0.98,
            throw_knockback: 0.94,
            bot_preferred_range: 1.55,
            bot_special_bias: 1.05,
        },
        FighterStyleKind::Catalyst => StyleTuning {
            ground_speed: 1.0,
            air_speed: 0.98,
            dash_impulse: 1.0,
            dash_cost: 0.98,
            stamina_regen: 1.02,
            guard_drain: 1.0,
            attack_startup: 1.0,
            attack_recovery: 1.02,
            damage: 0.98,
            knockback: 1.0,
            throw_knockback: 1.04,
            bot_preferred_range: 2.0,
            bot_special_bias: 1.25,
        },
    }
}

pub fn style_mechanics(kind: FighterStyleKind) -> StyleMechanics {
    match kind {
        FighterStyleKind::Anchor => StyleMechanics {
            hook_label: "brace step",
            guard_step_invulnerability: 1.32,
            dash_attack_duration: 1.0,
            special_cost: 1.0,
            special_cooldown: 1.0,
            heavy_armor_cost: 18.0,
            heavy_armor_invulnerability: HEAVY_STARTUP * 0.9,
            heavy_whiff_recovery_scale: 1.22,
            dash_light_branch: None,
            special_stamina_disrupt: 0.0,
        },
        FighterStyleKind::Vector => StyleMechanics {
            hook_label: "dash flow",
            guard_step_invulnerability: 1.0,
            dash_attack_duration: 0.82,
            special_cost: 1.0,
            special_cooldown: 1.0,
            heavy_armor_cost: 0.0,
            heavy_armor_invulnerability: 0.0,
            heavy_whiff_recovery_scale: 1.0,
            dash_light_branch: Some((0.16, 0.3)),
            special_stamina_disrupt: 0.0,
        },
        FighterStyleKind::Catalyst => StyleMechanics {
            hook_label: "special flow",
            guard_step_invulnerability: 1.0,
            dash_attack_duration: 1.0,
            special_cost: 0.88,
            special_cooldown: 0.84,
            heavy_armor_cost: 0.0,
            heavy_armor_invulnerability: 0.0,
            heavy_whiff_recovery_scale: 1.0,
            dash_light_branch: None,
            special_stamina_disrupt: 12.0,
        },
    }
}

pub fn style_identity(kind: FighterStyleKind) -> StyleIdentity {
    match kind {
        FighterStyleKind::Anchor => StyleIdentity {
            tagline: "brace throw",
            accent: Color::srgb(0.96, 0.64, 0.22),
            marker_scale: 1.12,
        },
        FighterStyleKind::Vector => StyleIdentity {
            tagline: "rush pressure",
            accent: Color::srgb(0.34, 0.92, 1.0),
            marker_scale: 0.92,
        },
        FighterStyleKind::Catalyst => StyleIdentity {
            tagline: "special zone",
            accent: Color::srgb(0.58, 1.0, 0.42),
            marker_scale: 1.02,
        },
    }
}

pub fn style_for_fighter_id(id: usize) -> FighterStyleKind {
    DEFAULT_FIGHTER_STYLES[id.min(DEFAULT_FIGHTER_STYLES.len() - 1)]
}

pub fn style_label(kind: FighterStyleKind) -> &'static str {
    match kind {
        FighterStyleKind::Anchor => "Anchor",
        FighterStyleKind::Vector => "Vector",
        FighterStyleKind::Catalyst => "Catalyst",
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn next_style_kind(kind: FighterStyleKind) -> FighterStyleKind {
    let index = FIGHTER_STYLE_KINDS
        .iter()
        .position(|candidate| *candidate == kind)
        .unwrap_or(0);
    FIGHTER_STYLE_KINDS[(index + 1) % FIGHTER_STYLE_KINDS.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_tuning_varies_core_combat_axes() {
        let anchor = style_tuning(FighterStyleKind::Anchor);
        let vector = style_tuning(FighterStyleKind::Vector);
        let catalyst = style_tuning(FighterStyleKind::Catalyst);

        assert!(anchor.guard_drain < vector.guard_drain);
        assert!(vector.ground_speed > anchor.ground_speed);
        assert!(catalyst.bot_preferred_range > anchor.bot_preferred_range);
    }

    #[test]
    fn style_identity_covers_all_styles() {
        for kind in FIGHTER_STYLE_KINDS {
            let identity = style_identity(kind);
            assert!(!style_label(kind).is_empty());
            assert!(!identity.tagline.is_empty());
            assert!(identity.marker_scale > 0.0);
        }
    }

    #[test]
    fn style_mechanics_add_one_named_hook_per_style() {
        let anchor = style_mechanics(FighterStyleKind::Anchor);
        let vector = style_mechanics(FighterStyleKind::Vector);
        let catalyst = style_mechanics(FighterStyleKind::Catalyst);

        assert!(anchor.guard_step_invulnerability > 1.0);
        assert!(anchor.heavy_armor_cost > 0.0);
        assert!(anchor.heavy_whiff_recovery_scale > 1.0);
        assert!(vector.dash_attack_duration < 1.0);
        assert!(vector.dash_light_branch.is_some());
        assert!(catalyst.special_cost < 1.0);
        assert!(catalyst.special_cooldown < 1.0);
        assert!(catalyst.special_stamina_disrupt > 0.0);
        assert_ne!(anchor.hook_label, vector.hook_label);
        assert_ne!(vector.hook_label, catalyst.hook_label);
    }
}
