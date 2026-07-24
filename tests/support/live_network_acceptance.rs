//! Four-peer production-live multiplayer acceptance matrix.
//!
//! This file is included as a `cfg(test)` child of `listen_authority`. The
//! shipping workers remain absolute real-time workers; these scenarios use
//! their test-only manual clocks so every link, client protocol, rollback world,
//! and authority pump is exercised in one stable deterministic service order.

use bevy::asset::{AssetServer, Assets};
use bevy::audio::AudioSource;
use bevy::prelude::{Camera, Image, Mesh, Window};

use crate::authority::AuthorityTickReport;
use crate::authority_input::{AuthorityInputConfig, AuthorityInputOrigin};
use crate::authority_peer_hub::{
    AuthorityAdvanceOutcome, AuthorityPeerHub, AuthorityPeerHubConfig, AuthorityPeerPhase,
};
use crate::headless::{HeadlessMatchConfig, build_headless_simulation};
use crate::listen_authority::ListenAuthenticatedRoster;
use crate::live_authority::LiveSimulationDriver;
use crate::network_io::{
    DEFAULT_FAULT_QUEUE_PACKETS, DeterministicNetworkLab, FaultConfig, FaultLabConfig,
    FaultLabEndpoint,
};
use crate::network_lab::{
    DOWNSTREAM_BUDGET_BYTES_PER_SECOND, UPSTREAM_BUDGET_BYTES_PER_SECOND, average_bytes_per_second,
};
use crate::network_protocol::{
    DefinitionId, InputButtons, InputFrame, InputSequence, MatchId, PeerId, QuantizedAxis,
    ReconnectClaim, SeatId, SeatOwner, SimTick, StateHash, TeamId,
};
use crate::network_quality::NetworkQualitySample;
use crate::network_runtime::{MAX_RUNTIME_QUEUE_MESSAGES, RuntimeMetrics};
use crate::online_roster::{
    OnlineManifestOptions, OnlineRoster, OnlineRosterMember, OnlineSeatSelection,
};
use crate::reconnect::{AuthenticatedPeer, AuthenticatedUserId};
use crate::remote_online_client::{
    RemoteCommandSubmitOutcome, RemoteLocalInputBatch, RemoteLocalInputSample, RemoteOnlineClient,
    RemoteOnlineClientConfig, RemoteOnlineClientPhase, RemoteOnlineTerminal,
};

const PEER_COUNT: usize = 4;
const STARTUP_LIMIT: usize = 4_096;
const RESULT_LIMIT: usize = 4_096;
const CLIENT_COMMAND_CAPACITY: usize = 8;
const COUNTDOWN_LEAD_TICKS: u32 = 24;
const LOOPBACK_DISTINCT_INPUT_TICKS: u64 = 180;
const LOOPBACK_INPUT_TRANSITION_END_TICK: u64 = 200;

const MOVEMENT_X: [i8; PEER_COUNT] = [96, 48, -48, -96];
const MOVEMENT_Y: [i8; PEER_COUNT] = [12, 24, 36, 48];
const ACTION_BUTTONS: [u16; PEER_COUNT] = [
    InputButtons::LIGHT,
    InputButtons::HEAVY,
    InputButtons::SPECIAL,
    InputButtons::DASH,
];
const QUALITY_RTT_MS: [u16; PEER_COUNT] = [24, 48, 72, 96];
const QUALITY_LOSS_BPS: [u16; PEER_COUNT] = [0, 25, 50, 75];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LiveMatrixScenario {
    Loopback4,
    Typical4,
    Degraded4,
    RollbackStorm4,
    ReconnectOneOfFour,
}

impl LiveMatrixScenario {
    const fn match_byte(self) -> u8 {
        match self {
            Self::Loopback4 => 0x11,
            Self::Typical4 => 0x22,
            Self::Degraded4 => 0x33,
            Self::RollbackStorm4 => 0x44,
            Self::ReconnectOneOfFour => 0x55,
        }
    }

    const fn active_ticks(self) -> usize {
        match self {
            Self::Loopback4 => 10 * 60 * 60,
            Self::Typical4 => 600,
            Self::Degraded4 => 600,
            Self::RollbackStorm4 => 360,
            Self::ReconnectOneOfFour => 180,
        }
    }

    fn fault_config(self, index: usize) -> FaultLabConfig {
        let seed = 0xAFC0_4E45_5400_0000_u64 ^ (u64::from(self.match_byte()) << 24) ^ index as u64;
        match self {
            Self::Loopback4 | Self::ReconnectOneOfFour => {
                FaultLabConfig::symmetric(FaultConfig::default(), seed)
            }
            Self::Typical4 => FaultLabConfig::net_typical_60hz(seed),
            Self::Degraded4 => FaultLabConfig::net_degraded_60hz(seed),
            Self::RollbackStorm4 => FaultLabConfig::new(
                FaultConfig {
                    base_latency_ticks: 1,
                    ..FaultConfig::default()
                },
                FaultConfig {
                    base_latency_ticks: 9,
                    jitter_ticks: 1,
                    reorder_per_10k: 500,
                    max_reorder_extra_ticks: 1,
                    ..FaultConfig::default()
                },
                seed ^ 0xA2B0,
                seed ^ 0xB2A0,
            ),
        }
    }
}

fn peer(value: u64) -> PeerId {
    PeerId::new(value).unwrap()
}

fn user_for(peer_id: PeerId) -> AuthenticatedUserId {
    AuthenticatedUserId::new(90_000 + peer_id.get()).unwrap()
}

fn canonical_peers() -> [PeerId; PEER_COUNT] {
    // Listen manifests place the host first, then sort remaining peers by id.
    [peer(900), peer(110), peer(420), peer(730)]
}

fn declaration_order() -> [PeerId; PEER_COUNT] {
    // Deliberately neither host-first nor PeerId-sorted.
    [peer(730), peer(420), peer(900), peer(110)]
}

fn authenticated(peer_id: PeerId) -> AuthenticatedPeer {
    AuthenticatedPeer {
        peer_id,
        user_id: user_for(peer_id),
    }
}

fn seat_selection(canonical_index: usize) -> OnlineSeatSelection {
    OnlineSeatSelection {
        team: TeamId::new((canonical_index % 2) as u8).unwrap(),
        character: DefinitionId::new(canonical_index as u16).unwrap(),
        style: DefinitionId::new((canonical_index % 3) as u16).unwrap(),
        equipment: DefinitionId::new(canonical_index as u16).unwrap(),
    }
}

