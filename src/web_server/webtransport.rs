use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinSet;
use tracing::{info, warn};
use wtransport::endpoint::endpoint_side;
use wtransport::{Endpoint, Identity, ServerConfig, VarInt};

use super::config::WebTransportListenerConfig;
use super::rate_limit::RateBucket;
use super::{
    OwnedActiveConnection, WebServerRunError, WebServerState, canonical_ip, unix_now_for_transport,
};
use crate::web_endpoint_adapters::{
    ServerDatagramBridge, receive_webtransport_admission, run_webtransport_datagram_adapter,
};

const WEBTRANSPORT_PATH: &str = "/v1/connect/wt";
const CONNECTION_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(10);
const TASK_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
const CLOSE_CODE: VarInt = VarInt::from_u32(1);

pub(super) struct BoundWebTransportListener {
    endpoint: Endpoint<endpoint_side::Server>,
    bind: std::net::SocketAddr,
}

pub(super) async fn bind_webtransport_listener(
    config: &WebTransportListenerConfig,
) -> Result<BoundWebTransportListener, WebServerRunError> {
    let identity = Identity::load_pemfiles(&config.certificate_pem, &config.private_key_pem)
        .await
        .map_err(|error| WebServerRunError::WebTransport(error.to_string()))?;
    let server_config = ServerConfig::builder()
        .with_bind_address(config.bind)
        .with_identity(identity)
        .max_idle_timeout(Some(CONNECTION_IDLE_TIMEOUT))
        .map_err(|error| WebServerRunError::WebTransport(error.to_string()))?
        .keep_alive_interval(Some(KEEP_ALIVE_INTERVAL))
        .build();
    let endpoint = Endpoint::server(server_config)
        .map_err(|error| WebServerRunError::WebTransport(error.to_string()))?;
    Ok(BoundWebTransportListener {
        endpoint,
        bind: config.bind,
    })
}

