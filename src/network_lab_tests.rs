use std::collections::VecDeque;

use crate::authority::{AuthorityMatch, AuthoritySimulation, AuthorityTickReport};
use crate::authority_input::{
    AuthorityInputConfig, AuthorityInputOrigin, CommittedTickInputs, InputIngestReport,
};
use crate::determinism::{FighterId, SimEntityKind};
use crate::local_loopback::{
    AppliedCanonicalSnapshot, ClientAuthorityOutcome, InitialSnapshotTarget,
};
use crate::network_codec::{
    ProcessedInputAck, ResultId, ResultIdentifier, StateHashAndAcks, WireMessage,
};
use crate::network_io::{
    AfcChannel, AfcDatagram, DeterministicNetworkLab, DisconnectWindow, FaultConfig,
    FaultLabConfig, FaultLabEndpoint, InProcessEndpoint, NonBlockingDatagramEndpoint, SendOutcome,
    UdpEndpoint,
};
use crate::network_lab::{
    DOWNSTREAM_BUDGET_BYTES_PER_SECOND, MAX_NETWORK_PEERS, MultiPeerRuntimeCoordinator,
    NetworkAcceptanceScenario, PeerEndpointPair, PeerTrafficSnapshot,
    UPSTREAM_BUDGET_BYTES_PER_SECOND, average_bytes_per_second,
};
use crate::network_protocol::{
    AuthorityKind, BuildId, ClockProbeId, CommittedInputRecord, CommittedInputRelay,
    CommittedInputSource, CommittedSeatInputWindow, CompatibilityId, ConnectionPhase, DefinitionId,
    FighterSlotConfig, GameplayContentHash, InputBatch, InputButtons, InputFrame, InputSequence,
    MAX_INPUT_FRAMES_PER_WINDOW, MAX_RESYNC_INPUT_TAIL_TICKS, MAX_SEATS, ManifestHash, MatchId,
    MatchManifest, PeerId, ProtocolVersion, QuantizedAxis, ReplayFormatVersion, ResyncApplied,
    ResyncBegin, ResyncReason, SIMULATION_HZ, SeatAssignment, SeatId, SeatInputWindow, SeatOwner,
    SeatOwnership, SimTick, SimulationVersion, StartMessage, StateHash, TeamId, TransferId,
};
use crate::network_runtime::{
    NetworkRuntime, PeerRole, RuntimeConfig, RuntimeConnectionState, RuntimeEvent,
};
use crate::predicted_client::{PredictedClient, PredictedClientMetrics};
use crate::resync_transfer::{
    AuthorityResyncTransfer, ClientResyncAssembler, CompletedResyncTransfer, ResyncBeginOutcome,
    ResyncChunkOutcome, ResyncInputTailOutcome,
};
use crate::rollback::{RollbackMetrics, RollbackWorld};
use crate::session::{AppliedInitialSync, ConfirmedSessionResult};
use crate::session_clock::MIN_CLOCK_SYNC_SAMPLES;
use crate::snapshot::{
    ArenaRuntimeSnapshot, CanonicalSnapshot, FighterSnapshot, MatchPhaseSnapshot,
    MatchResultSnapshot, MatchStateSnapshot, MatchStatsSnapshot, PoolAllocatorSnapshot,
    SnapshotError, SnapshotHeader,
};
use crate::state_sync::{
    AuthoritySnapshotHistory, AuthorityStateSyncCoordinator, DEFAULT_STATE_SYNC_HISTORY_ENTRIES,
    PeerStateUpdateOutcome,
};

const MATCH_BYTES: [u8; 16] = *b"network-lab-0001";
const RESULT_ID: u64 = 0xAFC0_4E45_544C_4142;
const START_TICK: u64 = 300;
const INPUT_LEAD_TICKS: u64 = 4;
const NORMAL_ROLLBACK_LIMIT: u64 = 12;
const PREDICTION_HISTORY: usize = 64;
const DELTA_INTERVAL_TICKS: u64 = 3;
const COMMITTED_RELAY_FRAMES: usize = 5;
const MAX_STARTUP_TICKS: u64 = 2_000;
const MAX_RESULT_DRAIN_TICKS: u64 = 600;

fn peer_id(index: usize) -> PeerId {
    PeerId::new(101 + index as u64).unwrap()
}

fn compatibility() -> CompatibilityId {
    CompatibilityId {
        protocol: ProtocolVersion::new(1).unwrap(),
        simulation: SimulationVersion::new(1).unwrap(),
        replay: ReplayFormatVersion::new(1).unwrap(),
        build: BuildId::new([0x31; 16]).unwrap(),
        gameplay_content: GameplayContentHash::new([0x42; 32]).unwrap(),
    }
}

fn manifest_with_owners(owners: [PeerId; MAX_SEATS]) -> MatchManifest {
    let assignments = std::array::from_fn::<_, MAX_SEATS, _>(|index| SeatAssignment {
        seat: SeatId::new(index as u8).unwrap(),
        fighter: FighterId::new(index as u8).unwrap(),
        owner: SeatOwner::Peer(owners[index]),
    });
    let slots = std::array::from_fn(|index| FighterSlotConfig {
        occupied: true,
        fighter: FighterId::new(index as u8).unwrap(),
        team: TeamId::new(index as u8).unwrap(),
        character: DefinitionId::new(index as u16 + 1).unwrap(),
        style: DefinitionId::new(1).unwrap(),
        equipment: DefinitionId::new(0).unwrap(),
    });
    MatchManifest {
        compatibility: compatibility(),
        manifest_hash: ManifestHash(0x4E45_544C_4142),
        match_id: MatchId::new(MATCH_BYTES).unwrap(),
        authority: AuthorityKind::Dedicated,
        trusted_results: true,
        arena: DefinitionId::new(1).unwrap(),
        rules: DefinitionId::new(1).unwrap(),
        slots,
        ownership: SeatOwnership::from_assignments(&assignments).unwrap(),
        master_gameplay_seed: 0xAFC0_2026,
        rng_scheme_version: 1,
        tick_rate_hz: SIMULATION_HZ,
        input_delay_ticks: INPUT_LEAD_TICKS as u8,
        rollback_limit_ticks: NORMAL_ROLLBACK_LIMIT as u8,
        snapshot_history_ticks: 64,
        agreed_start_tick: SimTick(START_TICK),
    }
}

fn four_peer_manifest() -> MatchManifest {
    manifest_with_owners(std::array::from_fn(peer_id))
}

fn couch_coop_manifest() -> MatchManifest {
    manifest_with_owners([peer_id(0), peer_id(0), peer_id(1), peer_id(2)])
}

