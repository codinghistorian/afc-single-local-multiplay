use core::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::{TryRecvError, TrySendError};

use super::{WebEndpointConfig, WebEndpointConfigError, WebEndpointMetrics};
use crate::network_io::{
    AfcDatagram, MAX_AFC_DATAGRAM_BYTES, NonBlockingDatagramEndpoint, ReceiveOutcome, SendOutcome,
};

#[derive(Default)]
struct ServerEndpointShared {
    disconnected: AtomicBool,
    inbound_depth: AtomicUsize,
    inbound_high_water: AtomicUsize,
    outbound_depth: AtomicUsize,
    outbound_high_water: AtomicUsize,
    inbound_packets: AtomicU64,
    outbound_packets: AtomicU64,
    inbound_queue_full: AtomicU64,
    outbound_queue_full: AtomicU64,
    malformed_frames: AtomicU64,
    oversized_frames: AtomicU64,
    transport_errors: AtomicU64,
}

impl ServerEndpointShared {
    fn metrics(&self) -> WebEndpointMetrics {
        WebEndpointMetrics {
            inbound_depth_packets: self.inbound_depth.load(Ordering::Relaxed),
            inbound_high_water_packets: self.inbound_high_water.load(Ordering::Relaxed),
            outbound_depth_packets: self.outbound_depth.load(Ordering::Relaxed),
            outbound_high_water_packets: self.outbound_high_water.load(Ordering::Relaxed),
            inbound_packets: self.inbound_packets.load(Ordering::Relaxed),
            outbound_packets: self.outbound_packets.load(Ordering::Relaxed),
            inbound_queue_full: self.inbound_queue_full.load(Ordering::Relaxed),
            outbound_queue_full: self.outbound_queue_full.load(Ordering::Relaxed),
            malformed_frames: self.malformed_frames.load(Ordering::Relaxed),
            oversized_frames: self.oversized_frames.load(Ordering::Relaxed),
            transport_errors: self.transport_errors.load(Ordering::Relaxed),
        }
    }

    fn disconnect(&self) {
        self.disconnected.store(true, Ordering::Release);
    }
}

/// Synchronous hub-facing endpoint. It contains no socket and performs no
/// async work; the corresponding bridge is owned by one Tokio connection task.
pub struct ServerDatagramEndpoint {
    inbound: mpsc::Receiver<AfcDatagram>,
    outbound: mpsc::Sender<AfcDatagram>,
    shared: Arc<ServerEndpointShared>,
}

impl ServerDatagramEndpoint {
    pub fn metrics(&self) -> WebEndpointMetrics {
        self.shared.metrics()
    }
}

impl NonBlockingDatagramEndpoint for ServerDatagramEndpoint {
    fn try_send(&mut self, datagram: AfcDatagram) -> SendOutcome {
        if self.shared.disconnected.load(Ordering::Acquire) {
            return SendOutcome::Disconnected(datagram);
        }
        let depth = self
            .shared
            .outbound_depth
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        match self.outbound.try_send(datagram) {
            Ok(()) => {
                self.shared
                    .outbound_high_water
                    .fetch_max(depth, Ordering::Relaxed);
                SendOutcome::Sent
            }
            Err(TrySendError::Full(datagram)) => {
                self.shared.outbound_depth.fetch_sub(1, Ordering::AcqRel);
                self.shared
                    .outbound_queue_full
                    .fetch_add(1, Ordering::Relaxed);
                SendOutcome::Full(datagram)
            }
            Err(TrySendError::Closed(datagram)) => {
                self.shared.outbound_depth.fetch_sub(1, Ordering::AcqRel);
                self.shared.disconnect();
                SendOutcome::Disconnected(datagram)
            }
        }
    }

    fn try_receive(&mut self) -> ReceiveOutcome {
        match self.inbound.try_recv() {
            Ok(datagram) => {
                let _ = self.shared.inbound_depth.fetch_update(
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                    |depth| Some(depth.saturating_sub(1)),
                );
                ReceiveOutcome::Received(datagram)
            }
            Err(TryRecvError::Empty) => {
                if self.shared.disconnected.load(Ordering::Acquire) {
                    ReceiveOutcome::Disconnected
                } else {
                    ReceiveOutcome::Empty
                }
            }
            Err(TryRecvError::Disconnected) => {
                self.shared.disconnect();
                ReceiveOutcome::Disconnected
            }
        }
    }
}

