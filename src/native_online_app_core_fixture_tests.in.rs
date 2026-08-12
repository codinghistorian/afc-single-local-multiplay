use crate::native_online_app::{
    NativeOnlineApplication, NativeOnlineRuntimePort, NativeOnlineSessionKind, NativeOnlineUiAction,
};

impl NativeOnlineRuntimePort for FakeNativeOnlineCore {
    fn view_model(&self) -> NativeOnlineViewModel {
        NativeOnlineCore::view_model(self)
    }

    fn execute_port(
        &mut self,
        command: NativeOnlineCommand,
        now_ms: u64,
    ) -> Result<(), NativeOnlineRuntimeError> {
        self.execute(command, now_ms)
    }

    fn open_invite_overlay_port(
        &mut self,
    ) -> Result<SteamOverlayRequestStatus, NativeOnlineRuntimeError> {
        self.open_invite_overlay().map_err(Into::into)
    }

    fn poll_event_port(&mut self) -> Option<OnlineLobbyEvent> {
        self.events.pop_front()
    }

    fn take_endpoint_port(&mut self) -> Option<NativeOnlineEndpoint> {
        self.endpoints.pop_front()
    }

    fn match_config_port(&self) -> Option<HeadlessMatchConfig> {
        self.coordinator.match_config().cloned()
    }

    fn committed_roster_port(&self) -> Option<CommittedAuthenticatedRoster> {
        self.committed_roster
    }

    fn local_authenticated_user_port(&self) -> Option<AuthenticatedUserId> {
        Some(self.local_authenticated_user())
    }

    fn transport_retirement_pending_port(&self) -> bool {
        self.coordinator.retiring_transport_count() != 0
    }

    fn make_local_declaration_port(
        &self,
        peer_id: PeerId,
        revision: u16,
        ready: bool,
        seats: &[OnlineSeatSelection],
    ) -> Result<OnlineRosterMember, NativeOnlineRuntimeError> {
        OnlineRosterMember::new(
            peer_id,
            self.local_authenticated_user(),
            revision,
            ready,
            seats,
        )
        .map_err(|_| NativeOnlineRuntimeError::InvalidAuthenticatedRoster)
    }
}

/// Two independent application processes backed by the real native-online
/// core and the deterministic Steam socket/auth implementations. Applications
/// are declared before cores so their worker-owned endpoints are always
/// stopped before the Steam/platform owners are dropped.
struct ApplicationCorePair {
    host_application: NativeOnlineApplication,
    client_application: NativeOnlineApplication,
    host: FakeNativeOnlineCore,
    client: FakeNativeOnlineCore,
    network: FakeSteamTransportNetwork,
    host_control: FakeSteamControl,
    client_control: FakeSteamControl,
    host_user: SteamUserId,
    client_user: SteamUserId,
    lobby: Option<SteamLobbyId>,
    now_ms: u64,
}

impl ApplicationCorePair {
    fn new() -> Self {
        let app_id = SteamAppId::new(12_347).unwrap();
        let host_user = SteamUserId::new(78_001).unwrap();
        let client_user = SteamUserId::new(78_002).unwrap();
        let network = FakeSteamTransportNetwork::new(128).unwrap();
        let auth_authority = FakeSteamAuthAuthority::new();
        let (host_backend, host_control) =
            FakeSteamBackend::new_with_auth_authority(app_id, host_user, auth_authority.clone());
        let (client_backend, client_control) =
            FakeSteamBackend::new_with_auth_authority(app_id, client_user, auth_authority);
        let make_platform = |backend| {
            SteamPlatform::new(SteamClientConfig::production(app_id), backend, 0).unwrap()
        };
        let lobby_config = OnlineLobbyConfig {
            quality_sample_interval_ms: 1,
            ..OnlineLobbyConfig::default()
        };
        let host = NativeOnlineCore::from_parts(
            make_platform(host_backend),
            FakeNativeTransportFactory {
                network: network.clone(),
            },
            lobby_config,
            0,
        )
        .unwrap();
        let client = NativeOnlineCore::from_parts(
            make_platform(client_backend),
            FakeNativeTransportFactory {
                network: network.clone(),
            },
            lobby_config,
            0,
        )
        .unwrap();
        Self {
            host_application: NativeOnlineApplication::default(),
            client_application: NativeOnlineApplication::default(),
            host,
            client,
            network,
            host_control,
            client_control,
            host_user,
            client_user,
            lobby: None,
            now_ms: 0,
        }
    }