fn build_four_peer_config(
    scenario: LiveMatrixScenario,
) -> (HeadlessMatchConfig, ListenAuthenticatedRoster) {
    let canonical = canonical_peers();
    let declared = declaration_order();
    let mut roster = OnlineRoster::default();
    for peer_id in declared {
        let canonical_index = canonical
            .iter()
            .position(|candidate| *candidate == peer_id)
            .unwrap();
        roster
            .upsert(
                OnlineRosterMember::new(
                    peer_id,
                    user_for(peer_id),
                    1,
                    true,
                    &[seat_selection(canonical_index)],
                )
                .unwrap(),
            )
            .unwrap();
    }

    let mut options = OnlineManifestOptions::casual_listen(
        MatchId::new([scenario.match_byte(); 16]).unwrap(),
        canonical[0],
        DefinitionId::new(0).unwrap(),
        DefinitionId::new(1).unwrap(),
        0xAFC0_2026_4E45_5400 ^ u64::from(scenario.match_byte()),
        SimTick(2),
    );
    match scenario {
        LiveMatrixScenario::Typical4 => {
            // Include the three-to-four-tick upstream path plus one scheduling
            // tick so jitter/loss does not turn most action edges into
            // authority substitutions.
            options.input_delay_ticks = 5;
        }
        LiveMatrixScenario::Degraded4 => {
            // Use the protocol-supported maximum for the four-to-seven-tick
            // upstream degraded path. The default two-tick setting is the
            // low-latency starting point, not a valid choice for this profile.
            options.input_delay_ticks = 6;
        }
        LiveMatrixScenario::RollbackStorm4 => {
            // The RTT-midpoint clock estimator is conservatively biased behind
            // the authority on this deliberately asymmetric one-up/nine-down
            // link. Six ticks keep upstream input ahead while retaining the
            // intended near-limit downstream correction depth.
            options.input_delay_ticks = 6;
        }
        _ => {}
    }
    let config = roster
        .build_headless_config(options, SimTick::ZERO)
        .unwrap();

    let assignments = config.manifest.ownership.as_slice();
    assert_eq!(assignments.len(), PEER_COUNT);
    for (index, assignment) in assignments.iter().enumerate() {
        assert_eq!(assignment.seat, SeatId::new(index as u8).unwrap());
        assert_eq!(assignment.owner, SeatOwner::Peer(canonical[index]));
        assert_eq!(
            config.manifest.slots[index].character,
            DefinitionId::new(index as u16).unwrap()
        );
    }

    let identities = declared.map(authenticated);
    let authenticated_roster =
        ListenAuthenticatedRoster::new(&config, authenticated(canonical[0]), identities).unwrap();
    assert_eq!(authenticated_roster.len(), PEER_COUNT);
    assert_eq!(authenticated_roster.as_slice(), &identities);

    (config, authenticated_roster)
}

fn hub_config() -> AuthorityPeerHubConfig {
    AuthorityPeerHubConfig {
        countdown_lead_ticks: COUNTDOWN_LEAD_TICKS,
        ..AuthorityPeerHubConfig::default()
    }
}

fn client_config() -> RemoteOnlineClientConfig {
    RemoteOnlineClientConfig {
        command_capacity: CLIENT_COMMAND_CAPACITY,
        ..RemoteOnlineClientConfig::default()
    }
}

fn neutral_disconnected(_peer: PeerId, seat: SeatId, tick: SimTick) -> InputFrame {
    InputFrame {
        tick,
        seat,
        sequence: InputSequence(tick.get() as u16),
        ..InputFrame::default()
    }
}

fn local_input(
    index: usize,
    distinct_gameplay: bool,
    finishing: bool,
    finishing_axes: (i8, i8),
    press_action: bool,
) -> RemoteLocalInputBatch {
    let action = ACTION_BUTTONS[index];
    let pressed = if distinct_gameplay && !finishing && press_action {
        action
    } else {
        0
    };
    let movement_x = if finishing {
        finishing_axes.0
    } else if distinct_gameplay {
        MOVEMENT_X[index]
    } else {
        0
    };
    RemoteLocalInputBatch::new(&[RemoteLocalInputSample {
        seat: SeatId::new(index as u8).unwrap(),
        movement_x: QuantizedAxis::new(movement_x).unwrap(),
        movement_y: QuantizedAxis::new(if finishing {
            finishing_axes.1
        } else if distinct_gameplay {
            MOVEMENT_Y[index]
        } else {
            0
        })
        .unwrap(),
        held_buttons: InputButtons::new(if distinct_gameplay && !finishing {
            action
        } else {
            0
        })
        .unwrap(),
        pressed_buttons: InputButtons::new(pressed).unwrap(),
        ..RemoteLocalInputSample::default()
    }])
    .unwrap()
}

fn quality_sample(index: usize) -> NetworkQualitySample {
    NetworkQualitySample {
        rtt_ms: QUALITY_RTT_MS[index],
        loss_bps: QUALITY_LOSS_BPS[index],
    }
}

struct LiveAcceptanceHarness {
    scenario: LiveMatrixScenario,
    config: HeadlessMatchConfig,
    client_config: RemoteOnlineClientConfig,
    clients: Vec<RemoteOnlineClient>,
    hub: AuthorityPeerHub<LiveSimulationDriver, FaultLabEndpoint>,
    shadow: LiveSimulationDriver,
    labs: Vec<DeterministicNetworkLab>,
    network_tick: SimTick,
    exact_actions_observed: [bool; PEER_COUNT],
    peer_origin_counts: [u64; PEER_COUNT],
    missing_substitute_counts: [u64; PEER_COUNT],
    active_missing_substitute_counts: Option<[u64; PEER_COUNT]>,
    disconnected_bot_counts: [u64; PEER_COUNT],
    reconnect_target_detached: bool,
    reconnect_target_resumed_peer_input: bool,
    finishing_started_at: Option<SimTick>,
    last_authority_runtime: [Option<RuntimeMetrics>; PEER_COUNT],
    traffic_started_at: Option<SimTick>,
    client_sent_counter: [u64; PEER_COUNT],
    authority_sent_counter: [u64; PEER_COUNT],
    client_sent_during_fight: [u64; PEER_COUNT],
    authority_sent_during_fight: [u64; PEER_COUNT],
}

