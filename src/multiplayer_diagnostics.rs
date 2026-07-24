//! Shipping persistence boundary for privacy-safe authority diagnostics.
//!
//! The values in this module deliberately mirror only protocol identifiers and
//! bounded numeric metrics. Steam/account identities, authentication tickets,
//! network addresses, backend strings, packet payloads, and persona names cannot
//! be represented by an incident or operational snapshot.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use crate::authority::AuthorityTickReport;
use crate::authority_input::{AuthorityInputOrigin, AuthorityInputStatus};
use crate::authority_peer_hub::AuthorityPeerHubMetrics;
use crate::multiplayer_observability::{
    MultiplayerCounterSnapshot, OnlineAuditLog, OnlineAuditMetrics, OnlineAuditRecord,
    ServerTickDistribution,
};
use crate::multiplayer_security::PeerSecurityMetrics;
use crate::network_protocol::{CompatibilityId, MatchManifest, SimTick};
use crate::online_failure::{
    OnlineFailure, OnlineFailureCode, OnlineFailureSeverity, OnlineRecoveryAction,
};
use crate::replay::{Replay, ReplayInputSource};
use crate::replay_archive::{ReplayArchive, ReplayArchiveError, StoredReplay};
use crate::snapshot::{CanonicalSnapshot, MAX_SNAPSHOT_BYTES, SnapshotError};

pub const DIAGNOSTICS_ROOT_ENV: &str = "AFC_DIAGNOSTICS_ROOT";
pub const REPLAY_CHECKPOINT_INTERVAL_TICKS: u64 = 60;
pub const REPLAY_KEYFRAME_INTERVAL_TICKS: u64 = 1_800;
pub const INCIDENT_INPUT_TAIL_TICKS: usize = 120;
pub const MAX_INCIDENT_BYTES: usize = 2 * 1_024 * 1_024;
pub const MAX_INCIDENT_FILES: usize = 8;
pub const MAX_INCIDENT_TOTAL_BYTES: u64 = 16 * 1_024 * 1_024;
pub const MAX_OPERATIONAL_BYTES: usize = 64 * 1_024;
pub const MAX_OPERATIONAL_FILES: usize = 16;
pub const MAX_OPERATIONAL_TOTAL_BYTES: u64 = 1 * 1_024 * 1_024;
pub const MAX_DIAGNOSTIC_DIRECTORY_ENTRIES: usize = 128;
pub const DIAGNOSTIC_CRITICAL_QUEUE_CAPACITY: usize = 4;
pub const DIAGNOSTIC_PERIODIC_QUEUE_CAPACITY: usize = 1;

const INCIDENT_SCHEMA_VERSION: u16 = 1;
const OPERATIONAL_SCHEMA_VERSION: u16 = 1;
const INCIDENT_EXTENSION: &str = "afci";
const OPERATIONAL_EXTENSION: &str = "afco";
const TEMP_CREATE_ATTEMPTS: u64 = 32;
static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticsCounterSnapshot {
    pub jobs_queued: u64,
    pub periodic_jobs_dropped: u64,
    pub replays_persisted: u64,
    pub incidents_persisted: u64,
    pub operational_snapshots_persisted: u64,
    pub files_pruned: u64,
    pub persistence_failures: u64,
    pub recorder_failures: u64,
    pub writer_start_failures: u64,
    pub writer_join_failures: u64,
    pub invalid_root_overrides: u64,
}

#[derive(Default)]
pub struct DiagnosticsCounters {
    jobs_queued: AtomicU64,
    periodic_jobs_dropped: AtomicU64,
    replays_persisted: AtomicU64,
    incidents_persisted: AtomicU64,
    operational_snapshots_persisted: AtomicU64,
    files_pruned: AtomicU64,
    persistence_failures: AtomicU64,
    recorder_failures: AtomicU64,
    writer_start_failures: AtomicU64,
    writer_join_failures: AtomicU64,
    invalid_root_overrides: AtomicU64,
}

impl DiagnosticsCounters {
    pub fn snapshot(&self) -> DiagnosticsCounterSnapshot {
        DiagnosticsCounterSnapshot {
            jobs_queued: self.jobs_queued.load(Ordering::Relaxed),
            periodic_jobs_dropped: self.periodic_jobs_dropped.load(Ordering::Relaxed),
            replays_persisted: self.replays_persisted.load(Ordering::Relaxed),
            incidents_persisted: self.incidents_persisted.load(Ordering::Relaxed),
            operational_snapshots_persisted: self
                .operational_snapshots_persisted
                .load(Ordering::Relaxed),
            files_pruned: self.files_pruned.load(Ordering::Relaxed),
            persistence_failures: self.persistence_failures.load(Ordering::Relaxed),
            recorder_failures: self.recorder_failures.load(Ordering::Relaxed),
            writer_start_failures: self.writer_start_failures.load(Ordering::Relaxed),
            writer_join_failures: self.writer_join_failures.load(Ordering::Relaxed),
            invalid_root_overrides: self.invalid_root_overrides.load(Ordering::Relaxed),
        }
    }