impl BoundWebTransportListener {
    pub(super) async fn run(
        self,
        state: WebServerState,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), WebServerRunError> {
        let endpoint = self.endpoint;
        info!(bind = %self.bind, "AFC WebTransport listener ready");
        let mut tasks = JoinSet::new();

        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                incoming = endpoint.accept() => {
                    if !incoming.remote_address_validated() {
                        incoming.retry();
                        continue;
                    }
                    let Some(active) = OwnedActiveConnection::try_webtransport(
                        Arc::clone(&state.metrics),
                        state.maximum_transport_sessions,
                    ) else {
                        // Refuse before starting a handshake task. The active
                        // connection ceiling must also bound pending work when
                        // a UDP flood arrives at a saturated listener.
                        state
                            .metrics
                            .webtransport_rejected
                            .fetch_add(1, Ordering::Relaxed);
                        incoming.refuse();
                        continue;
                    };
                    let connection_state = state.clone();
                    tasks.spawn(async move {
                        let _active = active;
                        handle_incoming(incoming, connection_state).await;
                    });
                }
                completed = tasks.join_next(), if !tasks.is_empty() => {
                    if completed.is_some_and(|result| result.is_err()) {
                        warn!("WebTransport connection task was cancelled");
                    }
                }
            }
        }

        endpoint.close(CLOSE_CODE, b"server shutdown");
        let deadline = tokio::time::Instant::now() + TASK_DRAIN_TIMEOUT;
        while !tasks.is_empty() {
            if tokio::time::timeout_at(deadline, tasks.join_next())
                .await
                .is_err()
            {
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub(super) async fn run_webtransport_listener(
    config: WebTransportListenerConfig,
    state: WebServerState,
    shutdown: watch::Receiver<bool>,
) -> Result<(), WebServerRunError> {
    bind_webtransport_listener(&config)
        .await?
        .run(state, shutdown)
        .await
}

async fn handle_incoming(incoming: wtransport::endpoint::IncomingSession, state: WebServerState) {
    let Ok(request) = incoming.await else {
        state
            .metrics
            .webtransport_rejected
            .fetch_add(1, Ordering::Relaxed);
        return;
    };
    if request.path() != WEBTRANSPORT_PATH {
        request.not_found().await;
        state
            .metrics
            .webtransport_rejected
            .fetch_add(1, Ordering::Relaxed);
        return;
    }
    let origin = request.origin().map(str::to_owned);
    if !origin
        .as_deref()
        .is_some_and(|origin| state.origin_allowed(origin))
    {
        request.forbidden().await;
        state
            .metrics
            .webtransport_rejected
            .fetch_add(1, Ordering::Relaxed);
        return;
    }
    let now = match unix_now_for_transport() {
        Ok(now) => now,
        Err(()) => {
            request.forbidden().await;
            state
                .metrics
                .webtransport_rejected
                .fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
    let address = canonical_ip(request.remote_address().ip());
    if state
        .rate_limiter
        .check(address, RateBucket::Admission, now)
        .is_err()
    {
        state.metrics.rate_limited.fetch_add(1, Ordering::Relaxed);
        request.too_many_requests().await;
        return;
    }
    let headers = origin
        .map(|origin| vec![("access-control-allow-origin", origin)])
        .unwrap_or_default();
    let Ok(connection) = request.accept_with_headers(headers).await else {
        state
            .metrics
            .webtransport_rejected
            .fetch_add(1, Ordering::Relaxed);
        return;
    };

    let admitted = async {
        let (ticket, responder) =
            receive_webtransport_admission(&connection, state.admission_timeout)
                .await
                .map_err(|_| ())?;
        let now = unix_now_for_transport()?;
        let (endpoint, bridge) = ServerDatagramBridge::pair(state.endpoint).map_err(|_| ())?;
        state
            .rooms
            .admit_join_ticket(&ticket, endpoint, now)
            .await
            .map_err(|_| ())?;
        responder.acknowledge().await.map_err(|_| ())?;
        Ok::<_, ()>(bridge)
    }
    .await;
    let Ok(bridge) = admitted else {
        state
            .metrics
            .webtransport_rejected
            .fetch_add(1, Ordering::Relaxed);
        connection.close(CLOSE_CODE, b"admission failure");
        warn!("WebTransport admission rejected");
        return;
    };
    state
        .metrics
        .webtransport_admitted
        .fetch_add(1, Ordering::Relaxed);
    if run_webtransport_datagram_adapter(connection.clone(), bridge)
        .await
        .is_err()
    {
        state
            .metrics
            .webtransport_adapter_errors
            .fetch_add(1, Ordering::Relaxed);
        connection.close(CLOSE_CODE, b"transport failure");
        warn!("WebTransport gameplay transport retired with an adapter error");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web_admission::{ADMISSION_ACCEPTED_FRAME, encode_admission_request};
    use crate::web_endpoint_adapters::WebEndpointConfig;
    use crate::web_identity::{
        JoinTicketMode, WebTokenKeyring, WebTokenLifetimes, WebTokenSigningKey,
    };
    use crate::web_room::{WebPrivateRoomOptions, WebRoomServiceConfig};
    use crate::web_server::{WebDeploymentMode, WebRateLimitConfig, WebServerConfig};
    use wtransport::endpoint::ConnectOptions;
    use wtransport::{ClientConfig, Endpoint, Identity};

    #[tokio::test]
    async fn real_http3_admission_stream_attaches_before_datagrams_are_enabled() {
        let temp = std::env::temp_dir().join(format!(
            "afc-webtransport-test-{}-{}",
            std::process::id(),
            getrandom::u64().unwrap()
        ));
        tokio::fs::create_dir_all(&temp).await.unwrap();
        let certificate_pem = temp.join("certificate.pem");
        let private_key_pem = temp.join("private-key.pem");
        let identity = Identity::self_signed(["localhost", "127.0.0.1"]).unwrap();
        let certificate_hash = identity.certificate_chain().as_slice()[0].hash();
        identity
            .certificate_chain()
            .store_pemfile(&certificate_pem)
            .await
            .unwrap();
        identity
            .private_key()
            .store_secret_pemfile(&private_key_pem)
            .await
            .unwrap();
        let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let bind = probe.local_addr().unwrap();
        drop(probe);

        let key = WebTokenSigningKey::new(1, [0x61; 32]).unwrap();
        let server_config = WebServerConfig {
            deployment: WebDeploymentMode::Development,
            http_bind: "127.0.0.1:0".parse().unwrap(),
            webtransport: Some(WebTransportListenerConfig {
                bind,
                certificate_pem,
                private_key_pem,
            }),
            public_websocket_url: "ws://127.0.0.1:8080/v1/connect/ws".to_owned(),
            public_webtransport_url: Some(format!(
                "https://127.0.0.1:{}/v1/connect/wt",
                bind.port()
            )),
            allowed_origins: vec!["https://html-classic.itch.zone".to_owned()],
            trusted_proxy_ips: Vec::new(),
            token_keyring: WebTokenKeyring::new(key, None, WebTokenLifetimes::default()).unwrap(),
            room: WebRoomServiceConfig::default(),
            endpoint: WebEndpointConfig::default(),
            rate_limit: WebRateLimitConfig::default(),
            admission_timeout: Duration::from_secs(2),
            maximum_request_body_bytes: 16 * 1_024,
            maximum_transport_sessions: 64,
        };
        let state = WebServerState::new(&server_config).unwrap();
        let now = unix_now_for_transport().unwrap();
        let host = state.rooms.issue_guest_session(now).unwrap();
        let guest = state.rooms.issue_guest_session(now).unwrap();
        let room = state
            .rooms
            .create_private_room(
                &host.token,
                WebPrivateRoomOptions {
                    maximum_players: 2,
                    ..WebPrivateRoomOptions::default()
                },
                now,
            )
            .unwrap();
        let room_code = room.room_code.to_string();
        state
            .rooms
            .join_private_room(&guest.token, &room_code, now)
            .unwrap();
        state
            .rooms
            .start_private_room(&host.token, &room_code, now)
            .await
            .unwrap();
        let ticket = state
            .rooms
            .issue_join_ticket(&host.token, &room_code, JoinTicketMode::Initial, now)
            .unwrap();

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let listener_config = server_config.webtransport.clone().unwrap();
        let listener_state = state.clone();
        let listener = tokio::spawn(async move {
            run_webtransport_listener(listener_config, listener_state, shutdown_rx)
                .await
                .unwrap();
        });
        let client_config = ClientConfig::builder()
            .with_bind_default()
            .with_server_certificate_hashes([certificate_hash])
            .build();
        let client = Endpoint::client(client_config).unwrap();
        let options =
            ConnectOptions::builder(format!("https://127.0.0.1:{}/v1/connect/wt", bind.port()))
                .add_header("origin", "https://html-classic.itch.zone")
                .build();
        let mut connection = None;
        for _ in 0..100 {
            match client.connect(options.clone()).await {
                Ok(connected) => {
                    connection = Some(connected);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
        let connection = connection.expect("WebTransport listener did not accept a real client");
        let (mut sender, mut receiver) = connection.open_bi().await.unwrap().await.unwrap();
        sender
            .write_all(&encode_admission_request(&ticket.ticket).unwrap())
            .await
            .unwrap();
        sender.finish().await.unwrap();
        let mut accepted = [0_u8; ADMISSION_ACCEPTED_FRAME.len()];
        receiver.read_exact(&mut accepted).await.unwrap();
        assert_eq!(accepted, ADMISSION_ACCEPTED_FRAME);
        for _ in 0..100 {
            if state.metrics.webtransport_admitted.load(Ordering::Relaxed) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(
            state.metrics.webtransport_admitted.load(Ordering::Relaxed),
            1
        );

        connection.close(CLOSE_CODE, b"test complete");
        state.rooms.shutdown_all().await;
        let _ = shutdown_tx.send(true);
        listener.await.unwrap();
        client.close(CLOSE_CODE, b"test complete");
        let _ = tokio::fs::remove_dir_all(temp).await;
    }
}