impl LiveAcceptanceHarness {
    fn finishing_axes(&self, seat_index: usize) -> (i8, i8) {
        let assignment = self
            .config
            .manifest
            .ownership
            .assignment_for_seat(SeatId::new(seat_index as u8).unwrap())
            .expect("four-peer fixture has one fighter per seat");
        let tick = self.hub.authority().simulation().current_sim_tick();
        let snapshot = self
            .hub
            .authority()
            .snapshot_at(tick)
            .expect("authority retains its current canonical snapshot");
        let position = snapshot.fighters[assignment.fighter.index()].pose.position;
        let maximum = position.x.unsigned_abs().max(position.z.unsigned_abs());
        if maximum == 0 {
            return if seat_index % 2 == 0 {
                (-127, 0)
            } else {
                (127, 0)
            };
        }
        let scale = i64::from(maximum);
        let x = (i64::from(position.x) * 127 / scale).clamp(-127, 127) as i8;
        let z = (i64::from(position.z) * 127 / scale).clamp(-127, 127) as i8;
        (x, z)
    }

    fn new(scenario: LiveMatrixScenario) -> Self {
        let (config, authenticated_roster) = build_four_peer_config(scenario);
        let simulation = build_headless_simulation(config.clone()).unwrap();
        let shadow = build_headless_simulation(config.clone()).unwrap();
        let mut hub = AuthorityPeerHub::new(
            config.manifest,
            simulation,
            AuthorityInputConfig::default(),
            authenticated_roster.as_slice(),
            hub_config(),
        )
        .unwrap();
        let client_config = client_config();
        let mut clients = Vec::with_capacity(PEER_COUNT);
        let mut labs = Vec::with_capacity(PEER_COUNT);
        for (index, peer_id) in canonical_peers().into_iter().enumerate() {
            let (lab, client_endpoint, authority_endpoint) =
                DeterministicNetworkLab::pair(scenario.fault_config(index)).unwrap();
            hub.attach_initial(peer_id, user_for(peer_id), authority_endpoint)
                .unwrap();
            let client = RemoteOnlineClient::spawn_manual(
                client_endpoint,
                config.clone(),
                peer_id,
                client_config,
            )
            .unwrap();
            client.mark_content_loaded();
            labs.push(lab);
            clients.push(client);
        }
        assert_eq!(clients.len(), PEER_COUNT);
        assert_eq!(labs.len(), PEER_COUNT);

        Self {
            scenario,
            config,
            client_config,
            clients,
            hub,
            shadow,
            labs,
            network_tick: SimTick::ZERO,
            exact_actions_observed: [false; PEER_COUNT],
            peer_origin_counts: [0; PEER_COUNT],
            missing_substitute_counts: [0; PEER_COUNT],
            active_missing_substitute_counts: None,
            disconnected_bot_counts: [0; PEER_COUNT],
            reconnect_target_detached: false,
            reconnect_target_resumed_peer_input: false,
            finishing_started_at: None,
            last_authority_runtime: [None; PEER_COUNT],
            traffic_started_at: None,
            client_sent_counter: [0; PEER_COUNT],
            authority_sent_counter: [0; PEER_COUNT],
            client_sent_during_fight: [0; PEER_COUNT],
            authority_sent_during_fight: [0; PEER_COUNT],
        }
    }

    fn assert_headless_authority(&mut self) {
        let world = self.hub.authority_mut().simulation_mut().world_mut();
        assert!(!world.contains_resource::<AssetServer>());
        assert!(!world.contains_resource::<Assets<Image>>());
        assert!(!world.contains_resource::<Assets<Mesh>>());
        assert!(!world.contains_resource::<Assets<AudioSource>>());
        let mut windows = world.query::<&Window>();
        assert!(windows.iter(world).next().is_none());
        let mut cameras = world.query::<&Camera>();
        assert!(cameras.iter(world).next().is_none());
    }

    fn submit_distinct_client_observations(&self) {
        let simulation_tick = self.hub.authority().simulation().current_sim_tick();
        let finishing = self.finishing_started_at.is_some();
        let distinct_gameplay = self.scenario != LiveMatrixScenario::Loopback4
            || simulation_tick.get() < LOOPBACK_DISTINCT_INPUT_TICKS;
        // RollbackStorm deliberately keeps one semantic edge in flight. Repeating
        // a fresh edge on every early tick turns the scenario into a sequence of
        // unrelated late semantic changes, so the oldest redundant record can
        // exceed the documented 12-tick correction window even though ordinary
        // held input is already recoverable from the relay tail. The lossy
        // profiles retain the bounded early pulse window so at least one edge is
        // committed despite their expected authority-side substitutions.
        let press_action = if self.scenario == LiveMatrixScenario::RollbackStorm4 {
            simulation_tick == SimTick::ZERO
        } else {
            simulation_tick.get() < 16
        };
        for (index, client) in self.clients.iter().enumerate() {
            if client.terminal().is_some() {
                continue;
            }
            assert_eq!(
                client.submit_inputs(local_input(
                    index,
                    distinct_gameplay,
                    finishing,
                    self.finishing_axes(index),
                    press_action
                )),
                RemoteCommandSubmitOutcome::Queued
            );
            assert_eq!(
                client.submit_quality_sample(quality_sample(index)).unwrap(),
                RemoteCommandSubmitOutcome::Queued
            );
        }
    }