    fn install_lobby_shell(&mut self) {
        let lobby = self
            .host
            .coordinator
            .status()
            .lobby
            .expect("host create completed before the shell is mirrored");
        self.host_control
            .mirror_lobby_shell_to(&self.client_control, lobby)
            .unwrap();
        self.lobby = Some(lobby);
        self.mirror();
    }

    fn mirror(&self) {
        let Some(lobby) = self.lobby else {
            return;
        };
        self.host_control
            .mirror_lobby_owner_state_to(&self.client_control, lobby)
            .unwrap();
        self.host_control
            .mirror_lobby_member_to(&self.client_control, lobby, self.host_user)
            .unwrap();
        self.client_control
            .mirror_lobby_member_to(&self.host_control, lobby, self.client_user)
            .unwrap();
    }

    fn pump_once(&mut self) {
        self.now_ms = self.now_ms.saturating_add(1);
        self.mirror();
        self.host.pump(self.now_ms).unwrap_or_else(|error| {
            panic!(
                "host real-core pump {} failed: {error:?}; host={:?}; client={:?}",
                self.now_ms,
                self.host.coordinator.status(),
                self.client.coordinator.status(),
            )
        });
        self.host_application
            .pump(&mut self.host, self.now_ms)
            .unwrap_or_else(|error| {
                panic!(
                    "host application pump {} failed: {error:?}; host={:?}; client={:?}",
                    self.now_ms,
                    self.host.coordinator.status(),
                    self.client.coordinator.status(),
                )
            });
        self.mirror();
        self.client.pump(self.now_ms).unwrap_or_else(|error| {
            panic!(
                "client real-core pump {} failed: {error:?}; host={:?}; client={:?}",
                self.now_ms,
                self.host.coordinator.status(),
                self.client.coordinator.status(),
            )
        });
        self.client_application
            .pump(&mut self.client, self.now_ms)
            .unwrap_or_else(|error| {
                panic!(
                    "client application pump {} failed: {error:?}; host={:?}; client={:?}",
                    self.now_ms,
                    self.host.coordinator.status(),
                    self.client.coordinator.status(),
                )
            });
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    fn pump_until(&mut self, limit: usize, predicate: impl Fn(&Self) -> bool) {
        for _ in 0..limit {
            if predicate(self) {
                return;
            }
            self.pump_once();
        }
        assert!(
            predicate(self),
            "real core/application/worker fixture did not converge: host={:?}, client={:?}, host_session={:?}, client_session={:?}, resources={:?}",
            self.host.coordinator.status(),
            self.client.coordinator.status(),
            self.host_application.active_session_kind(),
            self.client_application.active_session_kind(),
            self.network.resource_counts(),
        );
    }
}

#[test]
fn applications_drive_real_cores_into_workers_and_return_without_injected_handoffs() {
    let mut pair = ApplicationCorePair::new();

    // The application authors both declarations. No test assigns readiness,
    // committed config, authenticated mappings, or gameplay endpoints.
    pair.host_application
        .dispatch(&mut pair.host, NativeOnlineUiAction::CreatePrivate, 0)
        .unwrap();
    pair.pump_until(32, |pair| {
        pair.host.view_model().screen == NativeOnlineScreen::Lobby
    });
    pair.install_lobby_shell();
    let lobby = pair.lobby.unwrap();

    pair.client_control
        .emit_join_request(lobby, Some(pair.host_user))
        .unwrap();
    pair.pump_until(32, |pair| {
        pair.client.view_model().screen == NativeOnlineScreen::JoinPrompt
    });
    pair.client_application
        .dispatch(
            &mut pair.client,
            NativeOnlineUiAction::ToggleTeam,
            pair.now_ms,
        )
        .unwrap();
    pair.client_application
        .dispatch(
            &mut pair.client,
            NativeOnlineUiAction::AcceptJoin,
            pair.now_ms,
        )
        .unwrap();
    pair.mirror();

    pair.pump_until(240, |pair| {
        let host = pair.host.coordinator.status();
        let client = pair.client.coordinator.status();
        host.phase == OnlineLobbyPhase::Lobby
            && client.phase == OnlineLobbyPhase::Lobby
            && host.secure_remote_peers == 1
            && client.secure_remote_peers == 1
            && host.verified_remote_accounts == 1
            && client.verified_remote_accounts == 1
            && pair.host.endpoints.is_empty()
            && pair.client.endpoints.is_empty()
    });

    // Exercise the opposite Ready order from the common owner-first path.
    pair.client_application
        .dispatch(
            &mut pair.client,
            NativeOnlineUiAction::ToggleReady,
            pair.now_ms,
        )
        .unwrap();
    pair.pump_once();
    assert!(!pair.host.coordinator.status().all_members_ready);
    pair.host_application
        .dispatch(
            &mut pair.host,
            NativeOnlineUiAction::ToggleReady,
            pair.now_ms,
        )
        .unwrap();
    pair.pump_until(240, |pair| {
        let host = pair.host.coordinator.status();
        let client = pair.client.coordinator.status();
        host.all_members_ready
            && client.all_members_ready
            && host.start_blocker.is_none()
            && host.input_delay_calibration.state
                == crate::network_quality::InputDelayCalibrationState::Ready
    });
    let host_snapshot = pair.host_application.ui_snapshot(&pair.host, true);
    let client_snapshot = pair.client_application.ui_snapshot(&pair.client, true);
    for snapshot in [host_snapshot, client_snapshot] {
        assert_eq!(snapshot.secure_remote_peers, 1);
        assert_eq!(snapshot.required_remote_peers, 1);
        assert_eq!(snapshot.verified_remote_accounts, 1);
        assert_eq!(snapshot.required_remote_accounts, 1);
        assert!(snapshot.all_members_ready);
    }

    pair.host_application
        .dispatch(
            &mut pair.host,
            NativeOnlineUiAction::StartMatch,
            pair.now_ms,
        )
        .unwrap();
    pair.host_application.set_content_ready(true);
    pair.client_application.set_content_ready(true);
    pair.pump_until(12_000, |pair| {
        pair.host.view_model().screen == NativeOnlineScreen::Fighting
            && pair.client.view_model().screen == NativeOnlineScreen::Fighting
            && pair.host_application.accepts_gameplay_input()
            && pair.client_application.accepts_gameplay_input()
    });
    assert_eq!(
        pair.host_application.active_session_kind(),
        Some(NativeOnlineSessionKind::ListenOwner)
    );
    assert_eq!(
        pair.client_application.active_session_kind(),
        Some(NativeOnlineSessionKind::RemoteClient)
    );
    assert_eq!(pair.host_application.metrics().sessions_started, 1);
    assert_eq!(pair.client_application.metrics().sessions_started, 1);
    assert_eq!(pair.host_application.metrics().endpoints_staged, 1);
    assert_eq!(pair.client_application.metrics().endpoints_staged, 1);
    assert!(pair.host.endpoints.is_empty());
    assert!(pair.client.endpoints.is_empty());

    // Put both coordinators into a legal Results transition so the
    // applications can exercise the client-first intent and the owner's real
    // graceful worker shutdown. These are ordinary public lifecycle commands;
    // no runtime view, authentication mapping, or endpoint is assigned.
    pair.now_ms = pair.now_ms.saturating_add(1);
    for core in [&mut pair.host, &mut pair.client] {
        core.execute(NativeOnlineCommand::BeginResultConfirmation, pair.now_ms)
            .unwrap();
        core.execute(NativeOnlineCommand::ConfirmResult, pair.now_ms)
            .unwrap();
    }
    // The client's request is retained in Results until the owner publishes
    // the next declaration epoch.
    pair.client_application
        .dispatch(
            &mut pair.client,
            NativeOnlineUiAction::ReturnToLobby,
            pair.now_ms,
        )
        .unwrap();
    assert_eq!(
        pair.client.coordinator.status().phase,
        OnlineLobbyPhase::Results
    );
    pair.host_application
        .dispatch(
            &mut pair.host,
            NativeOnlineUiAction::ReturnToLobby,
            pair.now_ms,
        )
        .unwrap();
    pair.pump_until(2_000, |pair| {
        pair.host.coordinator.status().phase == OnlineLobbyPhase::Lobby
            && pair.host_application.active_session_kind().is_none()
            && pair.client_application.active_session_kind().is_none()
            && pair.client.coordinator.status().phase == OnlineLobbyPhase::Results
    });
    // The authority worker's typed shutdown terminal deliberately replaces
    // the remote's speculative confirmed result with no-contest. The client
    // now accepts that authoritative outcome and returns without any test-side
    // state projection.
    pair.client_application
        .dispatch(
            &mut pair.client,
            NativeOnlineUiAction::ReturnToLobby,
            pair.now_ms,
        )
        .unwrap();
    assert_eq!(
        pair.host.coordinator.status().phase,
        OnlineLobbyPhase::Lobby
    );
    assert_eq!(
        pair.client.coordinator.status().phase,
        OnlineLobbyPhase::Lobby
    );
    assert!(pair.host_application.active_session_kind().is_none());
    assert!(pair.client_application.active_session_kind().is_none());

    // Leave before the next matchmaking generation is pumped. This proves
    // that both the just-retired gameplay generation and the queued next-lobby
    // generation are cancelled by normal application commands.
    pair.client_application
        .dispatch(
            &mut pair.client,
            NativeOnlineUiAction::LeaveOnline,
            pair.now_ms,
        )
        .unwrap();
    pair.host_application
        .dispatch(
            &mut pair.host,
            NativeOnlineUiAction::LeaveOnline,
            pair.now_ms,
        )
        .unwrap();
    pair.lobby = None;
    pair.pump_until(1_000, |pair| {
        pair.host.coordinator.status().phase == OnlineLobbyPhase::OfflineMenu
            && pair.client.coordinator.status().phase == OnlineLobbyPhase::OfflineMenu
            && pair.host.coordinator.retiring_transport_count() == 0
            && pair.client.coordinator.retiring_transport_count() == 0
    });
    assert!(pair.host.endpoints.is_empty());
    assert!(pair.client.endpoints.is_empty());
    let network = pair.network.clone();
    let host_control = pair.host_control.clone();
    let client_control = pair.client_control.clone();
    drop(pair);

    assert_eq!(
        network.resource_counts(),
        crate::steam_transport::FakeSteamTransportResourceCounts::default()
    );
    assert_eq!(host_control.active_issued_ticket_count(), 0);
    assert_eq!(client_control.active_issued_ticket_count(), 0);
    assert_eq!(host_control.active_auth_session_count(), 0);
    assert_eq!(client_control.active_auth_session_count(), 0);
}

#[test]
fn hostile_control_and_permanent_auth_isolation_persist_distinct_pregame_archives() {
    static NEXT_DIAGNOSTICS_ROOT: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(1);
    let sequence = NEXT_DIAGNOSTICS_ROOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "afc-native-online-pregame-isolation-{}-{sequence}",
        std::process::id()
    ));
    let archive = AuthorityDiagnosticsArchive::new(&root);
    let mut hostile = FakeNativeCorePair::new();
    hostile.pump_until_authenticated_endpoints();

    // Hostile AFCP semantics and a permanent Steam account rejection use
    // separate stable result codes, even though both teardown paths are
    // scoped to the one attributed peer and keep the lobby owner operational.
    hostile
        .host
        .isolate_signal_peer(hostile.client_user, AuthSignalError::InvalidEnvelope)
        .unwrap();
    hostile.host.persist_pregame_failure_traces_to(&archive);

    let mut permanent = FakeNativeCorePair::new();
    permanent
        .host_control
        .set_auth_outcome(
            permanent.client_user,
            FakeAuthOutcome {
                license_owner_user: permanent.client_user,
                validation: Err(crate::steam_platform::AuthValidationFailure::TicketInvalid),
                license: LicenseStatus::HasLicense,
            },
        )
        .unwrap();
    for _ in 0..120 {
        if permanent
            .host
            .signal_rejected_users
            .contains(&Some(permanent.client_user))
        {
            break;
        }
        permanent.now_ms += 1;
        permanent.mirror();
        permanent.host.pump(permanent.now_ms).unwrap();
        permanent.mirror();
        if permanent
            .host
            .signal_rejected_users
            .contains(&Some(permanent.client_user))
        {
            break;
        }
        permanent.client.pump(permanent.now_ms).unwrap();
        permanent.mirror();
    }
    assert!(
        permanent
            .host
            .signal_rejected_users
            .contains(&Some(permanent.client_user)),
        "permanent invalid-ticket callback did not reach peer-scoped isolation"
    );
    permanent.host.persist_pregame_failure_traces_to(&archive);

    assert!(
        hostile
            .host
            .coordinator
            .take_completed_peer_trace()
            .is_none()
    );
    assert!(
        permanent
            .host
            .coordinator
            .take_completed_peer_trace()
            .is_none()
    );
    assert_eq!(hostile.host.coordinator.status().lobby, Some(hostile.lobby));
    assert_eq!(
        permanent.host.coordinator.status().lobby,
        Some(permanent.lobby)
    );
    assert!(hostile.host.coordinator.status().failure.is_none());
    assert!(permanent.host.coordinator.status().failure.is_none());
    let mut paths = std::fs::read_dir(root.join("steam-pregame"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    paths.sort();
    assert_eq!(paths.len(), 2);
    let diagnostics = paths
        .iter()
        .map(|path| archive.load_steam_pregame_trace(path).unwrap())
        .collect::<Vec<_>>();
    let terminal_codes = diagnostics
        .iter()
        .map(|trace| trace.events.last().unwrap().result_code)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        terminal_codes,
        std::collections::BTreeSet::from([
            SteamTransportCloseReason::MalformedControlTraffic.diagnostic_code(),
            SteamTransportCloseReason::AuthenticationRejected.diagnostic_code(),
        ])
    );
    for path in &paths {
        let encoded = std::fs::read_to_string(path).unwrap();
        assert!(!encoded.contains(&hostile.host_user.get().to_string()));
        assert!(!encoded.contains(&hostile.client_user.get().to_string()));
    }

    drop(hostile);
    drop(permanent);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn recovered_control_retry_trace_remains_persistable_after_replacement_is_secure() {
    static NEXT_RETRY_DIAGNOSTICS_ROOT: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(1);
    let sequence = NEXT_RETRY_DIAGNOSTICS_ROOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "afc-native-online-pregame-retry-{}-{sequence}",
        std::process::id()
    ));
    let archive = AuthorityDiagnosticsArchive::new(&root);
    let mut pair = FakeNativeCorePair::new();
    pair.pump_until(40, |pair| {
        pair.host.ticket_exchanges.iter().flatten().count() == 1
            && pair.client.ticket_exchanges.iter().flatten().count() == 1
    });
    let first_connection = pair
        .client
        .coordinator
        .control_connection_for_user(pair.host_user)
        .unwrap();

    pair.network
        .disconnect_locally(first_connection, pair.client_user)
        .unwrap();
    pair.pump_once();
    pair.now_ms += crate::steam_transport::CONTROL_RETRY_DELAY_MS;
    pair.pump_until_authenticated_endpoints();
    let replacement = pair
        .client
        .coordinator
        .control_connection_for_user(pair.host_user)
        .unwrap();
    assert_ne!(replacement, first_connection);

    // Production invokes this shared sink when ControlRetrying crosses the
    // runtime boundary. Delaying the sink until after successful replacement
    // proves the coordinator drained the old active transport generation and
    // did not strand its trace in the live replacement.
    pair.host.persist_pregame_failure_traces_to(&archive);
    pair.client.persist_pregame_failure_traces_to(&archive);
    let paths = std::fs::read_dir(root.join("steam-pregame"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(paths.len(), 2);
    for path in paths {
        let trace = archive.load_steam_pregame_trace(&path).unwrap();
        assert!(trace.is_pregame_failure());
        assert_eq!(
            trace.events.last().unwrap().connection_generation,
            first_connection.get()
        );
    }

    drop(pair);
    std::fs::remove_dir_all(root).unwrap();
}