/// Async transport-facing half paired with [`ServerDatagramEndpoint`].
pub struct ServerDatagramBridge {
    inbound: mpsc::Sender<AfcDatagram>,
    outbound: mpsc::Receiver<AfcDatagram>,
    shared: Arc<ServerEndpointShared>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ServerIngressOutcome {
    Delivered,
    Full(AfcDatagram),
    Disconnected(AfcDatagram),
}

impl ServerDatagramBridge {
    pub fn pair(
        config: WebEndpointConfig,
    ) -> Result<(ServerDatagramEndpoint, Self), WebEndpointConfigError> {
        config.validate()?;
        let (inbound_tx, inbound_rx) = mpsc::channel(config.inbound_capacity_packets);
        let (outbound_tx, outbound_rx) = mpsc::channel(config.outbound_capacity_packets);
        let shared = Arc::new(ServerEndpointShared::default());
        Ok((
            ServerDatagramEndpoint {
                inbound: inbound_rx,
                outbound: outbound_tx,
                shared: Arc::clone(&shared),
            },
            Self {
                inbound: inbound_tx,
                outbound: outbound_rx,
                shared,
            },
        ))
    }

    pub fn try_deliver_inbound(&self, datagram: AfcDatagram) -> ServerIngressOutcome {
        if self.shared.disconnected.load(Ordering::Acquire) {
            return ServerIngressOutcome::Disconnected(datagram);
        }
        let depth = self
            .shared
            .inbound_depth
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        match self.inbound.try_send(datagram) {
            Ok(()) => {
                self.shared
                    .inbound_high_water
                    .fetch_max(depth, Ordering::Relaxed);
                self.shared.inbound_packets.fetch_add(1, Ordering::Relaxed);
                ServerIngressOutcome::Delivered
            }
            Err(TrySendError::Full(datagram)) => {
                self.shared.inbound_depth.fetch_sub(1, Ordering::AcqRel);
                self.shared
                    .inbound_queue_full
                    .fetch_add(1, Ordering::Relaxed);
                ServerIngressOutcome::Full(datagram)
            }
            Err(TrySendError::Closed(datagram)) => {
                self.shared.inbound_depth.fetch_sub(1, Ordering::AcqRel);
                self.shared.disconnect();
                ServerIngressOutcome::Disconnected(datagram)
            }
        }
    }

    pub async fn receive_outbound(&mut self) -> Option<AfcDatagram> {
        let datagram = self.outbound.recv().await?;
        let _ =
            self.shared
                .outbound_depth
                .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |depth| {
                    Some(depth.saturating_sub(1))
                });
        self.shared.outbound_packets.fetch_add(1, Ordering::Relaxed);
        Some(datagram)
    }

    pub fn metrics(&self) -> WebEndpointMetrics {
        self.shared.metrics()
    }

    pub fn disconnect(&self) {
        self.shared.disconnect();
    }

    fn malformed(&self) {
        self.shared.malformed_frames.fetch_add(1, Ordering::Relaxed);
    }

    fn oversized(&self) {
        self.shared.oversized_frames.fetch_add(1, Ordering::Relaxed);
    }