    fn validate_committed_inputs(&mut self, report: &AuthorityTickReport) {
        assert_eq!(report.committed_inputs.len(), PEER_COUNT);
        for record in report.committed_inputs.iter() {
            let seat_index = usize::from(record.frame.seat.get());
            let assignment = self
                .config
                .manifest
                .ownership
                .assignment_for_seat(record.frame.seat)
                .unwrap();
            let SeatOwner::Peer(expected_peer) = assignment.owner else {
                panic!("four-peer fixture contains a non-peer seat");
            };
            assert_eq!(record.fighter, assignment.fighter);
            match record.origin {
                AuthorityInputOrigin::Peer(origin) => {
                    assert_eq!(
                        origin, expected_peer,
                        "seat {} accepted input from the wrong authenticated peer",
                        seat_index
                    );
                    let finishing_transition_end = self
                        .finishing_started_at
                        .map(|started| SimTick(started.get().saturating_add(20)));
                    let finishing = finishing_transition_end
                        .is_some_and(|transition_end| record.frame.tick > transition_end);
                    let finishing_transition = self.finishing_started_at.is_some()
                        && !finishing
                        && record.frame.tick > self.finishing_started_at.expect("checked above");
                    let neutral_soak = !finishing
                        && !finishing_transition
                        && self.scenario == LiveMatrixScenario::Loopback4
                        && record.frame.tick.get() > LOOPBACK_INPUT_TRANSITION_END_TICK;
                    let transition = self.scenario == LiveMatrixScenario::Loopback4
                        && record.frame.tick.get() > LOOPBACK_DISTINCT_INPUT_TICKS
                        && !neutral_soak;
                    if finishing {
                        assert_eq!(
                            record
                                .frame
                                .movement_x
                                .get()
                                .unsigned_abs()
                                .max(record.frame.movement_y.get().unsigned_abs()),
                            127,
                            "finishing input must remain a full-strength radial direction"
                        );
                        assert_eq!(record.frame.held_buttons.bits(), 0);
                        assert_eq!(record.frame.pressed_buttons.bits(), 0);
                    } else if finishing_transition {
                        assert!(
                            record.frame.movement_x.get() == MOVEMENT_X[seat_index]
                                || record.frame.movement_x.get() == 0
                                || record.frame.movement_x.get().unsigned_abs() == 127
                                || record.frame.movement_y.get().unsigned_abs() == 127
                        );
                    } else if neutral_soak {
                        assert_eq!(record.frame.movement_x.get(), 0);
                        assert_eq!(record.frame.movement_y.get(), 0);
                        assert_eq!(record.frame.held_buttons.bits(), 0);
                        assert_eq!(record.frame.pressed_buttons.bits(), 0);
                    } else if transition
                        && record.frame.movement_x.get() == 0
                        && record.frame.movement_y.get() == 0
                        && record.frame.held_buttons.bits() == 0
                    {
                        assert_eq!(record.frame.pressed_buttons.bits(), 0);
                    } else {
                        assert_eq!(record.frame.movement_x.get(), MOVEMENT_X[seat_index]);
                        assert_eq!(record.frame.movement_y.get(), MOVEMENT_Y[seat_index]);
                        assert_eq!(record.frame.held_buttons.bits(), ACTION_BUTTONS[seat_index]);
                        let pressed = record.frame.pressed_buttons.bits();
                        assert!(
                            pressed == 0 || pressed == ACTION_BUTTONS[seat_index],
                            "seat {seat_index} committed an unexpected edge pattern"
                        );
                    }
                    assert_eq!(record.frame.released_buttons.bits(), 0);
                    self.exact_actions_observed[seat_index] |=
                        record.frame.pressed_buttons.bits() == ACTION_BUTTONS[seat_index];
                    self.peer_origin_counts[seat_index] =
                        self.peer_origin_counts[seat_index].saturating_add(1);
                    if self.scenario == LiveMatrixScenario::ReconnectOneOfFour
                        && seat_index == 2
                        && !self.reconnect_target_detached
                        && self.clients[seat_index].generation() == 2
                    {
                        self.reconnect_target_resumed_peer_input = true;
                    }
                }
                AuthorityInputOrigin::DisconnectedBot(origin) => {
                    assert_eq!(origin, expected_peer);
                    assert_eq!(self.scenario, LiveMatrixScenario::ReconnectOneOfFour);
                    assert_eq!(seat_index, 2);
                    assert!(self.reconnect_target_detached);
                    self.disconnected_bot_counts[seat_index] =
                        self.disconnected_bot_counts[seat_index].saturating_add(1);
                }
                AuthorityInputOrigin::MissingSubstitute => {
                    assert!(
                        matches!(
                            self.scenario,
                            LiveMatrixScenario::Typical4 | LiveMatrixScenario::Degraded4
                        ) || (self.scenario == LiveMatrixScenario::ReconnectOneOfFour
                            && seat_index == 2
                            && self.reconnect_target_detached),
                        "scenario {:?} accepted an unbounded missing substitute for seat {} at authority tick {} (frame tick {}), client={:?}",
                        self.scenario,
                        seat_index,
                        report.tick.get(),
                        record.frame.tick.get(),
                        self.clients[seat_index].status()
                    );
                    self.missing_substitute_counts[seat_index] =
                        self.missing_substitute_counts[seat_index].saturating_add(1);
                }
                AuthorityInputOrigin::AuthorityBot => {
                    panic!("peer-owned seat was committed as an authority bot")
                }
            }
        }
    }

    fn step_shadow(&mut self, report: &AuthorityTickReport) {
        self.shadow
            .step_committed(&report.committed_inputs)
            .unwrap();
        assert_eq!(self.shadow.current_sim_tick(), report.tick);
        assert_eq!(
            StateHash(self.shadow.state_hash().unwrap()),
            report.state_hash,
            "independent live authority diverged at tick {}",
            report.tick.get()
        );
    }

    fn counter_delta(current: u64, previous: u64) -> u64 {
        current.checked_sub(previous).unwrap_or(current)
    }

    fn observe_authority_runtime(&mut self) {
        for (index, peer_id) in canonical_peers().into_iter().enumerate() {
            let Some(runtime) = self.hub.peer_runtime_metrics(peer_id) else {
                continue;
            };
            self.last_authority_runtime[index] = Some(runtime);
            if self.traffic_started_at.is_some() {
                self.authority_sent_during_fight[index] = self.authority_sent_during_fight[index]
                    .saturating_add(Self::counter_delta(
                        runtime.sent_bytes,
                        self.authority_sent_counter[index],
                    ));
                self.authority_sent_counter[index] = runtime.sent_bytes;
            }
        }
    }

    fn observe_client_runtime(&mut self) {
        if self.traffic_started_at.is_none() {
            return;
        }
        for (index, client) in self.clients.iter().enumerate() {
            let sent = client.status().runtime.sent_bytes;
            self.client_sent_during_fight[index] = self.client_sent_during_fight[index]
                .saturating_add(Self::counter_delta(sent, self.client_sent_counter[index]));
            self.client_sent_counter[index] = sent;
        }
    }