    pub fn observe_recorder_failure(&self) {
        self.recorder_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn observe_writer_start_failure(&self) {
        self.writer_start_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn observe_invalid_root_override(&self) {
        self.invalid_root_overrides.fetch_add(1, Ordering::Relaxed);
    }

    pub fn observe_persistence_failure(&self) {
        self.persistence_failures.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticsRootResolution {
    pub path: PathBuf,
    pub invalid_override: bool,
}

/// Resolves an absolute diagnostics root outside the repository. An explicit
/// relative override is rejected and the platform user-data default is used.
pub fn resolve_diagnostics_root() -> DiagnosticsRootResolution {
    if let Some(override_path) = std::env::var_os(DIAGNOSTICS_ROOT_ENV) {
        let path = PathBuf::from(override_path);
        if path.is_absolute() {
            return DiagnosticsRootResolution {
                path,
                invalid_override: false,
            };
        }
        return DiagnosticsRootResolution {
            path: default_diagnostics_root(),
            invalid_override: true,
        };
    }
    DiagnosticsRootResolution {
        path: default_diagnostics_root(),
        invalid_override: false,
    }
}

fn default_diagnostics_root() -> PathBuf {
    #[cfg(target_os = "windows")]
    if let Some(base) = absolute_environment_path("LOCALAPPDATA") {
        return base.join("AFC").join("diagnostics");
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = absolute_environment_path("HOME") {
        return home
            .join("Library")
            .join("Application Support")
            .join("AFC")
            .join("diagnostics");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(base) = absolute_environment_path("XDG_STATE_HOME") {
            return base.join("afc").join("diagnostics");
        }
        if let Some(home) = absolute_environment_path("HOME") {
            return home
                .join(".local")
                .join("state")
                .join("afc")
                .join("diagnostics");
        }
    }
    std::env::temp_dir()
        .join("afc-user-data")
        .join("diagnostics")
}

#[cfg(not(target_arch = "wasm32"))]
fn absolute_environment_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityDiagnostic {
    pub protocol: u16,
    pub simulation: u16,
    pub replay: u16,
    pub build: [u8; 16],
    pub gameplay_content: [u8; 32],
}

impl From<CompatibilityId> for CompatibilityDiagnostic {
    fn from(value: CompatibilityId) -> Self {
        Self {
            protocol: value.protocol.get(),
            simulation: value.simulation.get(),
            replay: value.replay.get(),
            build: *value.build.as_bytes(),
            gameplay_content: *value.gameplay_content.as_bytes(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityWorkerDiagnosticSnapshot {
    pub command_queue_capacity: u64,
    pub command_queue_depth: u64,
    pub command_queue_high_water: u64,
    pub commands_queued: u64,
    pub commands_full: u64,
    pub commands_disconnected: u64,
    pub commands_processed: u64,
    pub worker_iterations: u64,
    pub simulated_ticks: u64,
    pub waiting_iterations: u64,
    pub late_tick_starts: u64,
    pub maximum_tick_lateness_ns: u64,
    pub total_service_duration_ns: u64,
    pub maximum_service_duration_ns: u64,
    pub over_budget_iterations: u64,
    pub status_publications: u64,
    pub status_notifications_coalesced: u64,
    pub events_published: u64,
    pub events_dropped: u64,
    pub event_queue_high_water: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubDiagnosticSnapshot {
    pub connections_attached: u64,
    pub authentication_rejections: u64,
    pub active_ban_rejections: u64,
    pub stale_connection_operations: u64,
    pub peers_rejected: u64,
    pub spoofed_messages: u64,
    pub malformed_or_abusive_disconnects: u64,
    #[serde(default)]
    pub post_result_transport_closures: u64,
    pub security_violations: u64,
    pub security_warnings: u64,
    pub security_kicks: u64,
    pub temporary_bans: u64,
    pub platform_bans: u64,
    pub typed_disconnects_queued: u64,
    pub typed_disconnects_deferred: u64,
    pub authority_ticks: u64,
    pub startup_ticks_blocked: u64,
    #[serde(default)]
    pub startup_input_deadlines: u64,
    pub input_batches_accepted: u64,
    pub input_batches_rejected: u64,
    pub state_packets_queued: u64,
    pub state_packets_deferred: u64,
    pub resyncs_started: u64,
    pub repair_requests_coalesced: u64,
    pub resyncs_applied: u64,
    pub reconnects_completed: u64,
    #[serde(default)]
    pub reconnect_grace_expirations: u64,
    pub results_queued: u64,
    pub results_deferred: u64,
}

impl From<AuthorityPeerHubMetrics> for HubDiagnosticSnapshot {
    fn from(value: AuthorityPeerHubMetrics) -> Self {
        Self {
            connections_attached: value.connections_attached,
            authentication_rejections: value.authentication_rejections,
            active_ban_rejections: value.active_ban_rejections,
            stale_connection_operations: value.stale_connection_operations,
            peers_rejected: value.peers_rejected,
            spoofed_messages: value.spoofed_messages,
            malformed_or_abusive_disconnects: value.malformed_or_abusive_disconnects,
            post_result_transport_closures: value.post_result_transport_closures,
            security_violations: value.security_violations,
            security_warnings: value.security_warnings,
            security_kicks: value.security_kicks,
            temporary_bans: value.temporary_bans,
            platform_bans: value.platform_bans,
            typed_disconnects_queued: value.typed_disconnects_queued,
            typed_disconnects_deferred: value.typed_disconnects_deferred,
            authority_ticks: value.authority_ticks,
            startup_ticks_blocked: value.startup_ticks_blocked,
            startup_input_deadlines: value.startup_input_deadlines,
            input_batches_accepted: value.input_batches_accepted,
            input_batches_rejected: value.input_batches_rejected,
            state_packets_queued: value.state_packets_queued,
            state_packets_deferred: value.state_packets_deferred,
            resyncs_started: value.resyncs_started,
            repair_requests_coalesced: value.repair_requests_coalesced,
            resyncs_applied: value.resyncs_applied,
            reconnects_completed: value.reconnects_completed,
            reconnect_grace_expirations: value.reconnect_grace_expirations,
            results_queued: value.results_queued,
            results_deferred: value.results_deferred,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterDiagnosticSnapshot {
    pub packets_in: u64,
    pub packets_out: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub inputs_accepted: u64,
    pub inputs_rejected: u64,
    pub inputs_substituted: u64,
    pub rollbacks: u64,
    pub maximum_rollback_depth: u16,
    pub hard_resyncs: u64,
    pub reconnects: u64,
    pub confirmed_hash_mismatches: u64,
    pub queue_high_water: u32,
    pub history_high_water: u32,
    pub pool_high_water: u32,
}

impl From<MultiplayerCounterSnapshot> for CounterDiagnosticSnapshot {
    fn from(value: MultiplayerCounterSnapshot) -> Self {
        Self {
            packets_in: value.packets_in,
            packets_out: value.packets_out,
            bytes_in: value.bytes_in,
            bytes_out: value.bytes_out,
            inputs_accepted: value.inputs_accepted,
            inputs_rejected: value.inputs_rejected,
            inputs_substituted: value.inputs_substituted,
            rollbacks: value.rollbacks,
            maximum_rollback_depth: value.maximum_rollback_depth,
            hard_resyncs: value.hard_resyncs,
            reconnects: value.reconnects,
            confirmed_hash_mismatches: value.confirmed_hash_mismatches,
            queue_high_water: value.queue_high_water,
            history_high_water: value.history_high_water,
            pool_high_water: value.pool_high_water,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerTickDiagnosticSnapshot {
    pub samples: u16,
    pub p50_ns: u64,
    pub p95_ns: u64,
    pub p99_ns: u64,
    pub maximum_ns: u64,
    pub over_budget: u64,
}

impl From<ServerTickDistribution> for ServerTickDiagnosticSnapshot {
    fn from(value: ServerTickDistribution) -> Self {
        Self {
            samples: value.samples,
            p50_ns: value.p50_ns,
            p95_ns: value.p95_ns,
            p99_ns: value.p99_ns,
            maximum_ns: value.maximum_ns,
            over_budget: value.over_budget,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditMetricsDiagnosticSnapshot {
    pub retained: u16,
    pub written: u64,
    pub overwritten: u64,
}

impl From<OnlineAuditMetrics> for AuditMetricsDiagnosticSnapshot {
    fn from(value: OnlineAuditMetrics) -> Self {
        Self {
            retained: value.retained,
            written: value.written,
            overwritten: value.overwritten,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityDiagnosticSnapshot {
    pub peer_id: u64,
    pub violations: u64,
    pub warnings: u64,
    pub kicks: u64,
    pub temporary_bans: u64,
    pub platform_bans: u64,
    pub peak_score: u16,
}

impl SecurityDiagnosticSnapshot {
    pub fn new(peer_id: u64, metrics: PeerSecurityMetrics) -> Self {
        Self {
            peer_id,
            violations: metrics.violations,
            warnings: metrics.warnings,
            kicks: metrics.kicks,
            temporary_bans: metrics.temporary_bans,
            platform_bans: metrics.platform_bans,
            peak_score: metrics.peak_score,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityOperationalSnapshot {
    pub schema_version: u16,
    pub terminal: bool,
    pub match_id: [u8; 16],
    pub compatibility: CompatibilityDiagnostic,
    pub manifest_hash: u64,
    pub network_tick: u64,
    pub authority_tick: u64,
    pub result_id: Option<u64>,
    pub final_state_hash: Option<u64>,
    pub worker: AuthorityWorkerDiagnosticSnapshot,
    pub hub: HubDiagnosticSnapshot,
    pub observability: CounterDiagnosticSnapshot,
    pub server_ticks: ServerTickDiagnosticSnapshot,
    pub audit: AuditMetricsDiagnosticSnapshot,
    pub diagnostics: DiagnosticsCounterSnapshot,
    pub security: Vec<SecurityDiagnosticSnapshot>,
}

impl AuthorityOperationalSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        manifest: &MatchManifest,
        terminal: bool,
        network_tick: SimTick,
        authority_tick: SimTick,
        result: Option<(u64, u64)>,
        worker: AuthorityWorkerDiagnosticSnapshot,
        hub: AuthorityPeerHubMetrics,
        observability: MultiplayerCounterSnapshot,
        server_ticks: ServerTickDistribution,
        audit: OnlineAuditMetrics,
        diagnostics: DiagnosticsCounterSnapshot,
        security: Vec<SecurityDiagnosticSnapshot>,
    ) -> Result<Self, DiagnosticsError> {
        if security.len() > crate::authority_peer_hub::MAX_AUTHORITY_PEERS {
            return Err(DiagnosticsError::Capacity("security snapshots"));
        }
        Ok(Self {
            schema_version: OPERATIONAL_SCHEMA_VERSION,
            terminal,
            match_id: *manifest.match_id.as_bytes(),
            compatibility: manifest.compatibility.into(),
            manifest_hash: manifest.manifest_hash.0,
            network_tick: network_tick.get(),
            authority_tick: authority_tick.get(),
            result_id: result.map(|value| value.0),
            final_state_hash: result.map(|value| value.1),
            worker,
            hub: hub.into(),
            observability: observability.into(),
            server_ticks: server_ticks.into(),
            audit: audit.into(),
            diagnostics,
            security,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>, DiagnosticsError> {
        self.validate()?;
        let encoded = ron::ser::to_string(self)
            .map_err(|_| DiagnosticsError::Codec("encode operational snapshot"))?
            .into_bytes();
        enforce_size(encoded.len(), MAX_OPERATIONAL_BYTES, "operational snapshot")?;
        Ok(encoded)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DiagnosticsError> {
        enforce_size(bytes.len(), MAX_OPERATIONAL_BYTES, "operational snapshot")?;
        let text = std::str::from_utf8(bytes)
            .map_err(|_| DiagnosticsError::Codec("decode operational UTF-8"))?;
        let value: Self = ron::from_str(text)
            .map_err(|_| DiagnosticsError::Codec("decode operational snapshot"))?;
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), DiagnosticsError> {
        if self.schema_version != OPERATIONAL_SCHEMA_VERSION
            || self.match_id.iter().all(|byte| *byte == 0)
            || self.compatibility.protocol == 0
            || self.compatibility.simulation == 0
            || self.compatibility.replay == 0
            || self.manifest_hash == 0
            || self.security.len() > crate::authority_peer_hub::MAX_AUTHORITY_PEERS
        {
            return Err(DiagnosticsError::InvalidValue("operational snapshot"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SanitizedFailureDiagnostic {
    pub code: u16,
    pub severity: u8,
    pub recovery: u8,
    pub detail_code: u16,
}

impl From<OnlineFailure> for SanitizedFailureDiagnostic {
    fn from(value: OnlineFailure) -> Self {
        Self {
            code: online_failure_code(value.code),
            severity: online_failure_severity(value.severity),
            recovery: online_recovery_action(value.recovery),
            detail_code: value.detail_code,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedInputDiagnostic {
    pub fighter: u8,
    pub source: u8,
    pub seat: u8,
    pub movement_x: i8,
    pub movement_y: i8,
    pub held_buttons: u16,
    pub pressed_buttons: u16,
    pub released_buttons: u16,
    pub sequence: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedTickDiagnostic {
    pub tick: u64,
    pub input_count: u8,
    pub inputs: [AcceptedInputDiagnostic; crate::network_protocol::MAX_SEATS],
}

impl AcceptedTickDiagnostic {
    pub fn from_report(report: &AuthorityTickReport) -> Result<Self, DiagnosticsError> {
        let mut inputs = [AcceptedInputDiagnostic::default(); crate::network_protocol::MAX_SEATS];
        let mut input_count = 0_usize;
        for record in report.committed_inputs.iter() {
            if record.status != AuthorityInputStatus::Committed || record.frame.tick != report.tick
            {
                return Err(DiagnosticsError::InvalidValue("authority input report"));
            }
            let source = match record.origin {
                AuthorityInputOrigin::Peer(_) => ReplayInputSource::Peer as u8,
                AuthorityInputOrigin::AuthorityBot | AuthorityInputOrigin::DisconnectedBot(_) => {
                    ReplayInputSource::AuthorityBot as u8
                }
                AuthorityInputOrigin::MissingSubstitute => {
                    ReplayInputSource::AuthoritySubstitution as u8
                }
            };
            if input_count == inputs.len() {
                return Err(DiagnosticsError::Capacity("accepted inputs"));
            }
            inputs[input_count] = AcceptedInputDiagnostic {
                fighter: record.fighter.get(),
                source,
                seat: record.frame.seat.get(),
                movement_x: record.frame.movement_x.get(),
                movement_y: record.frame.movement_y.get(),
                held_buttons: record.frame.held_buttons.bits(),
                pressed_buttons: record.frame.pressed_buttons.bits(),
                released_buttons: record.frame.released_buttons.bits(),
                sequence: record.frame.sequence.0,
            };
            input_count += 1;
        }
        inputs[..input_count].sort_by_key(|input| (input.fighter, input.seat));
        Ok(Self {
            tick: report.tick.get(),
            input_count: input_count as u8,
            inputs,
        })
    }

    pub fn inputs(&self) -> &[AcceptedInputDiagnostic] {
        &self.inputs[..usize::from(self.input_count).min(self.inputs.len())]
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecordDiagnostic {
    pub sequence: u64,
    pub monotonic_ms: u64,
    pub match_id: Option<[u8; 16]>,
    pub peer_id: Option<u64>,
    pub seat_id: Option<u8>,
    pub fighter_id: Option<u8>,
    pub tick: Option<u64>,
    pub code: u16,
    pub value_a: u64,
    pub value_b: u64,
}

impl From<OnlineAuditRecord> for AuditRecordDiagnostic {
    fn from(value: OnlineAuditRecord) -> Self {
        Self {
            sequence: value.sequence,
            monotonic_ms: value.monotonic_ms,
            match_id: value.scope.match_id.map(|id| *id.as_bytes()),
            peer_id: value.scope.peer_id.map(|id| id.get()),
            seat_id: value.scope.seat_id.map(|id| id.get()),
            fighter_id: value.scope.fighter_id.map(|id| id.get()),
            tick: value.scope.tick.map(SimTick::get),
            code: value.code as u16,
            value_a: value.value_a,
            value_b: value.value_b,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorityIncidentBundle {
    pub operational: AuthorityOperationalSnapshot,
    pub failure: SanitizedFailureDiagnostic,
    pub latest_snapshot: CanonicalSnapshot,
    pub accepted_input_tail: Vec<AcceptedTickDiagnostic>,
    pub audit_tail: Vec<AuditRecordDiagnostic>,
}

#[derive(Serialize, Deserialize)]
struct IncidentWire {
    schema_version: u16,
    operational: AuthorityOperationalSnapshot,
    failure: SanitizedFailureDiagnostic,
    latest_snapshot: Vec<u8>,
    accepted_input_tail: Vec<AcceptedTickDiagnostic>,
    audit_tail: Vec<AuditRecordDiagnostic>,
}

impl AuthorityIncidentBundle {
    pub fn new(
        operational: AuthorityOperationalSnapshot,
        failure: OnlineFailure,
        latest_snapshot: CanonicalSnapshot,
        accepted_input_tail: Vec<AcceptedTickDiagnostic>,
        audit: &OnlineAuditLog,
    ) -> Result<Self, DiagnosticsError> {
        let bundle = Self {
            operational,
            failure: failure.into(),
            latest_snapshot,
            accepted_input_tail,
            audit_tail: audit.iter().map(Into::into).collect(),
        };
        bundle.validate()?;
        Ok(bundle)
    }

    pub fn encode(&self) -> Result<Vec<u8>, DiagnosticsError> {
        self.validate()?;
        let wire = IncidentWire {
            schema_version: INCIDENT_SCHEMA_VERSION,
            operational: self.operational.clone(),
            failure: self.failure,
            latest_snapshot: self.latest_snapshot.encode()?,
            accepted_input_tail: self.accepted_input_tail.clone(),
            audit_tail: self.audit_tail.clone(),
        };
        let encoded = ron::ser::to_string(&wire)
            .map_err(|_| DiagnosticsError::Codec("encode incident"))?
            .into_bytes();
        enforce_size(encoded.len(), MAX_INCIDENT_BYTES, "incident")?;
        Ok(encoded)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DiagnosticsError> {
        enforce_size(bytes.len(), MAX_INCIDENT_BYTES, "incident")?;
        let text = std::str::from_utf8(bytes)
            .map_err(|_| DiagnosticsError::Codec("decode incident UTF-8"))?;
        let wire: IncidentWire =
            ron::from_str(text).map_err(|_| DiagnosticsError::Codec("decode incident"))?;
        if wire.schema_version != INCIDENT_SCHEMA_VERSION {
            return Err(DiagnosticsError::InvalidValue("incident schema"));
        }
        enforce_size(
            wire.latest_snapshot.len(),
            MAX_SNAPSHOT_BYTES,
            "incident snapshot",
        )?;
        let bundle = Self {
            operational: wire.operational,
            failure: wire.failure,
            latest_snapshot: CanonicalSnapshot::decode(&wire.latest_snapshot)?,
            accepted_input_tail: wire.accepted_input_tail,
            audit_tail: wire.audit_tail,
        };
        bundle.validate()?;
        Ok(bundle)
    }

    fn validate(&self) -> Result<(), DiagnosticsError> {
        self.operational.validate()?;
        if self.failure.code == 0
            || self.accepted_input_tail.len() > INCIDENT_INPUT_TAIL_TICKS
            || self.audit_tail.len() > crate::multiplayer_observability::ONLINE_AUDIT_CAPACITY
            || self.latest_snapshot.header.match_id != self.operational.match_id
            || self.latest_snapshot.header.tick.get() != self.operational.authority_tick
        {
            return Err(DiagnosticsError::InvalidValue("incident bundle"));
        }
        let mut previous_tick = None;
        for tick in &self.accepted_input_tail {
            if usize::from(tick.input_count) > crate::network_protocol::MAX_SEATS
                || tick.inputs[usize::from(tick.input_count).min(tick.inputs.len())..]
                    .iter()
                    .any(|input| *input != AcceptedInputDiagnostic::default())
                || previous_tick.is_some_and(|previous| previous >= tick.tick)
                || tick.tick > self.operational.authority_tick
            {
                return Err(DiagnosticsError::InvalidValue("incident input tail"));
            }
            previous_tick = Some(tick.tick);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum DiagnosticsError {
    Io {
        operation: &'static str,
        source: io::Error,
    },
    Replay(ReplayArchiveError),
    Snapshot(SnapshotError),
    Codec(&'static str),
    InvalidValue(&'static str),
    Capacity(&'static str),
    FileTooLarge {
        kind: &'static str,
        bytes: usize,
        maximum: usize,
    },
    ConflictingExisting(PathBuf),
    DirectoryEntryLimit,
    TemporaryNameExhausted,
}

impl fmt::Display for DiagnosticsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { operation, source } => {
                write!(formatter, "diagnostics {operation} failed: {source}")
            }
            Self::Replay(error) => write!(formatter, "diagnostics replay failed: {error}"),
            Self::Snapshot(error) => write!(formatter, "diagnostics snapshot failed: {error}"),
            Self::Codec(operation) => write!(formatter, "diagnostics codec failed to {operation}"),
            Self::InvalidValue(field) => write!(formatter, "invalid diagnostics {field}"),
            Self::Capacity(field) => write!(formatter, "diagnostics {field} exceeded capacity"),
            Self::FileTooLarge {
                kind,
                bytes,
                maximum,
            } => write!(formatter, "{kind} has {bytes} bytes; maximum is {maximum}"),
            Self::ConflictingExisting(path) => write!(
                formatter,
                "diagnostics identity already exists with different bytes at {}",
                path.display()
            ),
            Self::DirectoryEntryLimit => write!(
                formatter,
                "diagnostics directory exceeded {MAX_DIAGNOSTIC_DIRECTORY_ENTRIES} managed files"
            ),
            Self::TemporaryNameExhausted => {
                formatter.write_str("diagnostics temporary filename space exhausted")
            }
        }
    }
}

impl Error for DiagnosticsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Replay(source) => Some(source),
            Self::Snapshot(source) => Some(source),
            _ => None,
        }
    }
}

impl From<ReplayArchiveError> for DiagnosticsError {
    fn from(value: ReplayArchiveError) -> Self {
        Self::Replay(value)
    }
}

impl From<SnapshotError> for DiagnosticsError {
    fn from(value: SnapshotError) -> Self {
        Self::Snapshot(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticSaveDisposition {
    Created,
    AlreadyPresent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredDiagnostic {
    pub path: PathBuf,
    pub encoded_bytes: usize,
    pub disposition: DiagnosticSaveDisposition,
    pub pruned_files: usize,
}

#[derive(Clone, Debug)]
pub struct AuthorityDiagnosticsArchive {
    root: PathBuf,
}

impl AuthorityDiagnosticsArchive {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn replay_archive(&self) -> ReplayArchive {
        ReplayArchive::new(self.root.join("replays"))
    }

    fn prepare_root(&self) -> Result<(), DiagnosticsError> {
        create_private_directory(&self.root)
    }

    pub fn save_replay(&self, replay: &Replay) -> Result<StoredReplay, DiagnosticsError> {
        self.prepare_root()?;
        let stored = self.replay_archive().save(replay)?;
        sync_directory(&self.root)?;
        Ok(stored)
    }

    pub fn save_incident(
        &self,
        incident: &AuthorityIncidentBundle,
    ) -> Result<StoredDiagnostic, DiagnosticsError> {
        self.prepare_root()?;
        let bytes = incident.encode()?;
        let name = format!(
            "match-{}-tick-{:020}-failure-{:04x}-{:04x}.{INCIDENT_EXTENSION}",
            hex(&incident.operational.match_id),
            incident.operational.authority_tick,
            incident.failure.code,
            incident.failure.detail_code,
        );
        let stored = atomic_save(
            &self.root.join("incidents"),
            &name,
            INCIDENT_EXTENSION,
            &bytes,
            MAX_INCIDENT_FILES,
            MAX_INCIDENT_TOTAL_BYTES,
        )?;
        sync_directory(&self.root)?;
        Ok(stored)
    }

    pub fn load_incident(&self, path: &Path) -> Result<AuthorityIncidentBundle, DiagnosticsError> {
        AuthorityIncidentBundle::decode(&read_bounded(path, MAX_INCIDENT_BYTES, "incident")?)
    }

    pub fn save_operational(
        &self,
        snapshot: &AuthorityOperationalSnapshot,
    ) -> Result<StoredDiagnostic, DiagnosticsError> {
        self.prepare_root()?;
        let bytes = snapshot.encode()?;
        let kind = if snapshot.terminal {
            "terminal"
        } else {
            "periodic"
        };
        let name = format!(
            "match-{}-network-{:020}-{kind}.{OPERATIONAL_EXTENSION}",
            hex(&snapshot.match_id),
            snapshot.network_tick,
        );
        let stored = atomic_save(
            &self.root.join("operational"),
            &name,
            OPERATIONAL_EXTENSION,
            &bytes,
            MAX_OPERATIONAL_FILES,
            MAX_OPERATIONAL_TOTAL_BYTES,
        )?;
        sync_directory(&self.root)?;
        Ok(stored)
    }

    pub fn load_operational(
        &self,
        path: &Path,
    ) -> Result<AuthorityOperationalSnapshot, DiagnosticsError> {
        AuthorityOperationalSnapshot::decode(&read_bounded(
            path,
            MAX_OPERATIONAL_BYTES,
            "operational snapshot",
        )?)
    }
}

enum DiagnosticsJob {
    Replay(Replay),
    Incident(AuthorityIncidentBundle),
    Operational(AuthorityOperationalSnapshot),
}

pub struct AuthorityDiagnosticsWriter {
    critical: Option<SyncSender<DiagnosticsJob>>,
    periodic: Option<SyncSender<DiagnosticsJob>>,
    join: Option<JoinHandle<()>>,
    counters: Arc<DiagnosticsCounters>,
}

impl AuthorityDiagnosticsWriter {
    pub fn start(root: PathBuf, counters: Arc<DiagnosticsCounters>) -> Result<Self, io::Error> {
        let (critical_tx, critical_rx) = mpsc::sync_channel(DIAGNOSTIC_CRITICAL_QUEUE_CAPACITY);
        let (periodic_tx, periodic_rx) = mpsc::sync_channel(DIAGNOSTIC_PERIODIC_QUEUE_CAPACITY);
        let worker_counters = Arc::clone(&counters);
        let join = thread::Builder::new()
            .name("afc-authority-diagnostics".to_owned())
            .spawn(move || {
                run_diagnostics_writer(root, critical_rx, periodic_rx, worker_counters)
            })?;
        Ok(Self {
            critical: Some(critical_tx),
            periodic: Some(periodic_tx),
            join: Some(join),
            counters,
        })
    }

    /// Never blocks the authority loop. A returned replay must be retried after
    /// canonical stepping has stopped.
    pub fn try_queue_replay(&self, replay: Replay) -> Result<(), Replay> {
        let Some(sender) = self.critical.as_ref() else {
            return Err(replay);
        };
        match sender.try_send(DiagnosticsJob::Replay(replay)) {
            Ok(()) => {
                self.counters.jobs_queued.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(
                TrySendError::Full(DiagnosticsJob::Replay(replay))
                | TrySendError::Disconnected(DiagnosticsJob::Replay(replay)),
            ) => Err(replay),
            Err(_) => unreachable!("queued replay preserves its job variant"),
        }
    }

    pub fn try_queue_periodic(&self, snapshot: AuthorityOperationalSnapshot) {
        let Some(sender) = self.periodic.as_ref() else {
            self.counters
                .periodic_jobs_dropped
                .fetch_add(1, Ordering::Relaxed);
            return;
        };
        match sender.try_send(DiagnosticsJob::Operational(snapshot)) {
            Ok(()) => {
                self.counters.jobs_queued.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.counters
                    .periodic_jobs_dropped
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Called only after the 60 Hz loop has ended. Terminal jobs are submitted
    /// without blocking and the writer is joined only within `deadline`.
    ///
    /// A filesystem call already executing on the writer thread cannot be
    /// cancelled safely. If it exceeds the deadline, dropping the join handle
    /// detaches that diagnostics-only thread so gameplay/AppExit cannot block.
    pub fn finish_and_join_bounded(
        mut self,
        deferred_replay: Option<Replay>,
        incident: Option<AuthorityIncidentBundle>,
        terminal: Option<AuthorityOperationalSnapshot>,
        timeout: Duration,
    ) -> bool {
        self.periodic.take();
        if let Some(sender) = self.critical.take() {
            if let Some(replay) = deferred_replay {
                self.try_send_terminal(&sender, DiagnosticsJob::Replay(replay));
            }
            if let Some(incident) = incident {
                self.try_send_terminal(&sender, DiagnosticsJob::Incident(incident));
            }
            if let Some(terminal) = terminal {
                self.try_send_terminal(&sender, DiagnosticsJob::Operational(terminal));
            }
            drop(sender);
        } else {
            self.counters
                .persistence_failures
                .fetch_add(1, Ordering::Relaxed);
        }
        let Some(join) = self.join.take() else {
            return true;
        };
        let deadline = Instant::now().checked_add(timeout);
        while !join.is_finished() && deadline.is_some_and(|deadline| Instant::now() < deadline) {
            thread::yield_now();
        }
        if !join.is_finished() {
            self.counters
                .writer_join_failures
                .fetch_add(1, Ordering::Relaxed);
            drop(join);
            return false;
        }
        if join.join().is_err() {
            self.counters
                .writer_join_failures
                .fetch_add(1, Ordering::Relaxed);
            return false;
        }
        true
    }

    fn try_send_terminal(&self, sender: &SyncSender<DiagnosticsJob>, job: DiagnosticsJob) {
        match sender.try_send(job) {
            Ok(()) => {
                self.counters.jobs_queued.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.counters
                    .persistence_failures
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

fn run_diagnostics_writer(
    root: PathBuf,
    critical: Receiver<DiagnosticsJob>,
    periodic: Receiver<DiagnosticsJob>,
    counters: Arc<DiagnosticsCounters>,
) {
    let archive = AuthorityDiagnosticsArchive::new(root);
    let mut critical_open = true;
    let mut periodic_open = true;
    while critical_open || periodic_open {
        let mut handled = false;
        if critical_open {
            match critical.recv_timeout(Duration::from_millis(25)) {
                Ok(job) => {
                    persist_job(&archive, job, &counters);
                    handled = true;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => critical_open = false,
            }
        }
        if handled || !periodic_open {
            continue;
        }
        match periodic.try_recv() {
            Ok(job) => persist_job(&archive, job, &counters),
            Err(TryRecvError::Empty) => {
                if !critical_open {
                    thread::sleep(Duration::from_millis(1));
                }
            }
            Err(TryRecvError::Disconnected) => periodic_open = false,
        }
    }
}

fn persist_job(
    archive: &AuthorityDiagnosticsArchive,
    job: DiagnosticsJob,
    counters: &DiagnosticsCounters,
) {
    let result = match job {
        DiagnosticsJob::Replay(replay) => archive
            .save_replay(&replay)
            .map(|_| (0, &counters.replays_persisted)),
        DiagnosticsJob::Incident(incident) => archive
            .save_incident(&incident)
            .map(|stored| (stored.pruned_files, &counters.incidents_persisted)),
        DiagnosticsJob::Operational(snapshot) => {
            archive.save_operational(&snapshot).map(|stored| {
                (
                    stored.pruned_files,
                    &counters.operational_snapshots_persisted,
                )
            })
        }
    };
    match result {
        Ok((pruned, completed)) => {
            completed.fetch_add(1, Ordering::Relaxed);
            counters
                .files_pruned
                .fetch_add(pruned as u64, Ordering::Relaxed);
        }
        Err(_) => {
            counters
                .persistence_failures
                .fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Last-resort terminal persistence used only if the bounded writer thread could
/// not be created. It must be called after canonical stepping has stopped.
pub fn persist_terminal_synchronously(
    root: PathBuf,
    counters: &DiagnosticsCounters,
    replay: Option<Replay>,
    incident: Option<AuthorityIncidentBundle>,
    terminal: Option<AuthorityOperationalSnapshot>,
) {
    let archive = AuthorityDiagnosticsArchive::new(root);
    if let Some(replay) = replay {
        persist_job(&archive, DiagnosticsJob::Replay(replay), counters);
    }
    if let Some(incident) = incident {
        persist_job(&archive, DiagnosticsJob::Incident(incident), counters);
    }
    if let Some(terminal) = terminal {
        persist_job(&archive, DiagnosticsJob::Operational(terminal), counters);
    }
}

fn atomic_save(
    directory: &Path,
    filename: &str,
    extension: &str,
    bytes: &[u8],
    maximum_files: usize,
    maximum_total_bytes: u64,
) -> Result<StoredDiagnostic, DiagnosticsError> {
    create_private_directory(directory)?;
    let final_path = directory.join(filename);
    let (temporary_path, mut temporary) = reserve_temporary(directory, filename)?;
    let mut cleanup = TemporaryCleanup::new(temporary_path.clone());
    temporary
        .write_all(bytes)
        .map_err(|source| io_error("write temporary file", source))?;
    temporary
        .sync_all()
        .map_err(|source| io_error("sync temporary file", source))?;
    drop(temporary);
    let disposition = match fs::hard_link(&temporary_path, &final_path) {
        Ok(()) => DiagnosticSaveDisposition::Created,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let retained = read_bounded(&final_path, bytes.len(), "existing diagnostic")?;
            if retained != bytes {
                return Err(DiagnosticsError::ConflictingExisting(final_path));
            }
            DiagnosticSaveDisposition::AlreadyPresent
        }
        Err(source) => return Err(io_error("publish temporary file", source)),
    };
    fs::remove_file(&temporary_path).map_err(|source| io_error("remove temporary file", source))?;
    cleanup.disarm();
    restrict_private_file(&final_path)?;
    File::open(&final_path)
        .and_then(|file| file.sync_all())
        .map_err(|source| io_error("sync published file", source))?;
    sync_directory(directory)?;
    let pruned_files = enforce_retention(
        directory,
        extension,
        &final_path,
        maximum_files,
        maximum_total_bytes,
    )?;
    Ok(StoredDiagnostic {
        path: final_path,
        encoded_bytes: bytes.len(),
        disposition,
        pruned_files,
    })
}

fn enforce_retention(
    directory: &Path,
    extension: &str,
    retained_path: &Path,
    maximum_files: usize,
    maximum_total_bytes: u64,
) -> Result<usize, DiagnosticsError> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory).map_err(|source| io_error("read directory", source))? {
        let entry = entry.map_err(|source| io_error("read directory entry", source))?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some(extension) {
            continue;
        }
        if entries.len() == MAX_DIAGNOSTIC_DIRECTORY_ENTRIES {
            return Err(DiagnosticsError::DirectoryEntryLimit);
        }
        let metadata = entry
            .metadata()
            .map_err(|source| io_error("read file metadata", source))?;
        if metadata.is_file() {
            entries.push((path, metadata.len(), metadata.modified().ok()));
        }
    }
    entries.sort_by(|left, right| {
        let left_retained = left.0 == retained_path;
        let right_retained = right.0 == retained_path;
        left_retained.cmp(&right_retained).then_with(|| {
            left.2
                .cmp(&right.2)
                .then_with(|| left.0.file_name().cmp(&right.0.file_name()))
        })
    });
    let mut total_bytes = entries
        .iter()
        .fold(0_u64, |sum, entry| sum.saturating_add(entry.1));
    let mut file_count = entries.len();
    let mut pruned = 0;
    for (path, bytes, _) in entries {
        if file_count <= maximum_files && total_bytes <= maximum_total_bytes {
            break;
        }
        if path == retained_path {
            continue;
        }
        fs::remove_file(path).map_err(|source| io_error("prune file", source))?;
        file_count = file_count.saturating_sub(1);
        total_bytes = total_bytes.saturating_sub(bytes);
        pruned += 1;
    }
    if file_count > maximum_files || total_bytes > maximum_total_bytes {
        return Err(DiagnosticsError::Capacity("retention"));
    }
    sync_directory(directory)?;
    Ok(pruned)
}

fn reserve_temporary(directory: &Path, stem: &str) -> Result<(PathBuf, File), DiagnosticsError> {
    for _ in 0..TEMP_CREATE_ATTEMPTS {
        let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(".{stem}.{}.{}.tmp", std::process::id(), sequence));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(io_error("create temporary file", source)),
        }
    }
    Err(DiagnosticsError::TemporaryNameExhausted)
}

fn create_private_directory(path: &Path) -> Result<(), DiagnosticsError> {
    fs::create_dir_all(path).map_err(|source| io_error("create directory", source))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|source| io_error("restrict directory permissions", source))?;
    }
    Ok(())
}

fn restrict_private_file(path: &Path) -> Result<(), DiagnosticsError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|source| io_error("restrict file permissions", source))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn read_bounded(
    path: &Path,
    maximum: usize,
    kind: &'static str,
) -> Result<Vec<u8>, DiagnosticsError> {
    let file = File::open(path).map_err(|source| io_error("open file", source))?;
    let bytes = file
        .metadata()
        .map_err(|source| io_error("read metadata", source))?
        .len();
    if bytes > maximum as u64 {
        return Err(DiagnosticsError::FileTooLarge {
            kind,
            bytes: bytes.min(usize::MAX as u64) as usize,
            maximum,
        });
    }
    let mut output = Vec::with_capacity(bytes as usize);
    file.take(maximum as u64 + 1)
        .read_to_end(&mut output)
        .map_err(|source| io_error("read file", source))?;
    enforce_size(output.len(), maximum, kind)?;
    Ok(output)
}

fn enforce_size(bytes: usize, maximum: usize, kind: &'static str) -> Result<(), DiagnosticsError> {
    if bytes > maximum {
        Err(DiagnosticsError::FileTooLarge {
            kind,
            bytes,
            maximum,
        })
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), DiagnosticsError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error("sync directory", source))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), DiagnosticsError> {
    Ok(())
}

fn io_error(operation: &'static str, source: io::Error) -> DiagnosticsError {
    DiagnosticsError::Io { operation, source }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[usize::from(byte >> 4)] as char);
        output.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    output
}

struct TemporaryCleanup {
    path: Option<PathBuf>,
}

impl TemporaryCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for TemporaryCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

/// Fixed-capacity accepted-input tail retained independently of replay success.
pub struct AcceptedInputTail {
    ticks: VecDeque<AcceptedTickDiagnostic>,
}

impl Default for AcceptedInputTail {
    fn default() -> Self {
        Self {
            ticks: VecDeque::with_capacity(INCIDENT_INPUT_TAIL_TICKS),
        }
    }
}

impl AcceptedInputTail {
    pub fn push_report(&mut self, report: &AuthorityTickReport) -> Result<(), DiagnosticsError> {
        let tick = AcceptedTickDiagnostic::from_report(report)?;
        if self.ticks.len() == INCIDENT_INPUT_TAIL_TICKS {
            self.ticks.pop_front();
        }
        self.ticks.push_back(tick);
        Ok(())
    }

    pub fn to_vec(&self) -> Vec<AcceptedTickDiagnostic> {
        self.ticks.iter().cloned().collect()
    }
}

fn online_failure_code(code: OnlineFailureCode) -> u16 {
    match code {
        OnlineFailureCode::SteamUnavailable => 1,
        OnlineFailureCode::SteamDisconnected => 2,
        OnlineFailureCode::InvalidReleaseConfiguration => 3,
        OnlineFailureCode::LobbyUnavailable => 4,
        OnlineFailureCode::LobbyFull => 5,
        OnlineFailureCode::LobbyClosed => 6,
        OnlineFailureCode::InviteRequired => 7,
        OnlineFailureCode::FriendRequired => 8,
        OnlineFailureCode::NotLobbyOwner => 9,
        OnlineFailureCode::PublicPlayDisabled => 10,
        OnlineFailureCode::InvalidSeatCount => 11,
        OnlineFailureCode::IncompatibleVersion => 12,
        OnlineFailureCode::AuthenticationFailed => 13,
        OnlineFailureCode::OwnershipFailed => 14,
        OnlineFailureCode::PlatformBanned => 15,
        OnlineFailureCode::AuthenticationTimedOut => 16,
        OnlineFailureCode::ConnectionTimedOut => 17,
        OnlineFailureCode::LoadingTimedOut => 18,
        OnlineFailureCode::SynchronizationFailed => 19,
        OnlineFailureCode::ClockSynchronizationFailed => 20,
        OnlineFailureCode::InvalidInput => 21,
        OnlineFailureCode::MalformedTraffic => 22,
        OnlineFailureCode::RateLimited => 23,
        OnlineFailureCode::Kicked => 24,
        OnlineFailureCode::AuthorityLost => 25,
        OnlineFailureCode::ServerShutdown => 26,
        OnlineFailureCode::DedicatedUnavailable => 27,
        OnlineFailureCode::InternalCapacity => 28,
        OnlineFailureCode::InternalFailure => 29,
        OnlineFailureCode::NetworkQualityRejected => 30,
        OnlineFailureCode::OverlayUnavailable => 31,
    }
}

fn online_failure_severity(severity: OnlineFailureSeverity) -> u8 {
    match severity {
        OnlineFailureSeverity::Notice => 1,
        OnlineFailureSeverity::Recoverable => 2,
        OnlineFailureSeverity::MatchEnded => 3,
        OnlineFailureSeverity::Fatal => 4,
    }
}

fn online_recovery_action(action: OnlineRecoveryAction) -> u8 {
    match action {
        OnlineRecoveryAction::Dismiss => 1,
        OnlineRecoveryAction::Retry => 2,
        OnlineRecoveryAction::ReturnToLobby => 3,
        OnlineRecoveryAction::Reconnect => 4,
        OnlineRecoveryAction::ReturnToMenu => 5,
        OnlineRecoveryAction::MatchEndedNoContest => 6,
        OnlineRecoveryAction::DisableOnline => 7,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::authority::AuthoritySimulation;
    use crate::game_state::LocalSetup;
    use crate::headless::build_headless_simulation;
    use crate::match_config::{MatchBuildOptions, build_headless_match_config};
    use crate::network_protocol::{AuthorityKind, MatchId, PeerId};

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "afc-diagnostics-{label}-{}-{}",
            std::process::id(),
            NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn fixture_operational(
        terminal: bool,
        tick: u64,
    ) -> (AuthorityOperationalSnapshot, CanonicalSnapshot) {
        let setup = LocalSetup::default();
        let match_id = MatchId::new(*b"diag-fixture-001").unwrap();
        let options = MatchBuildOptions::single_peer(
            match_id,
            AuthorityKind::Listen,
            false,
            PeerId::new(7).unwrap(),
            &setup,
            SimTick(120),
        );
        let config = build_headless_match_config(&setup, options).unwrap();
        let snapshot = build_headless_simulation(config.clone())
            .unwrap()
            .capture_snapshot()
            .unwrap();
        let operational = AuthorityOperationalSnapshot::new(
            &config.manifest,
            terminal,
            SimTick(tick),
            snapshot.header.tick,
            None,
            AuthorityWorkerDiagnosticSnapshot::default(),
            AuthorityPeerHubMetrics::default(),
            MultiplayerCounterSnapshot::default(),
            ServerTickDistribution::default(),
            OnlineAuditMetrics::default(),
            DiagnosticsCounterSnapshot::default(),
            Vec::new(),
        )
        .unwrap();
        (operational, snapshot)
    }

    #[test]
    fn incident_round_trip_is_bounded_idempotent_and_privacy_typed() {
        let root = temp_root("incident");
        let archive = AuthorityDiagnosticsArchive::new(&root);
        let (operational, snapshot) = fixture_operational(true, 3);
        let incident = AuthorityIncidentBundle::new(
            operational,
            OnlineFailure {
                code: OnlineFailureCode::InternalFailure,
                severity: OnlineFailureSeverity::Fatal,
                recovery: OnlineRecoveryAction::ReturnToLobby,
                detail_code: 77,
            },
            snapshot,
            Vec::new(),
            &OnlineAuditLog::default(),
        )
        .unwrap();
        let first = archive.save_incident(&incident).unwrap();
        assert_eq!(first.disposition, DiagnosticSaveDisposition::Created);
        let second = archive.save_incident(&incident).unwrap();
        assert_eq!(
            second.disposition,
            DiagnosticSaveDisposition::AlreadyPresent
        );
        assert_eq!(archive.load_incident(&first.path).unwrap(), incident);

        let mut conflicting = incident.clone();
        conflicting.operational.worker.worker_iterations = 1;
        assert!(matches!(
            archive.save_incident(&conflicting),
            Err(DiagnosticsError::ConflictingExisting(_))
        ));

        let text = String::from_utf8(fs::read(&first.path).unwrap()).unwrap();
        for forbidden in ["steam", "ticket", "address", "backend", "persona"] {
            assert!(!text.to_ascii_lowercase().contains(forbidden));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&first.path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(first.path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&root).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn operational_retention_and_oversize_are_exactly_bounded() {
        let root = temp_root("retention");
        let archive = AuthorityDiagnosticsArchive::new(&root);
        for tick in 0..MAX_OPERATIONAL_FILES + 3 {
            let (snapshot, _) = fixture_operational(false, tick as u64);
            archive.save_operational(&snapshot).unwrap();
        }
        let retained = fs::read_dir(root.join("operational"))
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry.path().extension().and_then(|value| value.to_str())
                    == Some(OPERATIONAL_EXTENSION)
            })
            .count();
        assert_eq!(retained, MAX_OPERATIONAL_FILES);
        assert!(matches!(
            AuthorityOperationalSnapshot::decode(&vec![0; MAX_OPERATIONAL_BYTES + 1]),
            Err(DiagnosticsError::FileTooLarge { .. })
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolved_shipping_root_is_absolute() {
        assert!(resolve_diagnostics_root().path.is_absolute());
    }

    #[test]
    fn slow_diagnostics_sink_cannot_extend_terminal_join_past_deadline() {
        let counters = Arc::new(DiagnosticsCounters::default());
        let (critical, critical_rx) = mpsc::sync_channel(1);
        let (periodic, periodic_rx) = mpsc::sync_channel(1);
        let join = thread::spawn(move || {
            // Models an already-entered blocking filesystem call. The finite
            // sleep also guarantees the detached test worker does not leak.
            thread::sleep(Duration::from_millis(75));
            drop(critical_rx);
            drop(periodic_rx);
        });
        let writer = AuthorityDiagnosticsWriter {
            critical: Some(critical),
            periodic: Some(periodic),
            join: Some(join),
            counters: Arc::clone(&counters),
        };

        let started = Instant::now();
        assert!(!writer.finish_and_join_bounded(None, None, None, Duration::from_millis(5),));
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "diagnostics deadline was not honored"
        );
        assert_eq!(counters.snapshot().writer_join_failures, 1);
        thread::sleep(Duration::from_millis(80));
    }
}