    fn transport_error(&self) {
        self.shared.transport_errors.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for ServerDatagramBridge {
    fn drop(&mut self) {
        self.shared.disconnect();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerWebAdapterError {
    WebSocketReceive,
    WebSocketSend,
    NonBinaryWebSocketFrame,
    OversizedDatagram { observed: usize },
    InboundQueueFull,
    EndpointDisconnected,
    WebTransportDatagramsUnavailable,
    WebTransportDatagramCeiling { negotiated: usize },
    WebTransportReceive,
    WebTransportSend,
}

impl fmt::Display for ServerWebAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "AFC web transport adapter failed: {self:?}")
    }
}

impl std::error::Error for ServerWebAdapterError {}

pub async fn run_websocket_datagram_adapter(
    socket: WebSocket,
    mut bridge: ServerDatagramBridge,
) -> Result<(), ServerWebAdapterError> {
    let result = run_websocket_datagram_adapter_inner(socket, &mut bridge).await;
    bridge.disconnect();
    result
}

async fn run_websocket_datagram_adapter_inner(
    socket: WebSocket,
    bridge: &mut ServerDatagramBridge,
) -> Result<(), ServerWebAdapterError> {
    let (mut sender, mut receiver) = socket.split();
    loop {
        tokio::select! {
            outbound = bridge.receive_outbound() => {
                let Some(outbound) = outbound else {
                    return Ok(());
                };
                sender
                    .send(Message::binary(outbound.as_slice().to_vec()))
                    .await
                    .map_err(|_| {
                        bridge.transport_error();
                        ServerWebAdapterError::WebSocketSend
                    })?;
            }
            inbound = receiver.next() => {
                let Some(inbound) = inbound else {
                    return Ok(());
                };
                match inbound.map_err(|_| {
                    bridge.transport_error();
                    ServerWebAdapterError::WebSocketReceive
                })? {
                    Message::Binary(bytes) => deliver_bytes(bridge, &bytes)?,
                    Message::Close(_) => return Ok(()),
                    Message::Ping(_) | Message::Pong(_) => {}
                    Message::Text(_) => {
                        bridge.malformed();
                        return Err(ServerWebAdapterError::NonBinaryWebSocketFrame);
                    }
                }
            }
        }
    }
}

pub async fn run_webtransport_datagram_adapter(
    connection: wtransport::Connection,
    mut bridge: ServerDatagramBridge,
) -> Result<(), ServerWebAdapterError> {
    let result = run_webtransport_datagram_adapter_inner(&connection, &mut bridge).await;
    bridge.disconnect();
    result
}

async fn run_webtransport_datagram_adapter_inner(
    connection: &wtransport::Connection,
    bridge: &mut ServerDatagramBridge,
) -> Result<(), ServerWebAdapterError> {
    let negotiated = connection
        .max_datagram_size()
        .ok_or(ServerWebAdapterError::WebTransportDatagramsUnavailable)?;
    if negotiated < MAX_AFC_DATAGRAM_BYTES {
        return Err(ServerWebAdapterError::WebTransportDatagramCeiling { negotiated });
    }
    loop {
        tokio::select! {
            outbound = bridge.receive_outbound() => {
                let Some(outbound) = outbound else {
                    return Ok(());
                };
                connection.send_datagram(outbound.as_slice()).map_err(|_| {
                    bridge.transport_error();
                    ServerWebAdapterError::WebTransportSend
                })?;
            }
            inbound = connection.receive_datagram() => {
                let inbound = inbound.map_err(|_| {
                    bridge.transport_error();
                    ServerWebAdapterError::WebTransportReceive
                })?;
                deliver_bytes(bridge, &inbound)?;
            }
        }
    }
}

fn deliver_bytes(bridge: &ServerDatagramBridge, bytes: &[u8]) -> Result<(), ServerWebAdapterError> {
    if bytes.len() > MAX_AFC_DATAGRAM_BYTES {
        bridge.oversized();
        return Err(ServerWebAdapterError::OversizedDatagram {
            observed: bytes.len(),
        });
    }
    let datagram = AfcDatagram::try_from_slice(bytes).map_err(|_| {
        bridge.oversized();
        ServerWebAdapterError::OversizedDatagram {
            observed: bytes.len(),
        }
    })?;
    match bridge.try_deliver_inbound(datagram) {
        ServerIngressOutcome::Delivered => Ok(()),
        ServerIngressOutcome::Full(_) => Err(ServerWebAdapterError::InboundQueueFull),
        ServerIngressOutcome::Disconnected(_) => Err(ServerWebAdapterError::EndpointDisconnected),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bridge_is_bounded_bidirectional_and_disconnects_fail_closed() {
        let config = WebEndpointConfig {
            inbound_capacity_packets: 1,
            outbound_capacity_packets: 1,
            ..WebEndpointConfig::default()
        };
        let (mut endpoint, mut bridge) = ServerDatagramBridge::pair(config).unwrap();
        let first = AfcDatagram::try_from_slice(b"first").unwrap();
        assert_eq!(
            bridge.try_deliver_inbound(first.clone()),
            ServerIngressOutcome::Delivered
        );
        assert!(matches!(
            bridge.try_deliver_inbound(first.clone()),
            ServerIngressOutcome::Full(_)
        ));
        assert_eq!(endpoint.try_receive(), ReceiveOutcome::Received(first));

        let outbound = AfcDatagram::try_from_slice(b"outbound").unwrap();
        assert_eq!(endpoint.try_send(outbound.clone()), SendOutcome::Sent);
        assert!(matches!(
            endpoint.try_send(outbound.clone()),
            SendOutcome::Full(_)
        ));
        assert_eq!(bridge.receive_outbound().await, Some(outbound));
        assert_eq!(endpoint.metrics().inbound_high_water_packets, 1);
        assert_eq!(endpoint.metrics().outbound_high_water_packets, 1);

        bridge.disconnect();
        assert!(matches!(
            endpoint.try_send(AfcDatagram::default()),
            SendOutcome::Disconnected(_)
        ));
        assert_eq!(endpoint.try_receive(), ReceiveOutcome::Disconnected);
    }

    #[test]
    fn byte_delivery_rejects_oversize_and_queue_overload() {
        let config = WebEndpointConfig {
            inbound_capacity_packets: 1,
            ..WebEndpointConfig::default()
        };
        let (_endpoint, bridge) = ServerDatagramBridge::pair(config).unwrap();
        deliver_bytes(&bridge, b"accepted").unwrap();
        assert_eq!(
            deliver_bytes(&bridge, b"full"),
            Err(ServerWebAdapterError::InboundQueueFull)
        );
        let oversized = vec![0; MAX_AFC_DATAGRAM_BYTES + 1];
        assert_eq!(
            deliver_bytes(&bridge, &oversized),
            Err(ServerWebAdapterError::OversizedDatagram {
                observed: MAX_AFC_DATAGRAM_BYTES + 1
            })
        );
    }
}
