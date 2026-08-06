use bevy::prelude::{Vec2, Vec3};

use crate::components::FighterAction;
#[cfg(test)]
use crate::constants::FIGHTER_COUNT;
use crate::constants::MAX_STAMINA;
use crate::techniques::TechniquePrediction;

use super::intelligence::BotSemanticAction;

pub(super) const MAX_TACTICAL_ACTIONS: usize = 12;
pub(super) const MAX_RESPONSE_BRANCHES: usize = 3;
pub(super) const MAX_FORECAST_FOLLOW_UPS: usize = 4;
pub(super) const DASH_FORECAST_TICKS: u8 = 7;
pub(super) const MAX_PLAN_STEPS: usize = 3;
const RESPONSE_FAMILY_COUNT: usize = 8;
const RESPONSE_RANGE_COUNT: usize = 3;
const RESPONSE_STATE_COUNT: usize = 4;
const RESPONSE_EDGE_COUNT: usize = 2;
const RESPONSE_OUTCOME_COUNT: usize = 4;
const RESPONSE_CONTEXT_COUNT: usize =
    RESPONSE_RANGE_COUNT * RESPONSE_STATE_COUNT * RESPONSE_EDGE_COUNT * RESPONSE_OUTCOME_COUNT;
const RESPONSE_PRIOR: f32 = 2.0;
const RESPONSE_DECAY_NUMERATOR: f32 = 7.0;
const RESPONSE_DECAY_DENOMINATOR: f32 = 8.0;
const RESPONSE_DECAY_TICKS: u32 = 20;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum TacticalPhase {
    #[default]
    Neutral,
    Advantage,
    Disadvantage,
    HitConfirm,
    GuardPressure,
    WakeUp,
    EdgePressure,
    ResourceRecovery,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum TacticId {
    #[default]
    NeutralPoke,
    WhiffPunish,
    BaitAndPunish,
    StrikeThrow,
    AntiAir,
    PressureString,
    EscapePressure,
    ProjectilePressure,
    EdgeControl,
}

impl TacticId {
    pub(super) const COUNT: usize = 9;

    pub(super) const fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum PlanBranch {
    #[default]
    Start,
    Pending,
    Hit,
    Guarded,
    Whiff,
    TargetAirborne,
    Threatened,
    Unsafe,
    Timeout,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct PlanInvalidationInputs {
    pub(super) target_valid: bool,
    pub(super) incapacitated: bool,
    pub(super) unsafe_ground: bool,
    pub(super) stamina: f32,
    pub(super) required_stamina: f32,
    pub(super) displacement: f32,
    pub(super) displacement_limit: f32,
    pub(super) rejection_count: u8,
}

pub(super) fn plan_is_invalid(input: PlanInvalidationInputs) -> bool {
    !input.target_valid
        || input.incapacitated
        || input.unsafe_ground
        || input.stamina + f32::EPSILON < input.required_stamina
        || input.displacement > input.displacement_limit
        || input.rejection_count >= 2
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum PlanMovement {
    #[default]
    Hold,
    Approach,
    Retreat,
    Strafe,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum OpponentResponse {
    #[default]
    Attack,
    Guard,
    Grab,
    Jump,
    Dodge,
    Retreat,
    Special,
    Wait,
}

impl OpponentResponse {
    pub(super) const ALL: [Self; RESPONSE_FAMILY_COUNT] = [
        Self::Attack,
        Self::Guard,
        Self::Grab,
        Self::Jump,
        Self::Dodge,
        Self::Retreat,
        Self::Special,
        Self::Wait,
    ];

    const fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum MoveRelativeRange {
    #[default]
    Inside,
    Fringe,
    Outside,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum ResponseOpponentState {
    #[default]
    Neutral,
    Attacking,
    Guarding,
    Vulnerable,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum PreviousOutcome {
    #[default]
    None,
    Hit,
    Guarded,
    Whiff,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ResponseContext {
    pub(super) range: MoveRelativeRange,
    pub(super) opponent_state: ResponseOpponentState,
    pub(super) edge_pressure: bool,
    pub(super) previous_outcome: PreviousOutcome,
}

impl ResponseContext {
    fn index(self) -> usize {
        (((self.range as usize * RESPONSE_STATE_COUNT + self.opponent_state as usize)
            * RESPONSE_EDGE_COUNT
            + usize::from(self.edge_pressure))
            * RESPONSE_OUTCOME_COUNT)
            + self.previous_outcome as usize
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ResponsePrediction {
    pub(crate) response: OpponentResponse,
    pub(crate) probability: f32,
}

#[derive(Clone)]
pub(super) struct OpponentResponseModel {
    counts: [[f32; RESPONSE_FAMILY_COUNT]; RESPONSE_CONTEXT_COUNT],
    ticks_until_decay: u32,
}

impl Default for OpponentResponseModel {
    fn default() -> Self {
        Self {
            counts: [[RESPONSE_PRIOR; RESPONSE_FAMILY_COUNT]; RESPONSE_CONTEXT_COUNT],
            ticks_until_decay: RESPONSE_DECAY_TICKS,
        }
    }
}

impl OpponentResponseModel {
    pub(super) fn observe(&mut self, context: ResponseContext, response: OpponentResponse) {
        let count = &mut self.counts[context.index()][response.index()];
        *count = (*count + 1.0).min(1_024.0);
    }

    pub(super) fn advance_ticks(&mut self, mut elapsed_ticks: u64) {
        while elapsed_ticks >= u64::from(self.ticks_until_decay) {
            elapsed_ticks -= u64::from(self.ticks_until_decay);
            self.decay();
            self.ticks_until_decay = RESPONSE_DECAY_TICKS;
        }
        self.ticks_until_decay = self
            .ticks_until_decay
            .saturating_sub(elapsed_ticks as u32)
            .max(1);
    }

    fn decay(&mut self) {
        for context in &mut self.counts {
            for count in context {
                *count *= RESPONSE_DECAY_NUMERATOR / RESPONSE_DECAY_DENOMINATOR;
            }
        }
    }

    pub(super) fn top_three(
        &self,
        context: ResponseContext,
    ) -> [ResponsePrediction; MAX_RESPONSE_BRANCHES] {
        let counts = &self.counts[context.index()];
        let total = counts.iter().copied().sum::<f32>().max(f32::EPSILON);
        let mut top = [ResponsePrediction {
            response: OpponentResponse::Attack,
            probability: f32::NEG_INFINITY,
        }; MAX_RESPONSE_BRANCHES];

        for response in OpponentResponse::ALL {
            let prediction = ResponsePrediction {
                response,
                probability: counts[response.index()] / total,
            };
            for index in 0..MAX_RESPONSE_BRANCHES {
                let current = top[index];
                let comes_first = prediction.probability > current.probability
                    || (prediction.probability == current.probability
                        && prediction.response.index() < current.response.index());
                if comes_first {
                    for shift in (index + 1..MAX_RESPONSE_BRANCHES).rev() {
                        top[shift] = top[shift - 1];
                    }
                    top[index] = prediction;
                    break;
                }
            }
        }
        top
    }

    #[cfg(test)]
    pub(super) fn count(&self, context: ResponseContext, response: OpponentResponse) -> f32 {
        self.counts[context.index()][response.index()]
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct ForecastMoveFacts {
    pub(super) action: Option<BotSemanticAction>,
    pub(super) startup_ticks: u8,
    pub(super) active_ticks: u8,
    pub(super) recovery_ticks: u8,
    pub(super) range: f32,
    pub(super) vertical_tolerance: f32,
    pub(super) stamina_cost: f32,
    pub(super) estimated_damage: f32,
    pub(super) guardable: bool,
    pub(super) forward_motion: f32,
}

impl ForecastMoveFacts {
    pub(super) fn from_prediction(
        action: BotSemanticAction,
        prediction: TechniquePrediction,
    ) -> Self {
        let startup_ms = prediction
            .startup_ms
            .or(prediction.spawned_skill_startup_ms)
            .unwrap_or(0);
        let active_end_ms = prediction.direct_active_end_ms.unwrap_or(startup_ms);
        Self {
            action: Some(action),
            startup_ticks: milliseconds_to_ticks(startup_ms),
            active_ticks: milliseconds_to_ticks(active_end_ms.saturating_sub(startup_ms)).max(1),
            recovery_ticks: milliseconds_to_ticks(prediction.recover_at_ms),
            range: prediction
                .planar_contact_envelope()
                .max(prediction.spawned_skill_range),
            vertical_tolerance: prediction
                .max_radius
                .max(prediction.spawned_skill_vertical_tolerance)
                .max(0.5),
            stamina_cost: prediction.stamina_cost,
            // Detached skills expose authored timing and geometry but not a
            // single direct payload because several spawn repeated fields or
            // multiple projectiles. Keep their compact forecast conservative.
            estimated_damage: prediction
                .estimated_damage
                .max(if prediction.has_spawned_skill {
                    6.0
                } else {
                    0.0
                }),
            guardable: prediction.all_direct_attacks_guardable || prediction.has_spawned_skill,
            forward_motion: prediction.authored_forward_motion,
        }
    }

    pub(super) fn fallback(action: BotSemanticAction, distance: f32) -> Self {
        let (startup_ticks, recovery_ticks, range, damage, guardable) = match action {
            BotSemanticAction::Grab => (3, 14, 0.95, 10.0, false),
            BotSemanticAction::Dash => (1, DASH_FORECAST_TICKS, 0.0, 0.0, false),
            BotSemanticAction::Jump => (1, 9, 0.0, 0.0, false),
            BotSemanticAction::SpecialProjectile => (6, 20, distance.max(3.0), 8.0, true),
            BotSemanticAction::SpecialTrap => (7, 20, 1.4, 8.0, true),
            BotSemanticAction::SpecialHazard | BotSemanticAction::SpecialShockwave => {
                (8, 22, 3.2, 10.0, true)
            }
            _ => (4, 14, 1.5, 7.0, true),
        };
        Self {
            action: Some(action),
            startup_ticks,
            active_ticks: 2,
            recovery_ticks,
            range,
            vertical_tolerance: 1.0,
            stamina_cost: 0.0,
            estimated_damage: damage,
            guardable,
            forward_motion: 0.0,
        }
    }
}

fn milliseconds_to_ticks(milliseconds: u32) -> u8 {
    ((milliseconds.saturating_add(49) / 50).min(u32::from(u8::MAX))) as u8
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum ForecastActionPhase {
    #[default]
    Neutral,
    Startup,
    Active,
    Recovery,
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CombatForecastState {
    pub(super) bot_position: Vec2,
    pub(super) target_position: Vec2,
    pub(super) bot_velocity: Vec2,
    pub(super) target_velocity: Vec2,
    pub(super) bot_facing: Vec2,
    pub(super) target_facing: Vec2,
    pub(super) bot_action: FighterAction,
    pub(super) target_action: FighterAction,
    pub(super) bot_action_phase: ForecastActionPhase,
    pub(super) target_action_phase: ForecastActionPhase,
    pub(super) bot_action_ticks_remaining: u8,
    pub(super) target_action_ticks_remaining: u8,
    pub(super) bot_health: f32,
    pub(super) target_health: f32,
    pub(super) bot_stamina: f32,
    pub(super) target_stamina: f32,
    pub(super) bot_grounded: bool,
    pub(super) target_grounded: bool,
    pub(super) bot_edge_risk: f32,
    pub(super) target_edge_risk: f32,
    pub(super) bot_move: Option<ForecastMoveFacts>,
    pub(super) target_move: Option<ForecastMoveFacts>,
    pub(super) external_threat_cost: f32,
}

impl Default for CombatForecastState {
    fn default() -> Self {
        Self {
            bot_position: Vec2::ZERO,
            target_position: Vec2::ZERO,
            bot_velocity: Vec2::ZERO,
            target_velocity: Vec2::ZERO,
            bot_facing: Vec2::X,
            target_facing: -Vec2::X,
            bot_action: FighterAction::Idle,
            target_action: FighterAction::Idle,
            bot_action_phase: ForecastActionPhase::Neutral,
            target_action_phase: ForecastActionPhase::Neutral,
            bot_action_ticks_remaining: 0,
            target_action_ticks_remaining: 0,
            bot_health: 0.0,
            target_health: 0.0,
            bot_stamina: 0.0,
            target_stamina: 0.0,
            bot_grounded: true,
            target_grounded: true,
            bot_edge_risk: 0.0,
            target_edge_risk: 0.0,
            bot_move: None,
            target_move: None,
            external_threat_cost: 0.0,
        }
    }
}

impl CombatForecastState {
    pub(super) fn distance(self) -> f32 {
        self.bot_position.distance(self.target_position)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct PhaseInputs {
    pub(super) bot_action: FighterAction,
    pub(super) target_action: FighterAction,
    pub(super) bot_confirmed_hit: bool,
    pub(super) bot_confirmed_guard: bool,
    pub(super) cancel_window_open: bool,
    pub(super) bot_recovery_ticks: u8,
    pub(super) target_recovery_ticks: u8,
    pub(super) stamina_ratio: f32,
    pub(super) distance: f32,
    pub(super) bot_edge_risk: f32,
    pub(super) target_edge_risk: f32,
}

impl Default for PhaseInputs {
    fn default() -> Self {
        Self {
            bot_action: FighterAction::Idle,
            target_action: FighterAction::Idle,
            bot_confirmed_hit: false,
            bot_confirmed_guard: false,
            cancel_window_open: false,
            bot_recovery_ticks: 0,
            target_recovery_ticks: 0,
            stamina_ratio: 0.0,
            distance: 0.0,
            bot_edge_risk: 0.0,
            target_edge_risk: 0.0,
        }
    }
}

pub(super) fn classify_phase(input: PhaseInputs) -> TacticalPhase {
    if matches!(
        input.bot_action,
        FighterAction::Knockdown
            | FighterAction::QuickStand
            | FighterAction::RecoveryRoll
            | FighterAction::GetUp
    ) {
        return TacticalPhase::WakeUp;
    }
    if matches!(
        input.bot_action,
        FighterAction::Hitstun
            | FighterAction::GuardBroken
            | FighterAction::Grabbed
            | FighterAction::LandingRecovery
    ) || (is_offensive_action(input.target_action)
        && input.distance < 2.5
        && input.target_recovery_ticks <= input.bot_recovery_ticks)
    {
        return TacticalPhase::Disadvantage;
    }
    if input.stamina_ratio < 0.28
        && input.distance > 2.0
        && !is_offensive_action(input.target_action)
    {
        return TacticalPhase::ResourceRecovery;
    }
    if input.bot_confirmed_hit && !input.bot_confirmed_guard && input.cancel_window_open {
        return TacticalPhase::HitConfirm;
    }
    if (input.bot_confirmed_guard || input.target_action == FighterAction::Guarding)
        && input.distance < 1.7
    {
        return TacticalPhase::GuardPressure;
    }
    if input.target_edge_risk > 0.45 && input.target_edge_risk > input.bot_edge_risk + 0.15 {
        return TacticalPhase::EdgePressure;
    }
    if matches!(
        input.target_action,
        FighterAction::Hitstun
            | FighterAction::Knockdown
            | FighterAction::GuardBroken
            | FighterAction::LandingRecovery
    ) || input.target_recovery_ticks > input.bot_recovery_ticks.saturating_add(2)
    {
        return TacticalPhase::Advantage;
    }
    TacticalPhase::Neutral
}

pub(super) fn is_offensive_action(action: FighterAction) -> bool {
    matches!(
        action,
        FighterAction::LightAttack1
            | FighterAction::LightAttack2
            | FighterAction::ComboFinisher
            | FighterAction::HeavyAttack
            | FighterAction::HeavyAttack2
            | FighterAction::UltimateStartup
            | FighterAction::UltimateRush
            | FighterAction::DashAttack
            | FighterAction::JumpAttack
            | FighterAction::JumpHeavyAttack
            | FighterAction::GrabStartup
            | FighterAction::ItemSwing
            | FighterAction::ItemThrow
            | FighterAction::SpecialCast
            | FighterAction::GuardCounter
    )
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct ForecastOutcome {
    pub(super) health_swing: f32,
    pub(super) stamina_swing: f32,
    pub(super) initiative: f32,
    pub(super) arena_position: f32,
    pub(super) edge_safety: f32,
    pub(super) expected_contact_tick: Option<u8>,
    pub(super) recovery_exposure: f32,
    pub(super) whiff_risk: f32,
    pub(super) worst_loss: f32,
}

impl ForecastOutcome {
    pub(super) fn utility(self) -> f32 {
        (self.health_swing / 20.0).clamp(-2.0, 2.0)
            + (self.stamina_swing / MAX_STAMINA).clamp(-1.0, 1.0) * 0.65
            + self.initiative.clamp(-1.0, 1.0) * 0.8
            + self.arena_position.clamp(-1.0, 1.0) * 0.35
            + self.edge_safety.clamp(-1.0, 1.0) * 0.8
            - self.recovery_exposure.clamp(0.0, 2.0) * 0.7
            - self.whiff_risk.clamp(0.0, 1.0) * 0.8
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ForecastAction {
    pub(super) action: BotSemanticAction,
    pub(super) tactic: TacticId,
    pub(super) facts: ForecastMoveFacts,
}

impl Default for ForecastAction {
    fn default() -> Self {
        Self {
            action: BotSemanticAction::Light,
            tactic: TacticId::NeutralPoke,
            facts: ForecastMoveFacts::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct ForecastFollowUp {
    pub(super) facts: Option<ForecastMoveFacts>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct ForecastPlanScore {
    pub(super) expected_utility: f32,
    pub(super) worst_loss: f32,
    pub(super) strategic_score: f32,
    pub(super) final_score: f32,
    pub(super) outcomes_evaluated: u16,
}

pub(super) fn score_forecast_plan(
    state: CombatForecastState,
    action: ForecastAction,
    responses: [ResponsePrediction; MAX_RESPONSE_BRANCHES],
    follow_ups: [ForecastFollowUp; MAX_FORECAST_FOLLOW_UPS],
    horizon_ticks: u32,
    risk_weight: f32,
    tactic_bias: f32,
    seeded_jitter: f32,
) -> ForecastPlanScore {
    let mut expected_utility = 0.0;
    let mut worst_loss = 0.0_f32;
    let mut evaluated = 0_u16;
    let response_mass = responses
        .iter()
        .map(|prediction| prediction.probability.max(0.0))
        .sum::<f32>()
        .max(f32::EPSILON);

    for response in responses {
        let mut best = None::<ForecastOutcome>;
        let mut empty_follow_up_evaluated = false;
        for follow_up in follow_ups {
            if follow_up.facts.is_none() {
                if empty_follow_up_evaluated {
                    continue;
                }
                empty_follow_up_evaluated = true;
            }
            let outcome = forecast_sequence(
                state,
                action,
                response.response,
                follow_up.facts,
                horizon_ticks,
            );
            evaluated = evaluated.saturating_add(1);
            if best.is_none_or(|current| outcome.utility() > current.utility()) {
                best = Some(outcome);
            }
        }
        let outcome = best.unwrap_or_else(|| {
            forecast_sequence(state, action, response.response, None, horizon_ticks)
        });
        let probability = response.probability.max(0.0) / response_mass;
        expected_utility += outcome.utility() * probability;
        worst_loss = worst_loss.max(outcome.worst_loss);
    }

    debug_assert!(usize::from(evaluated) <= MAX_RESPONSE_BRANCHES * MAX_FORECAST_FOLLOW_UPS);
    let strategic_score = expected_utility - risk_weight * worst_loss + tactic_bias;
    ForecastPlanScore {
        expected_utility,
        worst_loss,
        strategic_score,
        final_score: strategic_score + seeded_jitter,
        outcomes_evaluated: evaluated,
    }
}

fn forecast_sequence(
    state: CombatForecastState,
    action: ForecastAction,
    response: OpponentResponse,
    follow_up: Option<ForecastMoveFacts>,
    horizon_ticks: u32,
) -> ForecastOutcome {
    let mut outcome = forecast_single(state, action.facts, action.tactic, response, horizon_ticks);
    if let Some(follow_up) = follow_up
        && outcome.expected_contact_tick.is_some()
        && u32::from(action.facts.recovery_ticks) < horizon_ticks
    {
        let remaining = horizon_ticks.saturating_sub(u32::from(action.facts.recovery_ticks));
        let follow = forecast_single(state, follow_up, action.tactic, response, remaining);
        outcome.health_swing += follow.health_swing * 0.55;
        outcome.stamina_swing += follow.stamina_swing * 0.55;
        outcome.initiative += follow.initiative * 0.4;
        outcome.arena_position += follow.arena_position * 0.35;
        outcome.edge_safety += follow.edge_safety * 0.35;
        outcome.recovery_exposure += follow.recovery_exposure * 0.4;
        outcome.whiff_risk = (outcome.whiff_risk + follow.whiff_risk * 0.4).clamp(0.0, 1.0);
        outcome.worst_loss = outcome.worst_loss.max(follow.worst_loss * 0.65);
    }
    outcome
}

fn forecast_single(
    state: CombatForecastState,
    facts: ForecastMoveFacts,
    tactic: TacticId,
    response: OpponentResponse,
    horizon_ticks: u32,
) -> ForecastOutcome {
    let distance = state.distance();
    let relative_velocity = state.target_velocity - state.bot_velocity;
    let contact_tick = u32::from(facts.startup_ticks);
    let predicted_distance = (distance
        + relative_velocity.length() * contact_tick as f32 * 0.05 * 0.35
        - facts.forward_motion.max(0.0))
    .max(0.0);
    let in_time = contact_tick <= horizon_ticks;
    let in_range = facts.range > 0.0 && predicted_distance <= facts.range.max(0.1);
    let facing_quality = state
        .bot_facing
        .normalize_or_zero()
        .dot((state.target_position - state.bot_position).normalize_or_zero())
        .clamp(-1.0, 1.0);
    let mut contact_chance = if in_time && in_range && facing_quality > -0.05 {
        (0.82 - predicted_distance / facts.range.max(0.1) * 0.22 + facing_quality.max(0.0) * 0.12)
            .clamp(0.1, 0.95)
    } else {
        0.04
    };
    let mut incoming_loss = 0.0;
    let mut guard_stamina_swing = 0.0;
    let timing_advantage = (f32::from(state.target_action_ticks_remaining)
        - f32::from(state.bot_action_ticks_remaining))
        / 20.0;
    let mut initiative = (20.0 - f32::from(facts.recovery_ticks)) / 20.0 + timing_advantage * 0.35;
    initiative += match state.bot_action_phase {
        ForecastActionPhase::Neutral => 0.1,
        ForecastActionPhase::Startup | ForecastActionPhase::Active => 0.0,
        ForecastActionPhase::Recovery => -0.2,
        ForecastActionPhase::Disabled => -0.6,
    };
    let mut position = (facts.forward_motion / 3.0).clamp(-1.0, 1.0)
        + (state.target_edge_risk - state.bot_edge_risk) * 0.25;

    if !state.target_grounded && tactic != TacticId::AntiAir {
        contact_chance *= 0.45;
    }
    if !state.bot_grounded && facts.vertical_tolerance < 0.8 {
        contact_chance *= 0.7;
    }

    match response {
        OpponentResponse::Attack => {
            contact_chance *= if tactic == TacticId::WhiffPunish {
                1.08
            } else {
                0.85
            };
            incoming_loss = state
                .target_move
                .map_or(5.0, |target| target.estimated_damage * 0.55);
            incoming_loss *= match state.target_action_phase {
                ForecastActionPhase::Startup => 0.85,
                ForecastActionPhase::Active => 1.0,
                ForecastActionPhase::Recovery => 0.3,
                ForecastActionPhase::Neutral => 0.65,
                ForecastActionPhase::Disabled => 0.0,
            };
            let target_facing_quality = state
                .target_facing
                .normalize_or_zero()
                .dot((state.bot_position - state.target_position).normalize_or_zero())
                .clamp(0.0, 1.0);
            incoming_loss *= 0.45 + target_facing_quality * 0.55;
            initiative -= 0.25;
        }
        OpponentResponse::Guard => {
            if facts.action == Some(BotSemanticAction::Grab) {
                contact_chance = (contact_chance + 0.25).clamp(0.0, 1.0);
                initiative += 0.25;
            } else if facts.guardable {
                guard_stamina_swing = facts.estimated_damage.max(4.0) * contact_chance;
                contact_chance *= 0.22;
            }
        }
        OpponentResponse::Grab => {
            incoming_loss = if distance < 1.2 { 8.0 } else { 2.0 };
            initiative -= 0.2;
        }
        OpponentResponse::Jump => {
            if tactic == TacticId::AntiAir && facts.vertical_tolerance >= 0.8 {
                contact_chance = (contact_chance + 0.3).clamp(0.0, 1.0);
                initiative += 0.35;
            } else {
                contact_chance *= 0.42;
            }
        }
        OpponentResponse::Dodge => {
            contact_chance *= 0.35;
            initiative -= 0.15;
        }
        OpponentResponse::Retreat => {
            contact_chance *= if tactic == TacticId::ProjectilePressure {
                0.9
            } else {
                0.48
            };
            position -= 0.2;
        }
        OpponentResponse::Special => {
            incoming_loss = 6.0;
            initiative -= 0.2;
        }
        OpponentResponse::Wait => {
            contact_chance = (contact_chance + 0.08).clamp(0.0, 1.0);
        }
    }

    if tactic == TacticId::EscapePressure {
        incoming_loss *= 0.35;
        contact_chance = 0.0;
        initiative += 0.25;
        position += 0.15;
    } else if tactic == TacticId::BaitAndPunish {
        incoming_loss *= 0.55;
        initiative += matches!(response, OpponentResponse::Attack | OpponentResponse::Grab) as u8
            as f32
            * 0.45;
    }

    let dealt = facts.estimated_damage.max(0.0) * contact_chance;
    let whiff_risk = (1.0 - contact_chance).clamp(0.0, 1.0);
    let recovery_exposure = whiff_risk * f32::from(facts.recovery_ticks) / 20.0;
    let edge_safety = (state.target_edge_risk - state.bot_edge_risk) * contact_chance
        - state.external_threat_cost * 0.25;
    let finishing_bonus = if dealt >= state.target_health.max(0.0) {
        4.0
    } else {
        0.0
    };
    let health_swing = dealt - incoming_loss + finishing_bonus;
    let target_exhaustion_bonus =
        if state.target_stamina < MAX_STAMINA * 0.25 && response == OpponentResponse::Guard {
            guard_stamina_swing * 0.25
        } else {
            0.0
        };
    let stamina_swing = guard_stamina_swing + target_exhaustion_bonus - facts.stamina_cost;
    let low_health_risk = (1.0 - state.bot_health / 100.0).clamp(0.0, 1.0);
    let low_stamina_risk = (1.0 - state.bot_stamina / MAX_STAMINA).clamp(0.0, 1.0);
    let worst_loss = ((incoming_loss - dealt * 0.25).max(0.0) / 20.0
        + recovery_exposure * 0.7
        + state.external_threat_cost * 0.4
        + low_health_risk * incoming_loss / 30.0
        + low_stamina_risk * facts.stamina_cost / MAX_STAMINA)
        .clamp(0.0, 3.0);

    ForecastOutcome {
        health_swing,
        stamina_swing,
        initiative,
        arena_position: position,
        edge_safety,
        expected_contact_tick: (contact_chance >= 0.2).then_some(facts.startup_ticks),
        recovery_exposure,
        whiff_risk,
        worst_loss,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct PlanStep {
    pub(super) enabled: bool,
    pub(super) action: Option<BotSemanticAction>,
    pub(super) expected_action: Option<FighterAction>,
    pub(super) condition: PlanBranch,
    pub(super) movement: PlanMovement,
    pub(super) deadline_ticks: u8,
    pub(super) stamina_cost: f32,
    pub(super) requires_grounded: bool,
}

impl PlanStep {
    pub(super) const fn action(
        action: BotSemanticAction,
        expected_action: Option<FighterAction>,
        condition: PlanBranch,
        deadline_ticks: u8,
    ) -> Self {
        Self {
            enabled: true,
            action: Some(action),
            expected_action,
            condition,
            movement: PlanMovement::Hold,
            deadline_ticks,
            stamina_cost: 0.0,
            requires_grounded: false,
        }
    }

    pub(super) const fn with_requirements(
        mut self,
        stamina_cost: f32,
        requires_grounded: bool,
    ) -> Self {
        self.stamina_cost = stamina_cost;
        self.requires_grounded = requires_grounded;
        self
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct TacticOutcome {
    pub(super) hits: u8,
    pub(super) blocks: u8,
    pub(super) whiffs: u8,
    pub(super) damage_swing: f32,
    pub(super) stamina_swing: f32,
    pub(super) position_change: f32,
    pub(super) initiative_result: f32,
}

impl TacticOutcome {
    pub(super) fn normalized(self) -> f32 {
        (f32::from(self.hits) * 0.35 + f32::from(self.blocks) * 0.12
            - f32::from(self.whiffs) * 0.35
            + self.damage_swing / 30.0
            + self.stamina_swing / MAX_STAMINA * 0.45
            + self.position_change / 6.0
            + self.initiative_result * 0.25)
            .clamp(-1.0, 1.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ActiveTacticPlan {
    pub(super) target_id: usize,
    pub(super) tactic: TacticId,
    pub(super) expected_response: OpponentResponse,
    pub(super) steps: [PlanStep; MAX_PLAN_STEPS],
    pub(super) current_step: u8,
    pub(super) branch: PlanBranch,
    pub(super) deadline_tick: u64,
    pub(super) step_started_tick: u64,
    pub(super) step_committed: bool,
    pub(super) last_outcome_tick: u64,
    pub(super) rejection_count: u8,
    pub(super) origin: Vec3,
    pub(super) target_origin: Vec3,
    pub(super) start_bot_health: f32,
    pub(super) start_target_health: f32,
    pub(super) start_bot_stamina: f32,
    pub(super) start_target_stamina: f32,
    pub(super) accumulated: TacticOutcome,
    pub(super) forecast_score: f32,
    pub(super) learned_bias: f32,
}

impl ActiveTacticPlan {
    pub(super) fn current(self) -> Option<PlanStep> {
        self.steps
            .get(self.current_step as usize)
            .copied()
            .filter(|step| step.enabled)
    }

    pub(super) fn next_matching_step(&self, branch: PlanBranch) -> Option<u8> {
        ((self.current_step as usize + 1)..MAX_PLAN_STEPS)
            .find(|index| {
                let step = self.steps[*index];
                step.enabled && step.condition == branch
            })
            .map(|index| index as u8)
    }
}

pub(super) fn update_tactic_bias(
    biases: &mut [f32; TacticId::COUNT],
    tactic: TacticId,
    outcome: TacticOutcome,
    learning_rate: f32,
    cap: f32,
) -> f32 {
    for bias in biases.iter_mut() {
        *bias *= 0.985;
        if bias.abs() < 0.0001 {
            *bias = 0.0;
        }
    }
    let selected = &mut biases[tactic.index()];
    *selected = (*selected + learning_rate * outcome.normalized()).clamp(-cap, cap);
    *selected
}

pub(super) fn bounded_velocity(velocity: Vec3) -> Vec2 {
    let mut planar = Vec2::new(velocity.x, velocity.z);
    if planar.length() > 12.0 {
        planar = planar.normalize_or_zero() * 12.0;
    }
    planar
}

pub(super) fn response_context(
    distance: f32,
    move_envelope: f32,
    opponent_action: FighterAction,
    edge_risk: f32,
    previous_outcome: PreviousOutcome,
) -> ResponseContext {
    let envelope = move_envelope.max(0.75);
    let range = if distance <= envelope * 0.8 {
        MoveRelativeRange::Inside
    } else if distance <= envelope * 1.25 {
        MoveRelativeRange::Fringe
    } else {
        MoveRelativeRange::Outside
    };
    let opponent_state = if opponent_action == FighterAction::Guarding {
        ResponseOpponentState::Guarding
    } else if is_offensive_action(opponent_action) {
        ResponseOpponentState::Attacking
    } else if matches!(
        opponent_action,
        FighterAction::Hitstun
            | FighterAction::Knockdown
            | FighterAction::LandingRecovery
            | FighterAction::GuardBroken
            | FighterAction::GetUp
    ) {
        ResponseOpponentState::Vulnerable
    } else {
        ResponseOpponentState::Neutral
    };
    ResponseContext {
        range,
        opponent_state,
        edge_pressure: edge_risk > 0.35,
        previous_outcome,
    }
}

pub(super) fn response_family(
    perceived_action: FighterAction,
    opponent_position: Vec3,
    bot_position: Vec3,
    opponent_velocity: Vec2,
) -> OpponentResponse {
    match perceived_action {
        FighterAction::Guarding | FighterAction::GuardCounter => OpponentResponse::Guard,
        FighterAction::GrabStartup | FighterAction::GrabHold | FighterAction::Throwing => {
            OpponentResponse::Grab
        }
        FighterAction::Jumping | FighterAction::JumpAttack | FighterAction::JumpHeavyAttack => {
            OpponentResponse::Jump
        }
        FighterAction::Dashing | FighterAction::RecoveryRoll | FighterAction::GuardStep => {
            OpponentResponse::Dodge
        }
        FighterAction::SpecialCast => OpponentResponse::Special,
        action if is_offensive_action(action) => OpponentResponse::Attack,
        FighterAction::Moving => {
            let away = Vec2::new(
                opponent_position.x - bot_position.x,
                opponent_position.z - bot_position.z,
            )
            .normalize_or_zero();
            if opponent_velocity.dot(away) > 0.75 {
                OpponentResponse::Retreat
            } else {
                OpponentResponse::Wait
            }
        }
        _ => OpponentResponse::Wait,
    }
}

pub(super) fn tactic_for_action(
    action: BotSemanticAction,
    phase: TacticalPhase,
    predicted_response: OpponentResponse,
    target_airborne: bool,
) -> TacticId {
    if matches!(phase, TacticalPhase::Disadvantage | TacticalPhase::WakeUp) {
        return TacticId::EscapePressure;
    }
    if target_airborne && matches!(action, BotSemanticAction::Light | BotSemanticAction::Heavy) {
        return TacticId::AntiAir;
    }
    if matches!(phase, TacticalPhase::Advantage | TacticalPhase::HitConfirm)
        && matches!(action, BotSemanticAction::Light | BotSemanticAction::Heavy)
    {
        return TacticId::PressureString;
    }
    if phase == TacticalPhase::GuardPressure || predicted_response == OpponentResponse::Guard {
        return if action == BotSemanticAction::Grab {
            TacticId::StrikeThrow
        } else if action == BotSemanticAction::Light {
            TacticId::StrikeThrow
        } else {
            TacticId::EdgeControl
        };
    }
    if phase == TacticalPhase::EdgePressure {
        return TacticId::EdgeControl;
    }
    match action {
        BotSemanticAction::SpecialProjectile => TacticId::ProjectilePressure,
        BotSemanticAction::Dash | BotSemanticAction::Jump => TacticId::BaitAndPunish,
        BotSemanticAction::Heavy => TacticId::WhiffPunish,
        _ => TacticId::NeutralPoke,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> ResponseContext {
        ResponseContext {
            range: MoveRelativeRange::Inside,
            opponent_state: ResponseOpponentState::Guarding,
            edge_pressure: false,
            previous_outcome: PreviousOutcome::Guarded,
        }
    }

    #[test]
    fn response_model_starts_with_two_counts_and_updates_only_selected_response() {
        let mut model = OpponentResponseModel::default();
        assert_eq!(model.count(context(), OpponentResponse::Attack), 2.0);
        assert_eq!(model.count(context(), OpponentResponse::Guard), 2.0);
        model.observe(context(), OpponentResponse::Guard);
        assert_eq!(model.count(context(), OpponentResponse::Attack), 2.0);
        assert_eq!(model.count(context(), OpponentResponse::Guard), 3.0);
    }

    #[test]
    fn response_model_decays_by_seven_eighths_every_twenty_ticks() {
        let mut model = OpponentResponseModel::default();
        model.observe(context(), OpponentResponse::Jump);
        model.advance_ticks(19);
        assert_eq!(model.count(context(), OpponentResponse::Jump), 3.0);
        model.advance_ticks(1);
        assert_eq!(
            model.count(context(), OpponentResponse::Jump),
            3.0 * 7.0 / 8.0
        );
        model.advance_ticks(40);
        assert_eq!(
            model.count(context(), OpponentResponse::Jump),
            3.0 * (7.0_f32 / 8.0).powi(3)
        );
    }

    #[test]
    fn top_three_predictions_have_stable_family_order_for_ties() {
        let mut model = OpponentResponseModel::default();
        for _ in 0..4 {
            model.observe(context(), OpponentResponse::Jump);
        }
        for _ in 0..2 {
            model.observe(context(), OpponentResponse::Guard);
        }
        let top = model.top_three(context());
        assert_eq!(top[0].response, OpponentResponse::Jump);
        assert_eq!(top[1].response, OpponentResponse::Guard);
        assert_eq!(top[2].response, OpponentResponse::Attack);
        assert!(top[0].probability > top[1].probability);
    }

    #[test]
    fn phase_classification_prioritizes_survival_and_confirmed_branches() {
        let base = PhaseInputs {
            bot_action: FighterAction::Idle,
            target_action: FighterAction::Idle,
            bot_confirmed_hit: false,
            bot_confirmed_guard: false,
            cancel_window_open: false,
            bot_recovery_ticks: 0,
            target_recovery_ticks: 0,
            stamina_ratio: 1.0,
            distance: 1.0,
            bot_edge_risk: 0.0,
            target_edge_risk: 0.0,
        };
        assert_eq!(classify_phase(base), TacticalPhase::Neutral);
        assert_eq!(
            classify_phase(PhaseInputs {
                bot_action: FighterAction::Knockdown,
                ..base
            }),
            TacticalPhase::WakeUp
        );
        assert_eq!(
            classify_phase(PhaseInputs {
                bot_confirmed_hit: true,
                cancel_window_open: true,
                ..base
            }),
            TacticalPhase::HitConfirm
        );
        assert_eq!(
            classify_phase(PhaseInputs {
                bot_confirmed_hit: true,
                bot_confirmed_guard: true,
                cancel_window_open: true,
                ..base
            }),
            TacticalPhase::GuardPressure
        );
    }

    #[test]
    fn plan_scoring_applies_expected_value_risk_bias_and_jitter() {
        let state = CombatForecastState {
            bot_position: Vec2::ZERO,
            target_position: Vec2::new(1.0, 0.0),
            bot_facing: Vec2::X,
            target_facing: -Vec2::X,
            bot_health: 100.0,
            target_health: 100.0,
            bot_stamina: MAX_STAMINA,
            target_stamina: MAX_STAMINA,
            bot_grounded: true,
            target_grounded: true,
            ..Default::default()
        };
        let facts = ForecastMoveFacts::fallback(BotSemanticAction::Light, 1.0);
        let action = ForecastAction {
            action: BotSemanticAction::Light,
            tactic: TacticId::NeutralPoke,
            facts,
        };
        let responses = [
            ResponsePrediction {
                response: OpponentResponse::Wait,
                probability: 0.6,
            },
            ResponsePrediction {
                response: OpponentResponse::Guard,
                probability: 0.3,
            },
            ResponsePrediction {
                response: OpponentResponse::Attack,
                probability: 0.1,
            },
        ];
        let score = score_forecast_plan(
            state,
            action,
            responses,
            [ForecastFollowUp::default(); MAX_FORECAST_FOLLOW_UPS],
            20,
            0.35,
            0.2,
            0.05,
        );
        assert_eq!(score.final_score, score.strategic_score + 0.05);
        assert!(score.worst_loss >= 0.0);
        assert_eq!(score.outcomes_evaluated, 3);
    }

    #[test]
    fn tactic_bias_updates_are_bounded_and_failed_tactics_fall() {
        let mut biases = [0.0; TacticId::COUNT];
        let failed = TacticOutcome {
            whiffs: 1,
            damage_swing: -12.0,
            initiative_result: -1.0,
            ..Default::default()
        };
        for _ in 0..20 {
            update_tactic_bias(&mut biases, TacticId::BaitAndPunish, failed, 0.12, 0.75);
        }
        assert!(biases[TacticId::BaitAndPunish.index()] < -0.3);
        assert!(biases[TacticId::BaitAndPunish.index()] >= -0.75);
    }

    #[test]
    fn plan_branch_lookup_is_stable() {
        let plan = ActiveTacticPlan {
            target_id: 1,
            tactic: TacticId::StrikeThrow,
            expected_response: OpponentResponse::Guard,
            steps: [
                PlanStep::action(BotSemanticAction::Light, None, PlanBranch::Start, 8),
                PlanStep::action(BotSemanticAction::Grab, None, PlanBranch::Guarded, 8),
                PlanStep::action(BotSemanticAction::Light, None, PlanBranch::Hit, 8),
            ],
            current_step: 0,
            branch: PlanBranch::Pending,
            deadline_tick: 8,
            step_started_tick: 0,
            step_committed: false,
            last_outcome_tick: 0,
            rejection_count: 0,
            origin: Vec3::ZERO,
            target_origin: Vec3::ZERO,
            start_bot_health: 100.0,
            start_target_health: 100.0,
            start_bot_stamina: MAX_STAMINA,
            start_target_stamina: MAX_STAMINA,
            accumulated: TacticOutcome::default(),
            forecast_score: 0.0,
            learned_bias: 0.0,
        };
        assert_eq!(plan.next_matching_step(PlanBranch::Guarded), Some(1));
        assert_eq!(plan.next_matching_step(PlanBranch::Hit), Some(2));
        assert_eq!(plan.next_matching_step(PlanBranch::Whiff), None);
    }

    #[test]
    fn fixed_forecast_bounds_never_exceed_one_hundred_forty_four_outcomes() {
        assert_eq!(
            MAX_TACTICAL_ACTIONS * MAX_RESPONSE_BRANCHES * MAX_FORECAST_FOLLOW_UPS,
            144
        );
    }

    #[test]
    fn plan_invalidation_covers_target_safety_resources_displacement_and_rejection() {
        let valid = PlanInvalidationInputs {
            target_valid: true,
            incapacitated: false,
            unsafe_ground: false,
            stamina: 20.0,
            required_stamina: 10.0,
            displacement: 1.0,
            displacement_limit: 6.0,
            rejection_count: 0,
        };
        assert!(!plan_is_invalid(valid));
        for invalid in [
            PlanInvalidationInputs {
                target_valid: false,
                ..valid
            },
            PlanInvalidationInputs {
                incapacitated: true,
                ..valid
            },
            PlanInvalidationInputs {
                unsafe_ground: true,
                ..valid
            },
            PlanInvalidationInputs {
                stamina: 9.0,
                ..valid
            },
            PlanInvalidationInputs {
                displacement: 6.01,
                ..valid
            },
            PlanInvalidationInputs {
                rejection_count: 2,
                ..valid
            },
        ] {
            assert!(plan_is_invalid(invalid));
        }
    }

    #[test]
    fn response_storage_is_fixed_for_every_fighter() {
        let models: [OpponentResponseModel; FIGHTER_COUNT] =
            std::array::from_fn(|_| OpponentResponseModel::default());
        assert_eq!(models.len(), FIGHTER_COUNT);
    }
}