    fn start_traffic_window(&mut self) {
        assert!(self.traffic_started_at.is_none());
        self.observe_authority_runtime();
        self.traffic_started_at = Some(self.network_tick);
        for (index, client) in self.clients.iter().enumerate() {
            self.client_sent_counter[index] = client.status().runtime.sent_bytes;
            self.authority_sent_counter[index] = self.last_authority_runtime[index]
                .expect("fighting peer has an authority runtime")
                .sent_bytes;
        }
    }

    fn service(&mut self) -> Option<AuthorityTickReport> {
        // Stable production-like order:
        // 1. advance the four independent fault clocks;
        // 2. enqueue distinct per-seat input and quality observations;
        // 3. service every live client exactly once;
        // 4. pump the hub;
        // 5. advance at most one canonical authority tick;
        // 6. verify the independent shadow;
        // 7. pump the hub again to flush that canonical report.
        self.network_tick = self.network_tick.next();
        for lab in &self.labs {
            lab.advance_to(self.network_tick.get()).unwrap();
        }
        self.submit_distinct_client_observations();
        for (index, client) in self.clients.iter().enumerate() {
            if client.terminal().is_none() {
                let advanced = client.advance_manual(1);
                let expected_detached_reconnect = self.scenario
                    == LiveMatrixScenario::ReconnectOneOfFour
                    && index == 2
                    && self.reconnect_target_detached
                    && matches!(client.terminal(), Some(RemoteOnlineTerminal::Failed(_)));
                assert!(
                    advanced
                        || matches!(client.terminal(), Some(RemoteOnlineTerminal::Completed(_)))
                        || expected_detached_reconnect,
                    "client {} stopped unexpectedly: status={:?}, terminal={:?}, authority_phase={:?}, hub={:?}, audit={:?}",
                    client.peer_id().get(),
                    client.status(),
                    client.terminal(),
                    self.hub.peer_phase(client.peer_id()),
                    self.hub.metrics(),
                    self.hub.observability().audit().iter().collect::<Vec<_>>(),
                );
            }
        }
        self.observe_client_runtime();
        if self
            .clients
            .iter()
            .all(|client| matches!(client.terminal(), Some(RemoteOnlineTerminal::Completed(_))))
        {
            // The shipping client tears down its endpoint after publishing the
            // terminal result. Stop on that exact success boundary instead of
            // asking the authority to classify expected post-result closure.
            return None;
        }
        self.hub.pump_network(self.network_tick).unwrap();
        self.observe_authority_runtime();
        let (outcome, report) = self.hub.try_advance(neutral_disconnected).unwrap();
        match (outcome, report.as_ref()) {
            (AuthorityAdvanceOutcome::Advanced, Some(_))
            | (AuthorityAdvanceOutcome::WaitingForReady, None)
            | (AuthorityAdvanceOutcome::WaitingForStartTick, None)
            | (AuthorityAdvanceOutcome::Finished, None) => {}
            invalid => panic!("invalid authority advance outcome: {invalid:?}"),
        }
        if let Some(report) = &report {
            self.validate_committed_inputs(report);
            self.step_shadow(report);
        }
        self.hub.pump_network(self.network_tick).unwrap();
        self.observe_authority_runtime();
        report
    }

    fn drive_until_fighting(&mut self) {
        for _ in 0..STARTUP_LIMIT {
            self.service();
            let clients_fighting = self
                .clients
                .iter()
                .all(|client| client.status().phase == RemoteOnlineClientPhase::Fighting);
            let authority_fighting = canonical_peers()
                .into_iter()
                .all(|peer_id| self.hub.peer_phase(peer_id) == Some(AuthorityPeerPhase::Fighting));
            if clients_fighting && authority_fighting {
                let countdown = self
                    .hub
                    .countdown_start_tick()
                    .expect("authority selected countdown");
                for client in &self.clients {
                    assert_eq!(client.status().countdown_start_tick, Some(countdown));
                }
                self.start_traffic_window();
                return;
            }
        }
        panic!(
            "four-peer startup stalled: clients={:?}, phases={:?}, hub={:?}",
            self.clients
                .iter()
                .map(RemoteOnlineClient::status)
                .collect::<Vec<_>>(),
            canonical_peers()
                .into_iter()
                .map(|peer_id| self.hub.peer_phase(peer_id))
                .collect::<Vec<_>>(),
            self.hub.metrics()
        );
    }

    fn drive_fighting_ticks(&mut self, ticks: usize) {
        let starting_tick = self.hub.authority().simulation().current_sim_tick();
        let target_tick = SimTick(starting_tick.get().saturating_add(ticks as u64));
        for _ in 0..ticks.saturating_add(STARTUP_LIMIT) {
            if self.hub.authority().simulation().current_sim_tick() >= target_tick {
                break;
            }
            self.service();
            assert!(
                self.hub.confirmed_result().is_none(),
                "live match ended before the deterministic draw injection"
            );
        }
        assert_eq!(
            self.hub.authority().simulation().current_sim_tick().get(),
            target_tick.get(),
            "authority did not complete the requested active simulation ticks within the bounded startup allowance"
        );
    }

    fn assert_exact_actions_observed(&self) {
        assert_eq!(
            self.exact_actions_observed, [true; PEER_COUNT],
            "not every authenticated seat produced its distinct committed action pattern"
        );
    }

    fn detach_for_reconnect(&mut self, index: usize) -> (SimTick, SimTick) {
        let target = canonical_peers()[index];
        let before = self.clients[index].status();
        let connection = self.hub.connection_for_peer(target).unwrap();
        self.hub.detach(connection).unwrap();
        self.reconnect_target_detached = true;
        assert_eq!(self.hub.peer_phase(target), None);
        (
            before.confirmed_tick.unwrap_or(SimTick::ZERO),
            self.hub.authority().simulation().current_sim_tick(),
        )
    }

    fn attach_reconnect(&mut self, index: usize, last_confirmed_tick: SimTick) {
        let target = canonical_peers()[index];
        let reconnect_seed = 0xAFC0_5245_434F_4E00 ^ index as u64;
        let config = FaultLabConfig::symmetric(FaultConfig::default(), reconnect_seed);
        let (lab, client_endpoint, authority_endpoint) =
            DeterministicNetworkLab::pair(config).unwrap();
        self.hub
            .attach_reconnect(
                user_for(target),
                ReconnectClaim {
                    match_id: self.config.manifest.match_id,
                    peer_id: target,
                    last_confirmed_tick,
                },
                authority_endpoint,
            )
            .unwrap();
        self.clients[index]
            .reconnect_manual(client_endpoint)
            .unwrap();
        self.labs[index] = lab;
        assert_eq!(self.clients[index].peer_id(), target);
        assert_eq!(self.clients[index].generation(), 2);
        assert_eq!(
            self.clients[index].status().phase,
            RemoteOnlineClientPhase::Reconnecting
        );
    }

