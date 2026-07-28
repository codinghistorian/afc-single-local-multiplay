use bevy::prelude::*;
use bevy::time::Real;
use bevy::ui::UiTargetCamera;

use crate::camera::UiCamera;
use crate::game_state::{GameplayPauseOwner, GameplayPauseOwners, MatchPhase, MatchState};
use crate::tutorial::{TutorialPhase, TutorialSession, TutorialTransitionAction};
use crate::user_mode::{UserModeGameplayScene, UserModeTransitionAction};

pub(crate) const GAME_FADE_OUT_SECONDS: f32 = 0.18;
pub(crate) const GAME_FADE_HOLD_SECONDS: f32 = 0.06;
pub(crate) const GAME_FADE_IN_SECONDS: f32 = 0.26;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GameTransitionAction {
    UserMode(UserModeTransitionAction),
    Tutorial(TutorialTransitionAction),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum GameTransitionReveal {
    #[default]
    Immediate,
    BattleReady,
    TutorialLessonReady,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum GameTransitionStage {
    #[default]
    Idle,
    FadingOut,
    Covered,
    FadingIn,
}

#[derive(Resource, Clone, Debug)]
pub struct GameTransition {
    stage: GameTransitionStage,
    elapsed: f32,
    action: Option<GameTransitionAction>,
    reveal: GameTransitionReveal,
    committed: bool,
}

impl Default for GameTransition {
    fn default() -> Self {
        Self {
            stage: GameTransitionStage::Idle,
            elapsed: 0.0,
            action: None,
            reveal: GameTransitionReveal::Immediate,
            committed: false,
        }
    }
}

impl GameTransition {
    pub(crate) fn active(&self) -> bool {
        self.stage != GameTransitionStage::Idle
    }

    pub(crate) fn request(
        &mut self,
        action: GameTransitionAction,
        reveal: GameTransitionReveal,
    ) -> bool {
        if self.active() {
            return false;
        }
        self.stage = GameTransitionStage::FadingOut;
        self.elapsed = 0.0;
        self.action = Some(action);
        self.reveal = reveal;
        self.committed = false;
        true
    }

    pub(crate) fn pending_action(&self) -> Option<&GameTransitionAction> {
        (self.stage == GameTransitionStage::Covered && !self.committed)
            .then_some(self.action.as_ref())
            .flatten()
    }

    #[cfg(test)]
    pub(crate) fn action(&self) -> Option<&GameTransitionAction> {
        self.action.as_ref()
    }

    pub(crate) fn targets_tutorial(&self) -> bool {
        matches!(self.action, Some(GameTransitionAction::Tutorial(_)))
    }

    pub(crate) fn mark_committed(&mut self) {
        debug_assert_eq!(self.stage, GameTransitionStage::Covered);
        debug_assert!(!self.committed);
        self.committed = true;
    }

    fn advance(&mut self, delta_seconds: f32, reveal_ready: bool) -> bool {
        let delta_seconds = delta_seconds.max(0.0);
        match self.stage {
            GameTransitionStage::Idle => false,
            GameTransitionStage::FadingOut => {
                self.elapsed = (self.elapsed + delta_seconds).min(GAME_FADE_OUT_SECONDS);
                if self.elapsed >= GAME_FADE_OUT_SECONDS {
                    self.stage = GameTransitionStage::Covered;
                    self.elapsed = 0.0;
                }
                false
            }
            GameTransitionStage::Covered => {
                if !self.committed || !reveal_ready {
                    return false;
                }
                self.elapsed = (self.elapsed + delta_seconds).min(GAME_FADE_HOLD_SECONDS);
                if self.elapsed >= GAME_FADE_HOLD_SECONDS {
                    self.stage = GameTransitionStage::FadingIn;
                    self.elapsed = 0.0;
                }
                false
            }
            GameTransitionStage::FadingIn => {
                self.elapsed = (self.elapsed + delta_seconds).min(GAME_FADE_IN_SECONDS);
                if self.elapsed < GAME_FADE_IN_SECONDS {
                    return false;
                }
                *self = Self::default();
                true
            }
        }
    }

    pub(crate) fn alpha(&self) -> f32 {
        match self.stage {
            GameTransitionStage::Idle => 0.0,
            GameTransitionStage::FadingOut => {
                game_transition_ease(self.elapsed / GAME_FADE_OUT_SECONDS)
            }
            GameTransitionStage::Covered => 1.0,
            GameTransitionStage::FadingIn => {
                1.0 - game_transition_ease(self.elapsed / GAME_FADE_IN_SECONDS)
            }
        }
    }
}

fn game_transition_ease(amount: f32) -> f32 {
    let amount = amount.clamp(0.0, 1.0);
    amount * amount * (3.0 - 2.0 * amount)
}

pub(crate) fn request_game_transition(
    transition: &mut GameTransition,
    pause_owners: &mut GameplayPauseOwners,
    action: GameTransitionAction,
    reveal: GameTransitionReveal,
) -> bool {
    let started = transition.request(action, reveal);
    if started {
        pause_owners.set(GameplayPauseOwner::GameTransition, true);
    }
    started
}

pub fn game_transition_active(transition: Res<GameTransition>) -> bool {
    transition.active()
}

pub fn advance_game_transition(
    time: Res<Time<Real>>,
    state: Res<MatchState>,
    gameplay_scene: Res<UserModeGameplayScene>,
    tutorial_session: Res<TutorialSession>,
    mut transition: ResMut<GameTransition>,
    mut pause_owners: ResMut<GameplayPauseOwners>,
) {
    let reveal_ready = match transition.reveal {
        GameTransitionReveal::Immediate => true,
        GameTransitionReveal::BattleReady => {
            gameplay_scene.ready_for_battle() && state.phase == MatchPhase::Fighting
        }
        GameTransitionReveal::TutorialLessonReady => {
            tutorial_session.phase == TutorialPhase::Prompt
        }
    };
    if transition.advance(time.delta_secs(), reveal_ready) {
        pause_owners.set(GameplayPauseOwner::GameTransition, false);
    }
}

#[derive(Component)]
pub struct GameTransitionOverlay;

pub fn setup_game_transition_overlay(
    mut commands: Commands,
    ui_cameras: Query<Entity, With<UiCamera>>,
) {
    let mut overlay = commands.spawn((
        GameTransitionOverlay,
        Name::new("Game transition overlay"),
        Node {
            display: Display::None,
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        GlobalZIndex(10_000),
        BackgroundColor(Color::srgba(0.006, 0.006, 0.012, 0.0)),
        Pickable {
            should_block_lower: true,
            is_hoverable: false,
        },
    ));
    if let Some(camera) = ui_cameras.iter().next() {
        overlay.insert(UiTargetCamera(camera));
    }
}

pub fn update_game_transition_overlay(
    transition: Res<GameTransition>,
    mut overlays: Query<(&mut Node, &mut BackgroundColor), With<GameTransitionOverlay>>,
) {
    let alpha = transition.alpha();
    for (mut node, mut background) in &mut overlays {
        node.display = if transition.active() {
            Display::Flex
        } else {
            Display::None
        };
        *background = BackgroundColor(Color::srgba(0.006, 0.006, 0.012, alpha));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tutorial_action() -> GameTransitionAction {
        GameTransitionAction::Tutorial(TutorialTransitionAction::BeginPlaying)
    }

    #[test]
    fn timing_easing_and_covered_commit_are_stable() {
        let mut transition = GameTransition::default();
        assert!(transition.request(tutorial_action(), GameTransitionReveal::Immediate));
        assert_eq!(transition.alpha(), 0.0);

        transition.advance(GAME_FADE_OUT_SECONDS * 0.5, true);
        assert!((transition.alpha() - 0.5).abs() < 0.001);
        assert!(transition.pending_action().is_none());

        transition.advance(GAME_FADE_OUT_SECONDS * 0.5, true);
        assert_eq!(transition.alpha(), 1.0);
        assert_eq!(transition.pending_action(), Some(&tutorial_action()));

        transition.mark_committed();
        transition.advance(GAME_FADE_HOLD_SECONDS, true);
        transition.advance(GAME_FADE_IN_SECONDS * 0.5, true);
        assert!((transition.alpha() - 0.5).abs() < 0.001);
        assert!(transition.advance(GAME_FADE_IN_SECONDS * 0.5, true));
        assert!(!transition.active());
        assert_eq!(transition.alpha(), 0.0);
    }

    #[test]
    fn duplicate_requests_are_rejected_and_reveal_waits_for_readiness() {
        let mut transition = GameTransition::default();
        assert!(transition.request(tutorial_action(), GameTransitionReveal::BattleReady));
        assert!(!transition.request(tutorial_action(), GameTransitionReveal::Immediate));

        transition.advance(GAME_FADE_OUT_SECONDS, false);
        transition.mark_committed();
        transition.advance(GAME_FADE_HOLD_SECONDS * 2.0, false);
        assert_eq!(transition.alpha(), 1.0);

        transition.advance(GAME_FADE_HOLD_SECONDS, true);
        assert_eq!(transition.stage, GameTransitionStage::FadingIn);
    }

    #[test]
    fn covered_action_can_only_be_committed_once() {
        let mut transition = GameTransition::default();
        transition.request(tutorial_action(), GameTransitionReveal::Immediate);
        transition.advance(GAME_FADE_OUT_SECONDS, true);
        assert!(transition.pending_action().is_some());
        transition.mark_committed();
        assert!(transition.pending_action().is_none());
    }

    #[test]
    fn battle_reveal_stays_covered_and_paused_until_fighting_is_ready() {
        let mut transition = GameTransition::default();
        let mut owners = GameplayPauseOwners::default();
        request_game_transition(
            &mut transition,
            &mut owners,
            tutorial_action(),
            GameTransitionReveal::BattleReady,
        );

        let mut app = App::new();
        app.insert_resource(transition)
            .insert_resource(owners)
            .insert_resource(MatchState::default())
            .insert_resource(UserModeGameplayScene::default())
            .insert_resource(TutorialSession::default())
            .insert_resource(Time::<Real>::default())
            .add_systems(Update, advance_game_transition);

        app.world_mut()
            .resource_mut::<Time<Real>>()
            .advance_by(Duration::from_secs_f32(GAME_FADE_OUT_SECONDS));
        app.update();
        assert_eq!(app.world().resource::<GameTransition>().alpha(), 1.0);
        app.world_mut()
            .resource_mut::<GameTransition>()
            .mark_committed();

        app.world_mut()
            .resource_mut::<Time<Real>>()
            .advance_by(Duration::from_secs_f32(GAME_FADE_HOLD_SECONDS * 2.0));
        app.update();
        assert_eq!(app.world().resource::<GameTransition>().alpha(), 1.0);
        assert!(
            app.world()
                .resource::<GameplayPauseOwners>()
                .contains(GameplayPauseOwner::GameTransition)
        );

        app.world_mut().resource_mut::<MatchState>().phase = MatchPhase::Fighting;
        app.world_mut()
            .resource_mut::<Time<Real>>()
            .advance_by(Duration::from_secs_f32(GAME_FADE_HOLD_SECONDS));
        app.update();
        assert_eq!(app.world().resource::<GameTransition>().alpha(), 1.0);

        app.world_mut()
            .resource_mut::<Time<Real>>()
            .advance_by(Duration::from_secs_f32(GAME_FADE_IN_SECONDS));
        app.update();
        assert!(!app.world().resource::<GameTransition>().active());
        assert!(
            !app.world()
                .resource::<GameplayPauseOwners>()
                .contains(GameplayPauseOwner::GameTransition)
        );
    }
}