fn initial_snapshot(manifest: &MatchManifest) -> CanonicalSnapshot {
    let allocators = SimEntityKind::ALL
        .into_iter()
        .map(|kind| PoolAllocatorSnapshot::empty(kind, 1).unwrap())
        .collect();
    let fighters = FighterId::ALL.map(|fighter| FighterSnapshot {
        occupied: true,
        active: true,
        ..FighterSnapshot::empty(fighter)
    });
    CanonicalSnapshot {
        header: SnapshotHeader::new(
            u32::from(manifest.compatibility.simulation.get()),
            u32::from(manifest.compatibility.protocol.get()),
            0x4E45_544C_4142,
            *manifest.match_id.as_bytes(),
            SimTick::ZERO,
            manifest.master_gameplay_seed,
        ),
        match_state: MatchStateSnapshot {
            phase: MatchPhaseSnapshot::Fight,
            active_slots_mask: 0b1111,
            stocks: [3; MAX_SEATS],
            ..MatchStateSnapshot::default()
        },
        fighters,
        arena: ArenaRuntimeSnapshot::default(),
        allocators,
        dynamic_objects: Vec::new(),
        rng_streams: Vec::new(),
        stats: MatchStatsSnapshot::default(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToySimulationError {
    TickGap,
    MissingSeat,
}

struct ToySimulation {
    snapshot: CanonicalSnapshot,
    finish_tick: SimTick,
}

impl ToySimulation {
    fn new(manifest: &MatchManifest, finish_tick: u64) -> Self {
        Self {
            snapshot: initial_snapshot(manifest),
            finish_tick: SimTick(finish_tick),
        }
    }
}

impl AuthoritySimulation for ToySimulation {
    type Snapshot = CanonicalSnapshot;
    type Error = ToySimulationError;

    fn current_tick(&self) -> SimTick {
        self.snapshot.header.tick
    }

    fn step(&mut self, inputs: &CommittedTickInputs) -> Result<(), Self::Error> {
        if inputs.tick != self.snapshot.header.tick.next() {
            return Err(ToySimulationError::TickGap);
        }
        if inputs.len() != MAX_SEATS {
            return Err(ToySimulationError::MissingSeat);
        }
        apply_toy_tick(&mut self.snapshot, inputs.tick, |seat| {
            inputs
                .iter()
                .find(|record| record.frame.seat.get() as usize == seat)
                .expect("authority committed every occupied seat")
                .frame
        });
        if inputs.tick >= self.finish_tick {
            self.snapshot.match_state.phase = MatchPhaseSnapshot::Result;
            self.snapshot.match_state.result = MatchResultSnapshot::Draw {
                decided_tick: self.finish_tick,
            };
        }
        Ok(())
    }

    fn capture_snapshot(&self) -> Result<Self::Snapshot, Self::Error> {
        Ok(self.snapshot.clone())
    }

    fn final_result_id(&self) -> Option<u64> {
        (self.snapshot.header.tick >= self.finish_tick).then_some(RESULT_ID)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ToyRollbackError {
    TickGap,
    Snapshot(SnapshotError),
}

struct ToyRollbackWorld {
    snapshot: CanonicalSnapshot,
}

impl ToyRollbackWorld {
    fn new(manifest: &MatchManifest) -> Self {
        Self {
            snapshot: initial_snapshot(manifest),
        }
    }
}

impl RollbackWorld for ToyRollbackWorld {
    type Snapshot = CanonicalSnapshot;
    type Error = ToyRollbackError;

    fn current_tick(&self) -> SimTick {
        self.snapshot.header.tick
    }

    fn capture_snapshot(&self) -> Result<Self::Snapshot, Self::Error> {
        Ok(self.snapshot.clone())
    }

    fn restore_snapshot(&mut self, snapshot: &Self::Snapshot) -> Result<(), Self::Error> {
        self.snapshot = snapshot.clone();
        Ok(())
    }

    fn step(&mut self, tick: SimTick, inputs: &[InputFrame; MAX_SEATS]) -> Result<(), Self::Error> {
        if tick != self.snapshot.header.tick.next() {
            return Err(ToyRollbackError::TickGap);
        }
        apply_toy_tick(&mut self.snapshot, tick, |seat| inputs[seat]);
        Ok(())
    }

    fn state_hash(&self) -> Result<u64, Self::Error> {
        self.snapshot
            .canonical_hash()
            .map_err(ToyRollbackError::Snapshot)
    }
}

fn apply_toy_tick(
    snapshot: &mut CanonicalSnapshot,
    tick: SimTick,
    mut frame_for: impl FnMut(usize) -> InputFrame,
) {
    snapshot.header.tick = tick;
    snapshot.stats.gameplay_ticks = tick.get();
    for fighter in 0..MAX_SEATS {
        let frame = frame_for(fighter);
        let continuous = i32::from(frame.movement_x.get())
            + 128
            + i32::from(frame.movement_y.get())
            + i32::from(frame.held_buttons.bits());
        let edges = i32::from(frame.pressed_buttons.bits()) * 3
            + i32::from(frame.released_buttons.bits()) * 5;
        snapshot.stats.damage_by_fighter[fighter] = snapshot.stats.damage_by_fighter[fighter]
            .wrapping_mul(31)
            .wrapping_add(continuous)
            .wrapping_add(edges);
    }
}

fn tape_frame(tick: SimTick, seat: SeatId) -> InputFrame {
    let phase = ((tick.get().wrapping_mul(17) + u64::from(seat.get()) * 29) % 201) as i16 - 100;
    let movement_x = QuantizedAxis::new(phase as i8).unwrap();
    let movement_y = QuantizedAxis::new((-phase / 2) as i8).unwrap();
    let light = tick.get() % (11 + u64::from(seat.get())) == 0;
    let jump = tick.get() % (17 + u64::from(seat.get())) == 0;
    let held_bits =
        if light { InputButtons::LIGHT } else { 0 } | if jump { InputButtons::JUMP } else { 0 };
    InputFrame {
        tick,
        seat,
        movement_x,
        movement_y,
        held_buttons: InputButtons::new(held_bits).unwrap(),
        pressed_buttons: InputButtons::new(held_bits).unwrap(),
        released_buttons: InputButtons::default(),
        sequence: InputSequence(tick.get() as u16),
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct InputHistory {
    frames: [InputFrame; MAX_INPUT_FRAMES_PER_WINDOW],
    len: usize,
}

impl InputHistory {
    fn seed_through(&mut self, newest_tick: SimTick, seat: SeatId) {
        self.len = MAX_INPUT_FRAMES_PER_WINDOW.min(newest_tick.get() as usize);
        for offset in 0..self.len {
            self.frames[offset] = tape_frame(SimTick(newest_tick.get() - offset as u64), seat);
        }
    }

    fn push(&mut self, frame: InputFrame) {
        if self.len == 0 {
            self.frames[0] = frame;
            self.len = 1;
            return;
        }
        assert_eq!(frame.tick, self.frames[0].tick.next());
        let retained = self.len.min(MAX_INPUT_FRAMES_PER_WINDOW - 1);
        self.frames.copy_within(0..retained, 1);
        self.frames[0] = frame;
        self.len = retained + 1;
    }

    fn window(&self) -> SeatInputWindow {
        SeatInputWindow::from_newest_first(&self.frames[..self.len]).unwrap()
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct CommittedHistory {
    records: [CommittedInputRecord; MAX_INPUT_FRAMES_PER_WINDOW],
    len: usize,
}

impl CommittedHistory {
    fn push(&mut self, record: CommittedInputRecord) {
        let retained = self.len.min(MAX_INPUT_FRAMES_PER_WINDOW - 1);
        self.records.copy_within(0..retained, 1);
        self.records[0] = record;
        self.len = retained + 1;
    }

    fn window(&self, maximum_frames: usize) -> CommittedSeatInputWindow {
        CommittedSeatInputWindow::from_newest_first(&self.records[..self.len.min(maximum_frames)])
            .unwrap()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PostSyncStage {
    Applied,
    InitialSync,
    Ready,
}

struct PendingPostSync {
    applied: ResyncApplied,
    initial: bool,
    stage: PostSyncStage,
}

struct LabClient {
    peer_id: PeerId,
    predicted: PredictedClient<ToyRollbackWorld>,
    assembler: ClientResyncAssembler,
    initial_sync: Option<AppliedInitialSync>,
    pending_request: Option<crate::network_protocol::ResyncRequest>,
    resync_in_flight: bool,
    pending_post_sync: Option<PendingPostSync>,
    confirmed_result: Option<ConfirmedSessionResult>,
    input_histories: [InputHistory; MAX_SEATS],
    maximum_prediction_steps_in_network_tick: u64,
}

impl LabClient {
    fn new(manifest: &MatchManifest, peer_id: PeerId) -> Self {
        let mut predicted = PredictedClient::new(
            ToyRollbackWorld::new(manifest),
            manifest.match_id,
            PREDICTION_HISTORY,
        )
        .unwrap();
        predicted.configure_manifest(manifest).unwrap();
        Self {
            peer_id,
            predicted,
            assembler: ClientResyncAssembler::with_default_timeout(manifest.match_id, peer_id)
                .unwrap(),
            initial_sync: None,
            pending_request: None,
            resync_in_flight: false,
            pending_post_sync: None,
            confirmed_result: None,
            input_histories: [InputHistory::default(); MAX_SEATS],
            maximum_prediction_steps_in_network_tick: 0,
        }
    }

    fn handle_authority_outcome(&mut self, outcome: ClientAuthorityOutcome) {
        if let ClientAuthorityOutcome::HardResyncRequired {
            reason,
            last_confirmed_tick,
            last_confirmed_hash,
        } = outcome
            && self.pending_request.is_none()
            && !self.resync_in_flight
        {
            self.pending_request = Some(self.assembler.make_request(
                reason,
                last_confirmed_tick,
                last_confirmed_hash,
            ));
        }
    }

    fn apply_completed(&mut self, completed: CompletedResyncTransfer) {
        let expected = AppliedCanonicalSnapshot {
            tick: completed.applied.snapshot_tick,
            hash: completed.applied.snapshot_hash,
        };
        let initial = self.initial_sync.is_none();
        let actual = if initial {
            self.predicted.apply_initial_snapshot(&completed.snapshot)
        } else {
            self.predicted.apply_resync_snapshot(&completed.snapshot)
        }
        .unwrap();
        self.predicted
            .seed_resync_input_tail(&completed.input_tail)
            .unwrap();
        self.resync_in_flight = false;
        assert_eq!(actual, expected);
        if initial {
            self.initial_sync = Some(AppliedInitialSync {
                tick: actual.tick,
                hash: actual.hash,
            });
        }
        self.pending_post_sync = Some(PendingPostSync {
            applied: completed.applied,
            initial,
            stage: PostSyncStage::Applied,
        });
    }

    fn rollback_metrics(&self) -> RollbackMetrics {
        self.predicted
            .prediction()
            .expect("started lab client owns prediction history")
            .metrics()
    }

    fn predicted_metrics(&self) -> PredictedClientMetrics {
        self.predicted.metrics()
    }
}

struct PendingAuthorityTransfer {
    transfer: AuthorityResyncTransfer,
    begin_sent: bool,
    input_tail_sent: bool,
    next_chunk: u16,
}

#[derive(Clone, Copy, Debug, Default)]
struct LabCounters {
    hard_resync_requests: u64,
    history_resync_requests: u64,
    hash_resync_requests: u64,
    resync_transfers: u64,
    state_hashes_sent: u64,
    state_deltas_sent: u64,
    committed_relays_sent: u64,
    input_batches_sent: u64,
    maximum_authority_rejections: u16,
}

struct ScenarioHarness<E: NonBlockingDatagramEndpoint> {
    manifest: MatchManifest,
    network: MultiPeerRuntimeCoordinator<E>,
    authority: AuthorityMatch<ToySimulation>,
    clients: Vec<LabClient>,
    state_history: AuthoritySnapshotHistory,
    state_sync: AuthorityStateSyncCoordinator,
    committed_histories: [CommittedHistory; MAX_SEATS],
    transfers: Vec<Option<PendingAuthorityTransfer>>,
    superseded_transfers: Vec<VecDeque<ResyncBegin>>,
    next_transfer_id: u32,
    network_tick: SimTick,
    countdown_sent: bool,
    inputs_prefilled: bool,
    last_relay: Option<CommittedInputRelay>,
    last_state: Option<StateHashAndAcks>,
    last_state_messages: Vec<Option<WireMessage>>,
    result: Option<ResultIdentifier>,
    committed_relay_frames: usize,
    counters: LabCounters,
}

impl<E: NonBlockingDatagramEndpoint> ScenarioHarness<E> {
    fn new(
        manifest: MatchManifest,
        endpoint_pairs: Vec<PeerEndpointPair<E>>,
        finish_tick: u64,
    ) -> Self {
        let authority = AuthorityMatch::new(
            manifest,
            ToySimulation::new(&manifest, finish_tick),
            AuthorityInputConfig::default(),
        )
        .unwrap();
        let initial = authority
            .snapshot_at(SimTick::ZERO)
            .expect("authority stores its initial snapshot")
            .clone();
        let mut state_history =
            AuthoritySnapshotHistory::new(manifest.match_id, DEFAULT_STATE_SYNC_HISTORY_ENTRIES)
                .unwrap();
        state_history.record_snapshot(&initial).unwrap();
        let mut state_sync =
            AuthorityStateSyncCoordinator::new(manifest.match_id, endpoint_pairs.len()).unwrap();
        for pair in &endpoint_pairs {
            state_sync.connect_peer(pair.peer_id).unwrap();
        }
        let network = MultiPeerRuntimeCoordinator::new(
            manifest,
            endpoint_pairs,
            RuntimeConfig {
                inbound_capacity: 64,
                outbound_capacity: 64,
                reliable_reorder_capacity: 32,
                max_receive_datagrams_per_pump: 64,
                max_send_datagrams_per_pump: 64,
                reliable_max_attempts: 128,
                ..RuntimeConfig::default()
            },
        )
        .unwrap();
        let clients: Vec<_> = network
            .peer_ids()
            .map(|peer_id| LabClient::new(&manifest, peer_id))
            .collect();
        let transfers = (0..clients.len()).map(|_| None).collect();
        let superseded_transfers = (0..clients.len()).map(|_| VecDeque::new()).collect();
        let last_state_messages = (0..clients.len()).map(|_| None).collect();
        Self {
            manifest,
            network,
            authority,
            clients,
            state_history,
            state_sync,
            committed_histories: [CommittedHistory::default(); MAX_SEATS],
            transfers,
            superseded_transfers,
            next_transfer_id: 1,
            network_tick: SimTick::ZERO,
            countdown_sent: false,
            inputs_prefilled: false,
            last_relay: None,
            last_state: None,
            last_state_messages,
            result: None,
            committed_relay_frames: COMMITTED_RELAY_FRAMES,
            counters: LabCounters::default(),
        }
    }

    fn set_committed_relay_frames(&mut self, frames: usize) {
        assert!((1..=MAX_INPUT_FRAMES_PER_WINDOW).contains(&frames));
        self.committed_relay_frames = frames;
    }

    fn start(&mut self, advance_network: &mut impl FnMut(u64)) {
        for _ in 0..MAX_STARTUP_TICKS {
            self.advance_network_clock(advance_network);
            self.pump_round();
            if self.network.all_ready() && !self.countdown_sent {
                self.network.broadcast_countdown(self.network_tick).unwrap();
                self.countdown_sent = true;
            }
            if self.countdown_sent && !self.inputs_prefilled {
                self.queue_initial_input_windows();
                self.inputs_prefilled = true;
            }
            self.network.pump_authorities(self.network_tick);
            self.network.pump_clients(self.network_tick);
            if self.countdown_sent
                && self.network_tick >= self.network.countdown_start_tick().unwrap()
                && self.clients.iter().all(|client| {
                    self.network.client_phase(client.peer_id).unwrap() == ConnectionPhase::Fighting
                })
            {
                return;
            }
        }
        panic!("multi-peer startup exceeded its deterministic work bound");
    }

    fn advance_network_clock(&mut self, advance_network: &mut impl FnMut(u64)) {
        self.network_tick = self.network_tick.next();
        advance_network(self.network_tick.get());
    }

    fn pump_round(&mut self) {
        self.network.pump_clients(self.network_tick);
        self.drain_client_events();
        self.poll_clients();
        self.service_client_outbound();
        self.network.pump_clients(self.network_tick);

        self.network.pump_authorities(self.network_tick);
        self.drain_authority_events();
        self.service_authority_transfers();
        self.network.pump_authorities(self.network_tick);
    }

    fn drain_client_events(&mut self) {
        let mut events = VecDeque::new();
        self.network
            .drain_client_events(|tagged| events.push_back(tagged));
        while let Some(tagged) = events.pop_front() {
            let client_index = self.client_index(tagged.peer_id);
            match tagged.event {
                RuntimeEvent::Message(WireMessage::Start(StartMessage::Manifest(manifest))) => {
                    assert_eq!(manifest, self.manifest);
                    self.network
                        .client_runtime_mut(tagged.peer_id)
                        .unwrap()
                        .client_session_mut()
                        .unwrap()
                        .content_loaded(self.network_tick)
                        .unwrap();
                    let request = self.clients[client_index].assembler.make_request(
                        ResyncReason::InitialSync,
                        SimTick::ZERO,
                        StateHash(0),
                    );
                    self.clients[client_index].pending_request = Some(request);
                }
                RuntimeEvent::Message(WireMessage::Start(StartMessage::Countdown { .. })) => {}
                RuntimeEvent::Message(WireMessage::ResyncBegin(begin)) => {
                    match self.clients[client_index]
                        .assembler
                        .accept_begin(begin, self.network_tick)
                        .unwrap()
                    {
                        ResyncBeginOutcome::Started | ResyncBeginOutcome::Duplicate => {}
                        ResyncBeginOutcome::Superseded { .. } => {}
                    }
                    if let Some(completed) = self.clients[client_index]
                        .assembler
                        .apply_staged_chunks(self.network_tick)
                        .unwrap()
                    {
                        self.clients[client_index].apply_completed(completed);
                    }
                }
                RuntimeEvent::Message(WireMessage::ResyncChunk(chunk)) => {
                    if let ResyncChunkOutcome::Complete(completed) = self.clients[client_index]
                        .assembler
                        .accept_chunk(chunk, self.network_tick)
                        .unwrap()
                    {
                        self.clients[client_index].apply_completed(completed);
                    }
                }
                RuntimeEvent::Message(WireMessage::ResyncInputTail(tail)) => {
                    if let ResyncInputTailOutcome::Complete(completed) = self.clients[client_index]
                        .assembler
                        .accept_input_tail(tail, self.network_tick)
                        .unwrap()
                    {
                        self.clients[client_index].apply_completed(completed);
                    }
                }
                RuntimeEvent::Message(WireMessage::CommittedInputRelay(relay)) => {
                    let outcome = self.clients[client_index]
                        .predicted
                        .observe_committed_inputs(&relay)
                        .unwrap();
                    self.clients[client_index].handle_authority_outcome(outcome);
                }
                RuntimeEvent::Message(WireMessage::StateHashAndAcks(state)) => {
                    let outcome = self.clients[client_index]
                        .predicted
                        .observe_authority_hash(&state)
                        .unwrap();
                    self.clients[client_index].handle_authority_outcome(outcome);
                }
                RuntimeEvent::Message(WireMessage::StateDeltaAndAcks(delta)) => {
                    let outcome = self.clients[client_index]
                        .predicted
                        .observe_authority_delta(&delta)
                        .unwrap();
                    self.clients[client_index].handle_authority_outcome(outcome);
                }
                RuntimeEvent::Message(WireMessage::ResultIdentifier(result)) => {
                    assert_eq!(result.match_id, self.manifest.match_id);
                    self.clients[client_index].confirmed_result = self
                        .network
                        .client_runtime_mut(tagged.peer_id)
                        .unwrap()
                        .client_session()
                        .and_then(|session| session.result());
                }
                RuntimeEvent::Message(other) => {
                    panic!("unexpected client lab message: {other:?}")
                }
                RuntimeEvent::SessionError(error) => panic!("client session failed: {error:?}"),
                RuntimeEvent::TransportDisconnected => panic!("client transport disconnected"),
            }
        }
    }

    fn drain_authority_events(&mut self) {
        let mut events = VecDeque::new();
        self.network
            .drain_authority_events(|tagged| events.push_back(tagged));
        while let Some(tagged) = events.pop_front() {
            match tagged.event {
                RuntimeEvent::Message(WireMessage::Handshake(_))
                | RuntimeEvent::Message(WireMessage::Start(
                    StartMessage::ManifestAccepted { .. }
                    | StartMessage::InitialSyncApplied { .. }
                    | StartMessage::Ready { .. },
                )) => {}
                RuntimeEvent::Message(WireMessage::InputBatch(batch)) => {
                    assert!(self.countdown_sent, "input preceded global readiness");
                    let ingest = self
                        .authority
                        .ingest_peer_batch(tagged.peer_id, &batch)
                        .unwrap();
                    self.note_ingest(ingest);
                    self.state_sync
                        .observe_validated_input_batch(tagged.peer_id, &batch, &self.state_history)
                        .unwrap();
                }
                RuntimeEvent::Message(WireMessage::ResyncRequest(request)) => {
                    assert_eq!(request.peer_id, tagged.peer_id);
                    self.begin_transfer(tagged.peer_id, request);
                }
                RuntimeEvent::Message(WireMessage::ResyncApplied(applied)) => {
                    let index = self.client_index(tagged.peer_id);
                    if self.transfers[index].as_ref().is_some_and(|pending| {
                        pending.transfer.begin().transfer_id == applied.transfer_id
                    }) {
                        let mut pending = self.transfers[index].take().unwrap();
                        pending.transfer.validate_applied(&applied).unwrap();
                    } else {
                        let position = self.superseded_transfers[index]
                            .iter()
                            .position(|begin| begin.transfer_id == applied.transfer_id)
                            .expect("applied acknowledgement names an active or recent transfer");
                        let begin = self.superseded_transfers[index].remove(position).unwrap();
                        assert_eq!(applied.match_id, begin.match_id);
                        assert_eq!(applied.peer_id, tagged.peer_id);
                        assert_eq!(applied.snapshot_tick, begin.snapshot_tick);
                        assert_eq!(applied.snapshot_hash, begin.snapshot_hash);
                    }
                    self.state_sync
                        .observe_validated_resync_applied(tagged.peer_id, &applied)
                        .unwrap();
                }
                RuntimeEvent::Message(WireMessage::Disconnect(message)) => {
                    panic!("authority received disconnect: {message:?}")
                }
                RuntimeEvent::Message(other) => {
                    panic!("unexpected authority lab message: {other:?}")
                }
                RuntimeEvent::SessionError(error) => panic!("authority session failed: {error:?}"),
                RuntimeEvent::TransportDisconnected => {
                    panic!("authority transport disconnected")
                }
            }
        }
    }

    fn poll_clients(&mut self) {
        for client in &mut self.clients {
            let outcome = client.predicted.poll_authority().unwrap();
            client.handle_authority_outcome(outcome);
        }
    }

    fn service_client_outbound(&mut self) {
        for index in 0..self.clients.len() {
            let peer_id = self.clients[index].peer_id;
            if let Some(request) = self.clients[index].pending_request.take() {
                if request.reason != ResyncReason::InitialSync {
                    eprintln!(
                        "resync-request peer={} reason={:?} detail={:?} confirmed={} network_tick={} authority_tick={}",
                        peer_id.get(),
                        request.reason,
                        self.clients[index].predicted.last_hard_resync_reason(),
                        request.last_confirmed_tick.get(),
                        self.network_tick.get(),
                        self.authority.simulation().current_tick().get(),
                    );
                }
                self.network
                    .queue_client(peer_id, WireMessage::ResyncRequest(request))
                    .unwrap();
                self.clients[index].resync_in_flight = true;
                self.counters.hard_resync_requests = self
                    .counters
                    .hard_resync_requests
                    .saturating_add((request.reason != ResyncReason::InitialSync) as u64);
                self.counters.history_resync_requests = self
                    .counters
                    .history_resync_requests
                    .saturating_add((request.reason == ResyncReason::HistoryExpired) as u64);
                self.counters.hash_resync_requests = self
                    .counters
                    .hash_resync_requests
                    .saturating_add((request.reason == ResyncReason::HashMismatch) as u64);
                continue;
            }
            let Some(mut post) = self.clients[index].pending_post_sync.take() else {
                continue;
            };
            match post.stage {
                PostSyncStage::Applied => {
                    self.network
                        .queue_client(peer_id, WireMessage::ResyncApplied(post.applied))
                        .unwrap();
                    if post.initial {
                        post.stage = PostSyncStage::InitialSync;
                        self.clients[index].pending_post_sync = Some(post);
                    }
                }
                PostSyncStage::InitialSync => {
                    let runtime = self.network.client_runtime_mut(peer_id).unwrap();
                    let mut session = *runtime.client_session().unwrap();
                    let message = session
                        .apply_initial_sync(
                            self.manifest.match_id,
                            post.applied.snapshot_tick,
                            post.applied.snapshot_hash,
                            self.network_tick,
                        )
                        .unwrap();
                    session.mark_clock_synchronized().unwrap();
                    runtime.queue_start_message(message).unwrap();
                    *runtime.client_session_mut().unwrap() = session;
                    for probe in 1..=u32::from(MIN_CLOCK_SYNC_SAMPLES) {
                        self.network
                            .authority_runtime_mut(peer_id)
                            .unwrap()
                            .authority_gate_mut()
                            .unwrap()
                            .observe_clock_probe(peer_id, ClockProbeId::new(probe).unwrap())
                            .unwrap();
                    }
                    post.stage = PostSyncStage::Ready;
                    self.clients[index].pending_post_sync = Some(post);
                }
                PostSyncStage::Ready => {
                    let runtime = self.network.client_runtime_mut(peer_id).unwrap();
                    let ready = runtime.client_session().unwrap().ready_message().unwrap();
                    runtime.queue_start_message(ready).unwrap();
                }
            }
        }
    }

    fn begin_transfer(&mut self, peer_id: PeerId, request: crate::network_protocol::ResyncRequest) {
        let index = self.client_index(peer_id);
        if let Some(pending) = self.transfers[index].as_ref() {
            let previous = pending.transfer.request();
            if request.last_confirmed_tick <= previous.last_confirmed_tick {
                return;
            }
        }
        if let Some(previous) = self.transfers[index].take() {
            let recent = &mut self.superseded_transfers[index];
            if recent.len() == 4 {
                recent.pop_front();
            }
            recent.push_back(previous.transfer.begin());
        }
        let tick = self.authority.simulation().current_tick();
        let snapshot = self
            .authority
            .snapshot_at(tick)
            .expect("authority retains its current snapshot");
        let transfer_id = TransferId::new(self.next_transfer_id).unwrap();
        self.next_transfer_id += 1;
        let mut windows = [CommittedSeatInputWindow::default(); MAX_SEATS];
        if tick == SimTick::ZERO {
            for (index, assignment) in self.manifest.ownership.as_slice().iter().enumerate() {
                windows[index] =
                    CommittedSeatInputWindow::from_newest_first(&[CommittedInputRecord {
                        frame: InputFrame {
                            tick,
                            seat: assignment.seat,
                            ..InputFrame::default()
                        },
                        fighter: assignment.fighter,
                        source: CommittedInputSource::MissingSubstitute,
                    }])
                    .unwrap();
            }
        } else {
            let tail_len = self
                .manifest
                .ownership
                .as_slice()
                .iter()
                .map(|assignment| self.committed_histories[usize::from(assignment.seat.get())].len)
                .min()
                .unwrap()
                .min(MAX_RESYNC_INPUT_TAIL_TICKS);
            assert!(tail_len > 0);
            for (index, assignment) in self.manifest.ownership.as_slice().iter().enumerate() {
                windows[index] =
                    self.committed_histories[usize::from(assignment.seat.get())].window(tail_len);
            }
        }
        let transfer = AuthorityResyncTransfer::from_snapshot(
            request,
            transfer_id,
            snapshot,
            &windows[..self.manifest.ownership.len()],
        )
        .unwrap();
        self.transfers[index] = Some(PendingAuthorityTransfer {
            transfer,
            begin_sent: false,
            input_tail_sent: false,
            next_chunk: 0,
        });
        self.counters.resync_transfers = self.counters.resync_transfers.saturating_add(1);
    }

    fn service_authority_transfers(&mut self) {
        for index in 0..self.transfers.len() {
            let Some(pending) = self.transfers[index].as_mut() else {
                continue;
            };
            let peer_id = self.clients[index].peer_id;
            if !pending.begin_sent {
                self.network
                    .queue_authority(peer_id, WireMessage::ResyncBegin(pending.transfer.begin()))
                    .unwrap();
                pending.begin_sent = true;
                continue;
            }
            if !pending.input_tail_sent {
                self.network
                    .queue_authority(
                        peer_id,
                        WireMessage::ResyncInputTail(pending.transfer.input_tail()),
                    )
                    .unwrap();
                pending.input_tail_sent = true;
                continue;
            }
            if pending.next_chunk >= pending.transfer.begin().chunk_count {
                continue;
            }
            let chunk = pending
                .transfer
                .chunks_from(pending.next_chunk)
                .unwrap()
                .next()
                .unwrap();
            self.network
                .queue_authority(peer_id, WireMessage::ResyncChunk(chunk))
                .unwrap();
            pending.next_chunk += 1;
        }
    }

    fn queue_initial_input_windows(&mut self) {
        for client in &mut self.clients {
            let mut windows = [SeatInputWindow::default(); MAX_SEATS];
            let mut count = 0;
            for assignment in self.manifest.ownership.as_slice() {
                if assignment.owner != SeatOwner::Peer(client.peer_id) {
                    continue;
                }
                let seat_index = usize::from(assignment.seat.get());
                client.input_histories[seat_index]
                    .seed_through(SimTick(INPUT_LEAD_TICKS), assignment.seat);
                windows[count] = client.input_histories[seat_index].window();
                count += 1;
            }
            let mut batch =
                InputBatch::new(self.manifest.match_id, client.peer_id, &windows[..count]).unwrap();
            if let Some(ack) = client.predicted.state_baseline_ack() {
                batch = batch.with_state_baseline_ack(ack).unwrap();
            }
            self.network
                .queue_client(client.peer_id, WireMessage::InputBatch(batch))
                .unwrap();
            self.counters.input_batches_sent += 1;
        }
    }

    fn note_ingest(&mut self, ingest: InputIngestReport) {
        // Delayed redundant frames are expected in the degraded/storm profiles.
        // Identity, ownership, future-window, conflict, and capacity failures are
        // never network-quality outcomes.
        assert_eq!(ingest.rejections.invalid, 0);
        assert_eq!(ingest.rejections.unowned, 0);
        assert_eq!(ingest.rejections.future, 0);
        assert_eq!(ingest.rejections.sequence, 0);
        assert_eq!(ingest.rejections.conflicting, 0);
        assert_eq!(ingest.rejections.capacity, 0);
        self.counters.maximum_authority_rejections = self
            .counters
            .maximum_authority_rejections
            .max(ingest.rejected);
    }

    fn client_index(&self, peer_id: PeerId) -> usize {
        self.clients
            .iter()
            .position(|client| client.peer_id == peer_id)
            .expect("runtime event belongs to a configured client")
    }

    fn queue_gameplay_inputs(&mut self) {
        let newest_tick = SimTick(
            self.authority
                .simulation()
                .current_tick()
                .get()
                .saturating_add(INPUT_LEAD_TICKS + 1),
        );
        for client in &mut self.clients {
            let mut windows = [SeatInputWindow::default(); MAX_SEATS];
            let mut count = 0;
            for assignment in self.manifest.ownership.as_slice() {
                if assignment.owner != SeatOwner::Peer(client.peer_id) {
                    continue;
                }
                let seat_index = usize::from(assignment.seat.get());
                client.input_histories[seat_index].push(tape_frame(newest_tick, assignment.seat));
                windows[count] = client.input_histories[seat_index].window();
                count += 1;
            }
            let mut batch =
                InputBatch::new(self.manifest.match_id, client.peer_id, &windows[..count]).unwrap();
            if let Some(acknowledgement) = client.predicted.state_baseline_ack() {
                batch = batch.with_state_baseline_ack(acknowledgement).unwrap();
            }
            self.network
                .queue_client(client.peer_id, WireMessage::InputBatch(batch))
                .unwrap();
            self.counters.input_batches_sent += 1;
        }
    }

    fn predict_clients_through(&mut self, target: SimTick) {
        for client in &mut self.clients {
            let mut steps = 0_u64;
            while client.predicted.predicted_tick().unwrap() < target {
                let tick = client.predicted.predicted_tick().unwrap().next();
                let mut provided = [None; MAX_SEATS];
                for assignment in self.manifest.ownership.as_slice() {
                    if assignment.owner == SeatOwner::Peer(client.peer_id) {
                        provided[usize::from(assignment.seat.get())] =
                            Some(tape_frame(tick, assignment.seat));
                    }
                }
                client.predicted.predict_next(provided).unwrap();
                steps += 1;
            }
            client.maximum_prediction_steps_in_network_tick =
                client.maximum_prediction_steps_in_network_tick.max(steps);
        }
    }

    fn authority_state_message(&self, report: &AuthorityTickReport) -> StateHashAndAcks {
        let acknowledgement = self.authority.processed_input_acknowledgement();
        let mut acks = [ProcessedInputAck::default(); MAX_SEATS];
        let mut count = 0;
        for seat in acknowledgement.as_slice() {
            let Some(processed) = seat.processed_input else {
                continue;
            };
            acks[count] = ProcessedInputAck {
                seat: seat.seat,
                processed_through: processed.tick,
                sequence: processed.sequence,
            };
            count += 1;
        }
        StateHashAndAcks::new(
            self.manifest.match_id,
            report.tick,
            report.state_hash,
            &acks[..count],
        )
        .unwrap()
    }

    fn committed_relay(&mut self, report: &AuthorityTickReport) -> CommittedInputRelay {
        for record in report.committed_inputs.iter() {
            let source = match record.origin {
                AuthorityInputOrigin::Peer(peer_id) => CommittedInputSource::Peer(peer_id),
                AuthorityInputOrigin::AuthorityBot | AuthorityInputOrigin::DisconnectedBot(_) => {
                    CommittedInputSource::AuthorityBot
                }
                AuthorityInputOrigin::MissingSubstitute => CommittedInputSource::MissingSubstitute,
            };
            self.committed_histories[usize::from(record.frame.seat.get())].push(
                CommittedInputRecord {
                    frame: record.frame,
                    fighter: record.fighter,
                    source,
                },
            );
        }
        let mut windows = [CommittedSeatInputWindow::default(); MAX_SEATS];
        for (index, assignment) in self.manifest.ownership.as_slice().iter().enumerate() {
            windows[index] = self.committed_histories[usize::from(assignment.seat.get())]
                .window(self.committed_relay_frames);
        }
        CommittedInputRelay::new(
            self.manifest.match_id,
            report.tick,
            &windows[..self.manifest.ownership.len()],
        )
        .unwrap()
    }

    fn emit_authority_update(&mut self, report: &AuthorityTickReport) {
        let snapshot = self
            .authority
            .snapshot_at(report.tick)
            .expect("authority retains the just-produced snapshot")
            .clone();
        self.state_history.record_snapshot(&snapshot).unwrap();

        let relay = self.committed_relay(report);
        self.network
            .broadcast_authority(WireMessage::CommittedInputRelay(relay))
            .unwrap();
        self.last_relay = Some(relay);
        self.counters.committed_relays_sent += self.clients.len() as u64;

        let state = self.authority_state_message(report);
        self.last_state = Some(state);
        let delta_due =
            report.tick.get() % DELTA_INTERVAL_TICKS == 0 || report.final_result_id.is_some();
        let mut resync_requests = Vec::new();
        for index in 0..self.clients.len() {
            let peer_id = self.clients[index].peer_id;
            let message = if delta_due {
                match self
                    .state_sync
                    .build_latest_for_peer(&mut self.state_history, peer_id, state.as_slice())
                    .unwrap()
                {
                    PeerStateUpdateOutcome::Delta { message, .. } => {
                        self.counters.state_deltas_sent += 1;
                        WireMessage::StateDeltaAndAcks(message)
                    }
                    PeerStateUpdateOutcome::AwaitingBaselineAcknowledgement { .. } => {
                        self.counters.state_hashes_sent += 1;
                        WireMessage::StateHashAndAcks(state)
                    }
                    PeerStateUpdateOutcome::FullResyncRequired { required, .. } => {
                        // Latest-wins state updates may continue observing the
                        // same expired acknowledgement while a reliable full
                        // snapshot is in flight. Keep exactly one authority-
                        // initiated transfer active; an explicit newer client
                        // request can still supersede a completed-but-stale one.
                        if self.transfers[index].is_none() {
                            resync_requests.push((
                                peer_id,
                                crate::network_protocol::ResyncRequest {
                                    match_id: self.manifest.match_id,
                                    peer_id,
                                    reason: ResyncReason::HistoryExpired,
                                    last_confirmed_tick: required.acknowledged.tick,
                                    last_confirmed_hash: required.acknowledged.hash,
                                },
                            ));
                        }
                        self.counters.state_hashes_sent += 1;
                        WireMessage::StateHashAndAcks(state)
                    }
                }
            } else {
                self.counters.state_hashes_sent += 1;
                WireMessage::StateHashAndAcks(state)
            };
            self.network
                .queue_authority(peer_id, message.clone())
                .unwrap();
            self.last_state_messages[index] = Some(message);
        }
        for (peer_id, request) in resync_requests {
            self.begin_transfer(peer_id, request);
        }

        if let Some(result_id) = report.final_result_id {
            let result = ResultIdentifier {
                match_id: self.manifest.match_id,
                result_id: ResultId::new(result_id).unwrap(),
                final_tick: report.tick,
                final_state_hash: report.state_hash,
            };
            self.network
                .broadcast_authority(WireMessage::ResultIdentifier(result))
                .unwrap();
            self.result = Some(result);
        }
    }

    fn run_authority_tick(&mut self, advance_network: &mut impl FnMut(u64)) -> AuthorityTickReport {
        self.advance_network_clock(advance_network);
        self.queue_gameplay_inputs();
        self.pump_round();
        let next_tick = self.authority.simulation().current_tick().next();
        self.predict_clients_through(next_tick);
        let report = self.authority.step().unwrap();
        self.emit_authority_update(&report);
        self.service_authority_transfers();
        self.network.pump_authorities(self.network_tick);
        self.network.pump_clients(self.network_tick);
        self.drain_client_events();
        self.poll_clients();
        self.service_client_outbound();
        report
    }

    fn resend_final_frontier(&mut self) {
        if let Some(relay) = self.last_relay {
            self.network
                .broadcast_authority(WireMessage::CommittedInputRelay(relay))
                .unwrap();
        }
        let state = self
            .last_state
            .expect("result includes a final state identity");
        let mut resync_requests = Vec::new();
        for index in 0..self.clients.len() {
            let peer_id = self.clients[index].peer_id;
            let message = match self
                .state_sync
                .build_latest_for_peer(&mut self.state_history, peer_id, state.as_slice())
                .unwrap()
            {
                PeerStateUpdateOutcome::Delta { message, .. } => {
                    WireMessage::StateDeltaAndAcks(message)
                }
                PeerStateUpdateOutcome::AwaitingBaselineAcknowledgement { .. } => {
                    WireMessage::StateHashAndAcks(state)
                }
                PeerStateUpdateOutcome::FullResyncRequired { required, .. } => {
                    if self.transfers[index].is_none() {
                        resync_requests.push((
                            peer_id,
                            crate::network_protocol::ResyncRequest {
                                match_id: self.manifest.match_id,
                                peer_id,
                                reason: ResyncReason::HistoryExpired,
                                last_confirmed_tick: required.acknowledged.tick,
                                last_confirmed_hash: required.acknowledged.hash,
                            },
                        ));
                    }
                    WireMessage::StateHashAndAcks(state)
                }
            };
            self.network
                .queue_authority(peer_id, message.clone())
                .unwrap();
            self.last_state_messages[index] = Some(message);
        }
        for (peer_id, request) in resync_requests {
            self.begin_transfer(peer_id, request);
        }
    }

    fn has_final_parity(&self) -> bool {
        let Some(result) = self.result else {
            return false;
        };
        self.clients.iter().all(|client| {
            client.confirmed_result
                == Some(ConfirmedSessionResult {
                    result_id: result.result_id.get(),
                    final_tick: result.final_tick,
                    final_hash: result.final_state_hash,
                })
                && client.predicted.predicted_tick() == Some(result.final_tick)
                && client.predicted.confirmed_tick() == Some(result.final_tick)
                && client
                    .predicted
                    .world()
                    .state_hash()
                    .is_ok_and(|hash| hash == result.final_state_hash.0)
        })
    }

    fn drain_result(&mut self, advance_network: &mut impl FnMut(u64)) -> u64 {
        let final_tick = self
            .result
            .expect("result drain follows a result")
            .final_tick;
        for drain_tick in 0..MAX_RESULT_DRAIN_TICKS {
            if self.has_final_parity() {
                return drain_tick;
            }
            self.advance_network_clock(advance_network);
            self.resend_final_frontier();
            self.pump_round();
            self.predict_clients_through(final_tick);
        }
        for client in &self.clients {
            eprintln!(
                "peer={} predicted={:?} confirmed={:?} hash={:?} result={:?} request={} in_flight={} post_sync={} baseline={:?} prediction_metrics={:?}",
                client.peer_id.get(),
                client.predicted.predicted_tick(),
                client.predicted.confirmed_tick(),
                client.predicted.world().state_hash(),
                client.confirmed_result,
                client.pending_request.is_some(),
                client.resync_in_flight,
                client.pending_post_sync.is_some(),
                client.predicted.acknowledged_baseline(),
                client.predicted.metrics(),
            );
        }
        panic!("clients did not converge to the authoritative result within the drain bound");
    }

    fn run_match(
        &mut self,
        scenario: NetworkAcceptanceScenario,
        finish_tick: u64,
        advance_network: &mut impl FnMut(u64),
    ) -> ScenarioReport {
        self.start(advance_network);
        let traffic_before = self.network.traffic();
        let mut last = None;
        for _ in 0..finish_tick {
            let report = self.run_authority_tick(advance_network);
            last = Some(report);
            if report.final_result_id.is_some() {
                break;
            }
        }
        let last = last.expect("scenario advances at least one authority tick");
        assert_eq!(last.tick, SimTick(finish_tick));
        assert_eq!(last.final_result_id, Some(RESULT_ID));
        let traffic_at_result = self.network.traffic();
        let drain_ticks = self.drain_result(advance_network);
        assert!(self.has_final_parity());

        let peer_metrics = self
            .clients
            .iter()
            .map(|client| PeerScenarioReport {
                peer_id: client.peer_id,
                rollback: client.rollback_metrics(),
                prediction: client.predicted_metrics(),
                maximum_prediction_steps_in_network_tick: client
                    .maximum_prediction_steps_in_network_tick,
                traffic: traffic_delta(&traffic_before, &traffic_at_result, client.peer_id),
            })
            .collect();
        ScenarioReport {
            scenario,
            authority_tick: last.tick,
            authority_hash: last.state_hash,
            result_id: last.final_result_id.unwrap(),
            result_drain_ticks: drain_ticks,
            peers: peer_metrics,
            counters: self.counters,
        }
    }
}

#[derive(Clone, Debug)]
struct PeerScenarioReport {
    peer_id: PeerId,
    rollback: RollbackMetrics,
    prediction: PredictedClientMetrics,
    maximum_prediction_steps_in_network_tick: u64,
    traffic: PeerTrafficSnapshot,
}

#[derive(Clone, Debug)]
struct ScenarioReport {
    scenario: NetworkAcceptanceScenario,
    authority_tick: SimTick,
    authority_hash: StateHash,
    result_id: u64,
    result_drain_ticks: u64,
    peers: Vec<PeerScenarioReport>,
    counters: LabCounters,
}

fn traffic_delta(
    before: &[PeerTrafficSnapshot],
    after: &[PeerTrafficSnapshot],
    peer_id: PeerId,
) -> PeerTrafficSnapshot {
    let before = before
        .iter()
        .copied()
        .find(|traffic| traffic.peer_id == peer_id)
        .unwrap();
    let after = after
        .iter()
        .copied()
        .find(|traffic| traffic.peer_id == peer_id)
        .unwrap();
    after.saturating_sub(before)
}

fn manifest_peer_ids(manifest: &MatchManifest) -> Vec<PeerId> {
    let mut peers = Vec::new();
    for assignment in manifest.ownership.as_slice() {
        let SeatOwner::Peer(peer_id) = assignment.owner else {
            continue;
        };
        if !peers.contains(&peer_id) {
            peers.push(peer_id);
        }
    }
    peers
}

fn udp_endpoint_pairs(manifest: &MatchManifest) -> Vec<PeerEndpointPair<UdpEndpoint>> {
    manifest_peer_ids(manifest)
        .into_iter()
        .map(|peer_id| {
            let (client, authority) = UdpEndpoint::loopback_pair().unwrap();
            assert!(client.local_addr().unwrap().ip().is_loopback());
            assert!(authority.local_addr().unwrap().ip().is_loopback());
            PeerEndpointPair {
                peer_id,
                client,
                authority,
            }
        })
        .collect()
}

fn fault_endpoint_pairs(
    manifest: &MatchManifest,
    config: FaultLabConfig,
) -> (
    Vec<DeterministicNetworkLab>,
    Vec<PeerEndpointPair<FaultLabEndpoint>>,
) {
    let mut labs = Vec::new();
    let mut pairs = Vec::new();
    for (index, peer_id) in manifest_peer_ids(manifest).into_iter().enumerate() {
        let per_peer = FaultLabConfig::new(
            config.a_to_b,
            config.b_to_a,
            config.a_to_b_seed ^ index as u64,
            config.b_to_a_seed ^ (index as u64).rotate_left(17),
        );
        let (lab, client, authority) = DeterministicNetworkLab::pair(per_peer).unwrap();
        labs.push(lab);
        pairs.push(PeerEndpointPair {
            peer_id,
            client,
            authority,
        });
    }
    (labs, pairs)
}

fn degraded_fault_config(seed: u64) -> FaultLabConfig {
    FaultLabConfig::new(
        FaultConfig {
            base_latency_ticks: 4,
            jitter_ticks: 2,
            loss_per_10k: 300,
            duplication_per_10k: 100,
            reorder_per_10k: 200,
            max_reorder_extra_ticks: 3,
            ..FaultConfig::default()
        },
        FaultConfig {
            base_latency_ticks: 5,
            jitter_ticks: 2,
            loss_per_10k: 300,
            duplication_per_10k: 100,
            reorder_per_10k: 200,
            max_reorder_extra_ticks: 3,
            ..FaultConfig::default()
        },
        seed ^ 0xA2B0,
        seed ^ 0xB2A0,
    )
}

fn assert_acceptance_report(
    report: &ScenarioReport,
    finish_tick: u64,
    enforce_steady_state_budgets: bool,
) {
    eprintln!(
        "acceptance scenario={} ticks={} hash={:#018x} drain_ticks={} hard_requests={} history_requests={} hash_requests={} transfers={} relays={} deltas={} hashes={}",
        report.scenario.name(),
        finish_tick,
        report.authority_hash.0,
        report.result_drain_ticks,
        report.counters.hard_resync_requests,
        report.counters.history_resync_requests,
        report.counters.hash_resync_requests,
        report.counters.resync_transfers,
        report.counters.committed_relays_sent,
        report.counters.state_deltas_sent,
        report.counters.state_hashes_sent,
    );
    assert_eq!(report.authority_tick, SimTick(finish_tick));
    assert_ne!(report.authority_hash, StateHash(0));
    assert_eq!(report.result_id, RESULT_ID);
    assert!(report.result_drain_ticks <= MAX_RESULT_DRAIN_TICKS);
    assert_eq!(report.peers.len(), MAX_NETWORK_PEERS);
    assert_eq!(report.counters.committed_relays_sent, finish_tick * 4);
    assert_eq!(
        report.counters.state_deltas_sent + report.counters.state_hashes_sent,
        finish_tick * 4
    );

    for peer in &report.peers {
        assert!(
            peer.rollback.maximum_normal_rollback_depth <= NORMAL_ROLLBACK_LIMIT,
            "{} attempted rollback depth {} in {} (hard resyncs {})",
            peer.peer_id.get(),
            peer.rollback.maximum_normal_rollback_depth,
            report.scenario.name(),
            peer.prediction.hard_resyncs_applied
        );
        assert!(peer.rollback.snapshot_history_high_water <= PREDICTION_HISTORY);
        assert!(peer.rollback.input_history_high_water <= PREDICTION_HISTORY);
        assert!(peer.prediction.hard_resyncs_applied <= report.counters.resync_transfers);
        let corrections = peer
            .rollback
            .corrections
            .saturating_add(peer.rollback.late_input_corrections);
        if corrections != 0 {
            assert!(
                peer.rollback.resimulated_ticks
                    <= corrections.saturating_mul(NORMAL_ROLLBACK_LIMIT)
            );
        }

        let upstream = average_bytes_per_second(peer.traffic.upstream_bytes(), finish_tick);
        let downstream = average_bytes_per_second(peer.traffic.downstream_bytes(), finish_tick);
        eprintln!(
            "acceptance peer={} up_bps={} down_bps={} normal_rollback_max={} hard_applied={} stale_resync_baselines={} prediction_burst={}",
            peer.peer_id.get(),
            upstream,
            downstream,
            peer.rollback.maximum_normal_rollback_depth,
            peer.prediction.hard_resyncs_applied,
            peer.prediction.stale_resync_baselines_accepted,
            peer.maximum_prediction_steps_in_network_tick,
        );
        if enforce_steady_state_budgets {
            assert!(
                upstream <= UPSTREAM_BUDGET_BYTES_PER_SECOND,
                "{} upstream {} B/s exceeded {} B/s in {}",
                peer.peer_id.get(),
                upstream,
                UPSTREAM_BUDGET_BYTES_PER_SECOND,
                report.scenario.name()
            );
            assert!(
                downstream <= DOWNSTREAM_BUDGET_BYTES_PER_SECOND,
                "{} downstream {} B/s exceeded {} B/s in {}: control={} input={} state={} resync={} result={} hard={} transfers={}",
                peer.peer_id.get(),
                downstream,
                DOWNSTREAM_BUDGET_BYTES_PER_SECOND,
                report.scenario.name(),
                peer.traffic
                    .authority
                    .sent
                    .channel(AfcChannel::Control)
                    .bytes,
                peer.traffic.authority.sent.channel(AfcChannel::Input).bytes,
                peer.traffic.authority.sent.channel(AfcChannel::State).bytes,
                peer.traffic
                    .authority
                    .sent
                    .channel(AfcChannel::Resync)
                    .bytes,
                peer.traffic
                    .authority
                    .sent
                    .channel(AfcChannel::Result)
                    .bytes,
                peer.prediction.hard_resyncs_applied,
                report.counters.resync_transfers,
            );
        }
        assert!(
            peer.traffic
                .client
                .sent
                .channel(AfcChannel::Input)
                .datagrams
                > 0
        );
        assert!(
            peer.traffic
                .authority
                .sent
                .channel(AfcChannel::Input)
                .datagrams
                > 0
        );
        assert!(
            peer.traffic
                .authority
                .sent
                .channel(AfcChannel::State)
                .datagrams
                > 0
        );
        assert_eq!(peer.traffic.client.sent.unclassified.datagrams, 0);
        assert_eq!(peer.traffic.authority.sent.unclassified.datagrams, 0);
    }
}

fn assert_runtime_bounds<E: NonBlockingDatagramEndpoint>(harness: &ScenarioHarness<E>) {
    for metrics in harness.network.runtime_metrics() {
        for runtime in [metrics.client, metrics.authority] {
            assert!(runtime.inbound_high_water <= 64);
            assert!(runtime.outbound_high_water <= 64);
            assert!(runtime.reliable_high_water <= 32);
            assert_eq!(runtime.inbound_queue_overflows, 0);
            assert_eq!(runtime.outbound_queue_overflows, 0);
            assert_eq!(runtime.reliable_reorder_overflows, 0);
        }
    }
}

#[test]
fn net_loopback4() {
    // The ordinary-UDP pre-Steam gate runs ten simulated minutes at 60 Hz.
    const MATCH_TICKS: u64 = 10 * 60 * 60;
    let manifest = four_peer_manifest();
    let endpoints = udp_endpoint_pairs(&manifest);
    let mut harness = ScenarioHarness::new(manifest, endpoints, MATCH_TICKS);
    let report = harness.run_match(
        NetworkAcceptanceScenario::NetLoopback4,
        MATCH_TICKS,
        &mut |_| {},
    );
    assert_acceptance_report(&report, MATCH_TICKS, true);
    assert_eq!(report.counters.hard_resync_requests, 0);
    assert_runtime_bounds(&harness);
    for peer in report.peers {
        assert!(peer.rollback.maximum_normal_rollback_depth <= INPUT_LEAD_TICKS);
        assert!(peer.maximum_prediction_steps_in_network_tick <= 1);
    }
}

#[test]
fn net_typical4() {
    const MATCH_TICKS: u64 = 600;
    let manifest = four_peer_manifest();
    let (labs, endpoints) =
        fault_endpoint_pairs(&manifest, FaultLabConfig::net_typical_60hz(0x711C_A1));
    let clock_labs = labs.clone();
    let mut harness = ScenarioHarness::new(manifest, endpoints, MATCH_TICKS);
    let report = harness.run_match(
        NetworkAcceptanceScenario::NetTypical4,
        MATCH_TICKS,
        &mut |tick| {
            for lab in &clock_labs {
                lab.advance_to(tick).unwrap();
            }
        },
    );
    assert_acceptance_report(&report, MATCH_TICKS, true);
    assert_runtime_bounds(&harness);
    assert!(labs.iter().any(|lab| {
        let metrics = lab.metrics();
        metrics.a_to_b.dropped_by_loss + metrics.b_to_a.dropped_by_loss > 0
    }));
}

#[test]
fn net_degraded4() {
    const MATCH_TICKS: u64 = 600;
    let manifest = four_peer_manifest();
    let (labs, endpoints) = fault_endpoint_pairs(&manifest, degraded_fault_config(0xDE6A_ADED));
    let clock_labs = labs.clone();
    let mut harness = ScenarioHarness::new(manifest, endpoints, MATCH_TICKS);
    let report = harness.run_match(
        NetworkAcceptanceScenario::NetDegraded4,
        MATCH_TICKS,
        &mut |tick| {
            for lab in &clock_labs {
                lab.advance_to(tick).unwrap();
            }
        },
    );
    assert_acceptance_report(&report, MATCH_TICKS, true);
    assert_runtime_bounds(&harness);
    let aggregate = labs.iter().fold((0, 0, 0), |totals, lab| {
        let metrics = lab.metrics();
        (
            totals.0 + metrics.a_to_b.dropped_by_loss + metrics.b_to_a.dropped_by_loss,
            totals.1 + metrics.a_to_b.duplicate_copies + metrics.b_to_a.duplicate_copies,
            totals.2 + metrics.a_to_b.reordered_copies + metrics.b_to_a.reordered_copies,
        )
    });
    assert!(aggregate.0 > 0, "degraded profile injected no loss");
    assert!(aggregate.1 > 0, "degraded profile injected no duplication");
    assert!(aggregate.2 > 0, "degraded profile injected no reordering");
}

#[test]
fn rollback_storm() {
    const MATCH_TICKS: u64 = 360;
    let manifest = four_peer_manifest();
    // Keep the upstream leg inside the negotiated input lead so this scenario
    // isolates client rollback cost instead of simultaneously exercising
    // authority missing-input substitution. The delayed downstream leg drives
    // remote-seat corrections into the intended 9-12 tick band.
    let storm = FaultLabConfig::new(
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
        0x5702_0001,
        0x5702_0002,
    );
    let (labs, endpoints) = fault_endpoint_pairs(&manifest, storm);
    let clock_labs = labs.clone();
    let mut harness = ScenarioHarness::new(manifest, endpoints, MATCH_TICKS);
    harness.set_committed_relay_frames(MAX_INPUT_FRAMES_PER_WINDOW);
    let report = harness.run_match(
        NetworkAcceptanceScenario::RollbackStorm,
        MATCH_TICKS,
        &mut |tick| {
            for lab in &clock_labs {
                lab.advance_to(tick).unwrap();
            }
        },
    );
    assert_acceptance_report(&report, MATCH_TICKS, true);
    assert_runtime_bounds(&harness);
    assert!(
        report
            .peers
            .iter()
            .all(|peer| peer.rollback.maximum_normal_rollback_depth >= 9)
    );
    assert!(
        report
            .peers
            .iter()
            .all(|peer| peer.prediction.hard_resyncs_applied == 0)
    );
}

#[test]
fn relay_gap_forces_bounded_hard_resync_and_recovers_final_parity() {
    const MATCH_TICKS: u64 = 180;
    let manifest = four_peer_manifest();
    let burst = FaultLabConfig::symmetric(
        FaultConfig {
            base_latency_ticks: 1,
            delivery_burst_interval_ticks: 16,
            ..FaultConfig::default()
        },
        0xB025_7001,
    );
    let (labs, endpoints) = fault_endpoint_pairs(&manifest, burst);
    let clock_labs = labs.clone();
    let mut harness = ScenarioHarness::new(manifest, endpoints, MATCH_TICKS);
    let report = harness.run_match(
        NetworkAcceptanceScenario::NetDegraded4,
        MATCH_TICKS,
        &mut |tick| {
            for lab in &clock_labs {
                lab.advance_to(tick).unwrap();
            }
        },
    );
    // Sixteen-tick delivery bursts intentionally force repeated full snapshots;
    // this is a recovery/queue-bound gate, not a steady-state bandwidth profile.
    assert_acceptance_report(&report, MATCH_TICKS, false);
    assert!(report.counters.hard_resync_requests > 0);
    assert!(
        report
            .peers
            .iter()
            .any(|peer| peer.prediction.hard_resyncs_applied > 0)
    );
    assert_runtime_bounds(&harness);
}

#[test]
fn coordinator_preserves_couch_coop_ownership_on_one_connection() {
    let manifest = couch_coop_manifest();
    let endpoint_pairs = manifest_peer_ids(&manifest)
        .into_iter()
        .map(|peer_id| {
            let (client, authority) = InProcessEndpoint::pair(64).unwrap();
            PeerEndpointPair {
                peer_id,
                client,
                authority,
            }
        })
        .collect();
    let coordinator =
        MultiPeerRuntimeCoordinator::new(manifest, endpoint_pairs, RuntimeConfig::default())
            .unwrap();
    assert_eq!(coordinator.peer_count(), 3);
    assert_eq!(coordinator.peer_seat_count(peer_id(0)).unwrap(), 2);
    assert_eq!(coordinator.peer_seat_count(peer_id(1)).unwrap(), 1);
    assert_eq!(coordinator.peer_seat_count(peer_id(2)).unwrap(), 1);
}

#[test]
fn malformed_datagram_flood_stays_within_runtime_bounds() {
    let (mut attacker, receiver) = InProcessEndpoint::pair(16).unwrap();
    let mut runtime = NetworkRuntime::new(
        receiver,
        PeerRole::Authority,
        compatibility(),
        RuntimeConfig {
            inbound_capacity: 8,
            outbound_capacity: 8,
            abuse_warning_threshold: 4,
            abuse_disconnect_threshold: 32,
            ..RuntimeConfig::default()
        },
    )
    .unwrap();
    let mut sent = 0_u64;
    for ordinal in 0..512_u64 {
        let len = (ordinal as usize % 1_200) + 1;
        let mut bytes = vec![0_u8; len];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = ordinal.wrapping_mul(0x9E37_79B9).wrapping_add(index as u64) as u8;
        }
        let datagram = AfcDatagram::try_from_slice(&bytes).unwrap();
        match attacker.try_send(datagram) {
            SendOutcome::Sent => sent += 1,
            SendOutcome::Full(_) => {
                runtime.pump(SimTick(ordinal));
            }
            other => panic!("unexpected attacker send result: {other:?}"),
        }
        runtime.pump(SimTick(ordinal));
        while runtime.try_next_event().is_some() {}
    }
    assert!(sent > 0);
    assert!(runtime.metrics().malformed_datagrams > 0);
    assert!(runtime.metrics().inbound_high_water <= 8);
    assert!(runtime.metrics().reliable_high_water <= 16);
    assert_eq!(runtime.inbound_len(), 0);
}

#[test]
fn deterministic_disconnect_is_explicit_and_bounded() {
    let config = FaultConfig {
        disconnect: Some(DisconnectWindow {
            start_tick: 5,
            reconnect_tick: Some(10),
        }),
        ..FaultConfig::default()
    };
    let (lab, left_endpoint, right_endpoint) =
        DeterministicNetworkLab::pair(FaultLabConfig::symmetric(config, 0xD15C_0)).unwrap();
    let mut left = NetworkRuntime::new(
        left_endpoint,
        PeerRole::Client,
        compatibility(),
        RuntimeConfig::default(),
    )
    .unwrap();
    let mut right = NetworkRuntime::new(
        right_endpoint,
        PeerRole::Authority,
        compatibility(),
        RuntimeConfig::default(),
    )
    .unwrap();
    for tick in 0..=5 {
        lab.advance_to(tick).unwrap();
        left.pump(SimTick(tick));
        right.pump(SimTick(tick));
    }
    assert_eq!(
        left.connection_state(),
        RuntimeConnectionState::TransportDisconnected
    );
    assert_eq!(
        right.connection_state(),
        RuntimeConnectionState::TransportDisconnected
    );
    let metrics = lab.metrics();
    assert!(metrics.a_to_b.disconnected_receive_attempts > 0);
    assert!(metrics.b_to_a.disconnected_receive_attempts > 0);
}

#[test]
fn deterministic_bandwidth_bucket_delays_without_unbounded_queue_growth() {
    let config = FaultConfig {
        bandwidth_bytes_per_tick: 1_200,
        bandwidth_burst_bytes: 1_200,
        queue_capacity_packets: 16,
        ..FaultConfig::default()
    };
    let (lab, mut sender, mut receiver) =
        DeterministicNetworkLab::pair(FaultLabConfig::symmetric(config, 0xBAAD_7100)).unwrap();
    let packet = AfcDatagram::try_from_slice(&[0x5A; 1_000]).unwrap();
    for _ in 0..8 {
        assert_eq!(sender.try_send(packet.clone()), SendOutcome::Sent);
    }
    for tick in 0..8 {
        lab.advance_to(tick).unwrap();
        let mut delivered_this_tick = 0;
        while let crate::network_io::ReceiveOutcome::Received(datagram) = receiver.try_receive() {
            delivered_this_tick += datagram.len();
        }
        assert!(delivered_this_tick <= 1_200);
        assert!(lab.metrics().a_to_b.pending_datagrams <= 16);
    }
    let metrics = lab.metrics();
    assert_eq!(metrics.a_to_b.delivered_datagrams, 8);
    assert!(metrics.a_to_b.pending_high_water <= 16);
}