    fn drive_reconnect_until_fighting(&mut self, index: usize, snapshot_floor: SimTick) {
        let target = canonical_peers()[index];
        for _ in 0..STARTUP_LIMIT {
            self.service();
            if self.clients[index].status().phase == RemoteOnlineClientPhase::Fighting
                && self.hub.peer_phase(target) == Some(AuthorityPeerPhase::Fighting)
            {
                let status = self.clients[index].status();
                assert!(
                    status
                        .confirmed_tick
                        .is_some_and(|tick| tick >= snapshot_floor),
                    "reconnected client did not apply a fresh retained authority snapshot"
                );
                assert_eq!(status.protocol.hard_resync_snapshots_applied, 1);
                assert_eq!(status.rollback.hard_resyncs_applied, 0);
                assert_eq!(self.hub.metrics().reconnects_completed, 1);
                self.reconnect_target_detached = false;
                return;
            }
        }
        panic!(
            "peer {} reconnect stalled: client={:?}, hub_phase={:?}, hub={:?}",
            target.get(),
            self.clients[index].status(),
            self.hub.peer_phase(target),
            self.hub.metrics()
        );
    }

    fn drive_confirmed_result(&mut self) {
        self.active_missing_substitute_counts = Some(self.missing_substitute_counts);
        self.finishing_started_at = Some(self.hub.authority().simulation().current_sim_tick());
        let mut final_report = None;
        for _ in 0..RESULT_LIMIT {
            if self.clients.iter().all(|client| {
                client.confirmed_result().is_some()
                    && matches!(client.terminal(), Some(RemoteOnlineTerminal::Completed(_)))
            }) {
                break;
            }
            if let Some(report) = self.service()
                && report.final_result_id.is_some()
            {
                final_report = Some(report);
            }
        }

        let report = final_report.expect("authority did not publish its canonical result tick");
        let authority = self
            .hub
            .confirmed_result()
            .expect("authority retained result identity");
        assert_eq!(report.final_result_id, Some(authority.result_id.get()));
        assert_eq!(report.tick, authority.final_tick);
        assert_eq!(report.state_hash, authority.final_state_hash);

        let client_statuses: Vec<_> = self
            .clients
            .iter()
            .map(RemoteOnlineClient::status)
            .collect();
        for (index, client) in self.clients.iter_mut().enumerate() {
            let result = client.confirmed_result().unwrap_or_else(|| {
                panic!(
                    "peer {} did not confirm the draw: status={:?}, terminal={:?}",
                    client.peer_id().get(),
                    client_statuses[index],
                    client.terminal()
                )
            });
            assert_eq!(result.result_id, authority.result_id.get());
            assert_eq!(result.final_tick, authority.final_tick);
            assert_eq!(result.final_hash, authority.final_state_hash);
            assert_eq!(
                client.terminal(),
                Some(RemoteOnlineTerminal::Completed(result))
            );

            let mut projection_target = build_headless_simulation(self.config.clone()).unwrap();
            let update = client
                .project_latest(projection_target.world_mut())
                .unwrap();
            assert_eq!(update.confirmed_result, Some(result));
            assert_eq!(update.projected_confirmed_result, Some(result));
            assert_eq!(
                update.terminal,
                Some(RemoteOnlineTerminal::Completed(result))
            );
            assert_eq!(projection_target.current_sim_tick(), result.final_tick);
            assert_eq!(projection_target.state_hash().unwrap(), result.final_hash.0);
        }
    }

    fn assert_runtime_metrics(runtime: RuntimeMetrics, capacity: usize) {
        assert!(runtime.inbound_high_water <= capacity);
        assert!(runtime.outbound_high_water <= capacity);
        assert!(runtime.reliable_high_water <= capacity);
        assert_eq!(runtime.inbound_queue_overflows, 0);
        assert_eq!(runtime.outbound_queue_overflows, 0);
        assert_eq!(runtime.reliable_reorder_overflows, 0);
        assert_eq!(runtime.ack_queue_overflows, 0);
        assert_eq!(runtime.retry_exhaustions, 0);
        assert_eq!(runtime.transport_errors, 0);
        assert_eq!(runtime.malformed_datagrams, 0);
        assert_eq!(runtime.direction_rejections, 0);
        assert_eq!(runtime.decode_rejections, 0);
    }

