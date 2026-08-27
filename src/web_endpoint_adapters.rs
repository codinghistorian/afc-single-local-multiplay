//! Browser and hosted-server adapters for opaque AFC datagrams.
//!
//! AFC reliability, sequence numbers, canonical messages, and the 1,200-byte
//! ceiling remain owned by [`crate::network_runtime`]. These adapters only
//! preserve datagram boundaries and translate bounded nonblocking queues to
//! WebSocket binary messages or WebTransport datagrams.

use core::fmt;

use crate::network_io::{MAX_AFC_DATAGRAM_BYTES, MAX_IN_PROCESS_QUEUE_PACKETS};

pub const AFC_WEBSOCKET_SUBPROTOCOL: &str = "afc.datagram.v1";
pub const DEFAULT_WEB_ENDPOINT_QUEUE_PACKETS: usize = 256;
pub const DEFAULT_WEBSOCKET_BUFFERED_BYTES: u32 = 256 * 1_024;
pub const MAX_WEBSOCKET_BUFFERED_BYTES: u32 = 16 * 1_024 * 1_024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WebEndpointConfig {
    pub inbound_capacity_packets: usize,
    pub outbound_capacity_packets: usize,
    /// Browser `WebSocket.bufferedAmount` admission ceiling. This is separate
    /// from the AFC runtime's bounded packet queue.
    pub websocket_buffered_bytes: u32,
}

impl Default for WebEndpointConfig {
    fn default() -> Self {
        Self {
            inbound_capacity_packets: DEFAULT_WEB_ENDPOINT_QUEUE_PACKETS,
            outbound_capacity_packets: DEFAULT_WEB_ENDPOINT_QUEUE_PACKETS,
            websocket_buffered_bytes: DEFAULT_WEBSOCKET_BUFFERED_BYTES,
        }
    }
}

impl WebEndpointConfig {
    pub fn validate(self) -> Result<(), WebEndpointConfigError> {
        if self.inbound_capacity_packets == 0
            || self.inbound_capacity_packets > MAX_IN_PROCESS_QUEUE_PACKETS
        {
            return Err(WebEndpointConfigError::InboundCapacity);
        }
        if self.outbound_capacity_packets == 0
            || self.outbound_capacity_packets > MAX_IN_PROCESS_QUEUE_PACKETS
        {
            return Err(WebEndpointConfigError::OutboundCapacity);
        }
        if self.websocket_buffered_bytes < MAX_AFC_DATAGRAM_BYTES as u32
            || self.websocket_buffered_bytes > MAX_WEBSOCKET_BUFFERED_BYTES
        {
            return Err(WebEndpointConfigError::WebSocketBufferedBytes);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebEndpointConfigError {
    InboundCapacity,
    OutboundCapacity,
    WebSocketBufferedBytes,
}

impl fmt::Display for WebEndpointConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid AFC web endpoint configuration: {self:?}"
        )
    }
}

impl std::error::Error for WebEndpointConfigError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WebEndpointMetrics {
    pub inbound_depth_packets: usize,
    pub inbound_high_water_packets: usize,
    pub outbound_depth_packets: usize,
    pub outbound_high_water_packets: usize,
    pub inbound_packets: u64,
    pub outbound_packets: u64,
    pub inbound_queue_full: u64,
    pub outbound_queue_full: u64,
    pub malformed_frames: u64,
    pub oversized_frames: u64,
    pub transport_errors: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebEndpointKind {
    WebTransport,
    WebSocket,
}

#[cfg(all(feature = "web-server", not(target_arch = "wasm32")))]
mod server;
#[cfg(all(feature = "web-server", not(target_arch = "wasm32")))]
pub use server::{
    DEFAULT_WEB_ADMISSION_TIMEOUT, ServerDatagramBridge, ServerDatagramEndpoint,
    ServerIngressOutcome, ServerWebAdapterError, WebTransportAdmissionResponder,
    acknowledge_websocket_admission, receive_websocket_admission, receive_webtransport_admission,
    run_websocket_datagram_adapter, run_webtransport_datagram_adapter,
};

#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
pub use browser::{
    BrowserDatagramEndpoint, BrowserTransportPreference, BrowserWebEndpointBuildError,
    BrowserWebSocketEndpoint, BrowserWebTransportEndpoint,
    connect_browser_admitted_datagram_endpoint, connect_browser_datagram_endpoint,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_configuration_has_strict_packet_and_byte_bounds() {
        WebEndpointConfig::default().validate().unwrap();
        assert_eq!(
            WebEndpointConfig {
                inbound_capacity_packets: 0,
                ..WebEndpointConfig::default()
            }
            .validate(),
            Err(WebEndpointConfigError::InboundCapacity)
        );
        assert_eq!(
            WebEndpointConfig {
                outbound_capacity_packets: MAX_IN_PROCESS_QUEUE_PACKETS + 1,
                ..WebEndpointConfig::default()
            }
            .validate(),
            Err(WebEndpointConfigError::OutboundCapacity)
        );
        assert_eq!(
            WebEndpointConfig {
                websocket_buffered_bytes: MAX_AFC_DATAGRAM_BYTES as u32 - 1,
                ..WebEndpointConfig::default()
            }
            .validate(),
            Err(WebEndpointConfigError::WebSocketBufferedBytes)
        );
    }

    #[test]
    fn websocket_subprotocol_is_stable_and_versioned() {
        assert_eq!(AFC_WEBSOCKET_SUBPROTOCOL, "afc.datagram.v1");
    }
}