    fn assert_bounded_metrics(&self, expected_reconnects: u64) {
        let active_missing_substitutes = self
            .active_missing_substitute_counts
            .expect("active-fight substitution counters were sealed before result closure");
        let elapsed_ticks = self.traffic_started_at.map_or(1, |started| {
            self.network_tick.get().saturating_sub(started.get()).max(1)
        });
        let history_capacity = usize::from(self.config.manifest.snapshot_history_ticks);
        let rollback_limit = u64::from(self.config.manifest.rollback_limit_ticks);
        for (index, client) in self.clients.iter().enumerate() {
            let status = client.status();
            Self::assert_runtime_metrics(
                status.runtime,
                self.client_config.protocol.runtime.inbound_capacity,
            );
            assert!(status.worker.command_queue_high_water <= CLIENT_COMMAND_CAPACITY);
            assert_eq!(status.worker.command_queue_full, 0);
            assert_eq!(status.worker.command_queue_disconnected, 0);
            assert!(status.worker.input_commands_submitted > 0);
            assert!(status.worker.quality_commands_submitted > 0);
            assert!(status.quality.sample_count > 0);
            assert!(status.quality.peak_rtt_ms >= QUALITY_RTT_MS[index]);
            assert!(status.quality.peak_loss_bps >= QUALITY_LOSS_BPS[index]);
            assert!(status.rollback.maximum_normal_rollback_depth <= rollback_limit);
            assert!(status.rollback.snapshot_history_high_water <= history_capacity);
            assert!(status.rollback.input_history_high_water <= history_capacity);
            assert!(
                status.protocol.result_repair_requests <= 1,
                "one final result may schedule at most one reliable closure repair"
            );
            let terminal_repairs = status.protocol.result_repair_requests;
            let expected_repair_snapshots = status.protocol.hard_resync_requests;
            assert!(
                terminal_repairs <= expected_repair_snapshots,
                "terminal repair requests are a classified subset of all hard-resync requests"
            );
            match self.scenario {
                LiveMatrixScenario::Loopback4 | LiveMatrixScenario::RollbackStorm4 => {
                    assert_eq!(expected_repair_snapshots, 0);
                    assert_eq!(status.rollback.hard_resyncs_applied, 0);
                    assert_eq!(status.protocol.hard_resync_snapshots_applied, 0);
                }
                LiveMatrixScenario::Typical4 | LiveMatrixScenario::Degraded4 => {
                    assert!(
                        status.rollback.hard_resyncs_applied <= expected_repair_snapshots,
                        "a request may complete with a hash-confirmed stale snapshot boundary"
                    );
                    assert_eq!(
                        status.protocol.hard_resync_snapshots_applied,
                        expected_repair_snapshots
                    );
                }
                LiveMatrixScenario::ReconnectOneOfFour => {
                    assert!(
                        status.rollback.hard_resyncs_applied <= expected_repair_snapshots,
                        "a request may complete with a hash-confirmed stale snapshot boundary"
                    );
                    assert_eq!(
                        status.protocol.hard_resync_snapshots_applied,
                        expected_repair_snapshots + u64::from(index == 2)
                    );
                }
            }
            let corrections = status.rollback.corrections;
            if corrections > 0 {
                assert!(
                    status.rollback.resimulated_ticks <= corrections.saturating_mul(rollback_limit)
                );
            }

            let peer_id = canonical_peers()[index];
            let authority_runtime = self.last_authority_runtime[index]
                .expect("authority-link metrics were observed before terminal teardown");
            Self::assert_runtime_metrics(authority_runtime, hub_config().runtime.inbound_capacity);
            let upstream =
                average_bytes_per_second(self.client_sent_during_fight[index], elapsed_ticks);
            let downstream =
                average_bytes_per_second(self.authority_sent_during_fight[index], elapsed_ticks);
            assert!(
                upstream <= UPSTREAM_BUDGET_BYTES_PER_SECOND,
                "peer {} upstream {} B/s exceeded {} B/s",
                peer_id.get(),
                upstream,
                UPSTREAM_BUDGET_BYTES_PER_SECOND
            );
            assert!(
                downstream <= DOWNSTREAM_BUDGET_BYTES_PER_SECOND,
                "peer {} downstream {} B/s exceeded {} B/s",
                peer_id.get(),
                downstream,
                DOWNSTREAM_BUDGET_BYTES_PER_SECOND
            );
        }

        for lab in &self.labs {
            let metrics = lab.metrics();
            for direction in [metrics.a_to_b, metrics.b_to_a] {
                assert!(direction.injection_attempts > 0);
                assert!(direction.injected_bytes > 0);
                assert!(direction.pending_high_water <= DEFAULT_FAULT_QUEUE_PACKETS);
                assert_eq!(direction.queue_full_events, 0);
                assert_eq!(direction.disconnected_send_attempts, 0);
                assert_eq!(direction.disconnected_receive_attempts, 0);
            }
        }

        let hub = self.hub.metrics();
        assert_eq!(
            hub.connections_attached,
            PEER_COUNT as u64 + expected_reconnects
        );
        assert_eq!(hub.reconnects_completed, expected_reconnects);
        assert_eq!(hub.authentication_rejections, 0);
        assert_eq!(hub.peers_rejected, 0);
        assert_eq!(hub.spoofed_messages, 0);
        assert_eq!(hub.malformed_or_abusive_disconnects, 0);
        assert!(hub.post_result_transport_closures <= PEER_COUNT as u64);
        assert_eq!(hub.security_violations, 0);
        assert_eq!(hub.security_kicks, 0);
        assert_eq!(
            self.hub.authority().input_metrics().rejected_future_frames,
            0,
            "honest lead scheduling must never submit beyond the authority acceptance window"
        );
        assert!(hub.input_batches_accepted > 0);
        assert!(hub.results_queued >= PEER_COUNT as u64);
        assert!(hub.results_queued <= (PEER_COUNT * 4) as u64);
        let hard_resync_requests: u64 = self
            .clients
            .iter()
            .map(|client| client.status().protocol.hard_resync_requests)
            .sum();
        assert_eq!(
            hub.resyncs_started,
            PEER_COUNT as u64 + expected_reconnects + hard_resync_requests,
            "only initial, client-requested, explicit reconnect, and terminal-loss transfers are allowed"
        );
        assert!(
            self.hub.observability().counters().queue_high_water
                <= MAX_RUNTIME_QUEUE_MESSAGES as u32
        );
        assert!(
            self.hub.observability().counters().history_high_water
                <= hub_config().state_history_entries as u32
        );

        assert!(
            self.peer_origin_counts.into_iter().all(|count| count > 0),
            "every seat must commit authenticated peer input: {:?}",
            self.peer_origin_counts
        );
        match self.scenario {
            LiveMatrixScenario::Loopback4 | LiveMatrixScenario::RollbackStorm4 => {
                assert_eq!(active_missing_substitutes, [0; PEER_COUNT]);
                assert_eq!(self.disconnected_bot_counts, [0; PEER_COUNT]);
            }
            LiveMatrixScenario::Typical4 => {
                assert!(
                    active_missing_substitutes
                        .into_iter()
                        .all(|count| count <= 64),
                    "typical-link substitution exceeded its measured bound: {:?}",
                    active_missing_substitutes
                );
                assert_eq!(self.disconnected_bot_counts, [0; PEER_COUNT]);
            }
            LiveMatrixScenario::Degraded4 => {
                assert!(
                    active_missing_substitutes
                        .into_iter()
                        .all(|count| count <= 400),
                    "degraded-link substitution exceeded its measured bound: {:?}",
                    active_missing_substitutes
                );
                assert_eq!(self.disconnected_bot_counts, [0; PEER_COUNT]);
            }
            LiveMatrixScenario::ReconnectOneOfFour => {
                for index in 0..PEER_COUNT {
                    if index != 2 {
                        assert_eq!(self.missing_substitute_counts[index], 0);
                        assert_eq!(self.disconnected_bot_counts[index], 0);
                    }
                }
                assert!(
                    self.reconnect_target_resumed_peer_input,
                    "the replacement generation never resumed authenticated peer input"
                );
            }
        }
    }

    fn assert_fault_profile_exercised(&self) {
        match self.scenario {
            LiveMatrixScenario::Loopback4 | LiveMatrixScenario::ReconnectOneOfFour => {
                for lab in &self.labs {
                    let metrics = lab.metrics();
                    for direction in [metrics.a_to_b, metrics.b_to_a] {
                        assert_eq!(direction.dropped_by_loss, 0);
                        assert_eq!(direction.duplicate_copies, 0);
                        assert_eq!(direction.reordered_copies, 0);
                    }
                }
            }
            LiveMatrixScenario::Typical4 => {
                for (index, lab) in self.labs.iter().enumerate() {
                    let metrics = lab.metrics();
                    for (name, direction) in [
                        ("client-to-authority", metrics.a_to_b),
                        ("authority-to-client", metrics.b_to_a),
                    ] {
                        assert!(
                            direction.dropped_by_loss > 0,
                            "typical peer {index} {name} path injected no configured loss"
                        );
                        assert_eq!(direction.duplicate_copies, 0);
                        assert_eq!(direction.reordered_copies, 0);
                    }
                }
            }
            LiveMatrixScenario::Degraded4 => {
                for (index, lab) in self.labs.iter().enumerate() {
                    let metrics = lab.metrics();
                    for (name, direction) in [
                        ("client-to-authority", metrics.a_to_b),
                        ("authority-to-client", metrics.b_to_a),
                    ] {
                        assert!(
                            direction.dropped_by_loss > 0,
                            "degraded peer {index} {name} path injected no configured loss"
                        );
                        assert!(
                            direction.duplicate_copies > 0,
                            "degraded peer {index} {name} path injected no configured duplicates"
                        );
                        assert!(
                            direction.reordered_copies > 0,
                            "degraded peer {index} {name} path injected no configured reorder"
                        );
                    }
                }
            }
            LiveMatrixScenario::RollbackStorm4 => {
                for (index, lab) in self.labs.iter().enumerate() {
                    let metrics = lab.metrics();
                    assert_eq!(metrics.a_to_b.reordered_copies, 0);
                    assert!(
                        metrics.b_to_a.reordered_copies > 0,
                        "rollback-storm peer {index} downstream injected no reordering"
                    );
                }
                assert!(
                    self.clients.iter().all(|client| {
                        let rollback = client.status().rollback;
                        rollback.corrections > 0
                    }),
                    "rollback storm did not correct every independently predicted client"
                );
                assert!(
                    self.clients.iter().any(|client| client
                        .status()
                        .rollback
                        .maximum_normal_rollback_depth
                        >= 8),
                    "rollback storm never reached its intended deep normal-rollback band"
                );
            }
        }
    }
}

fn run_standard_scenario(scenario: LiveMatrixScenario) {
    let mut harness = LiveAcceptanceHarness::new(scenario);
    harness.assert_headless_authority();
    harness.drive_until_fighting();
    harness.drive_fighting_ticks(scenario.active_ticks());
    harness.assert_exact_actions_observed();
    harness.drive_confirmed_result();
    harness.assert_bounded_metrics(0);
    harness.assert_fault_profile_exercised();
}

#[test]
fn production_live_loopback4() {
    run_standard_scenario(LiveMatrixScenario::Loopback4);
}

#[test]
fn production_live_typical4() {
    run_standard_scenario(LiveMatrixScenario::Typical4);
}

#[test]
fn production_live_degraded4() {
    run_standard_scenario(LiveMatrixScenario::Degraded4);
}

#[test]
fn production_live_rollback_storm4() {
    run_standard_scenario(LiveMatrixScenario::RollbackStorm4);
}

#[test]
fn production_live_reconnect_one_of_four() {
    const TARGET_INDEX: usize = 2;
    const DISCONNECTED_TICKS: usize = 8;

    let mut harness = LiveAcceptanceHarness::new(LiveMatrixScenario::ReconnectOneOfFour);
    harness.assert_headless_authority();
    harness.drive_until_fighting();
    harness.drive_fighting_ticks(90);
    harness.assert_exact_actions_observed();

    let healthy_before: Vec<_> = harness
        .clients
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != TARGET_INDEX)
        .map(|(_, client)| client.status())
        .collect();
    let (last_confirmed, detached_at) = harness.detach_for_reconnect(TARGET_INDEX);
    for _ in 0..DISCONNECTED_TICKS {
        harness.service();
        for (index, peer_id) in canonical_peers().into_iter().enumerate() {
            if index == TARGET_INDEX {
                continue;
            }
            assert_eq!(
                harness.clients[index].status().phase,
                RemoteOnlineClientPhase::Fighting
            );
            assert_eq!(
                harness.hub.peer_phase(peer_id),
                Some(AuthorityPeerPhase::Fighting)
            );
        }
    }
    let snapshot_floor = harness.hub.authority().simulation().current_sim_tick();
    assert!(snapshot_floor >= detached_at);

    harness.attach_reconnect(TARGET_INDEX, last_confirmed);
    harness.drive_reconnect_until_fighting(TARGET_INDEX, snapshot_floor);

    let mut healthy_cursor = 0;
    for (index, client) in harness.clients.iter().enumerate() {
        if index == TARGET_INDEX {
            continue;
        }
        let before = healthy_before[healthy_cursor];
        let after = client.status();
        healthy_cursor += 1;
        assert_eq!(client.generation(), 1);
        assert_eq!(after.phase, RemoteOnlineClientPhase::Fighting);
        assert!(after.confirmed_tick >= before.confirmed_tick);
        assert_eq!(
            after.protocol.hard_resync_snapshots_applied,
            before.protocol.hard_resync_snapshots_applied,
            "one peer's reconnect forced an unnecessary healthy-peer repair"
        );
    }

    harness.drive_fighting_ticks(LiveMatrixScenario::ReconnectOneOfFour.active_ticks());
    harness.drive_confirmed_result();
    harness.assert_bounded_metrics(1);
    harness.assert_fault_profile_exercised();
}
