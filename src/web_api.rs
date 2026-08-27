//! Versioned JSON contract shared by the hosted authority and browser client.
//!
//! Keeping these data-transfer types outside either endpoint prevents a server
//! refactor from silently changing the browser lobby protocol. Gameplay bytes
//! still use the canonical binary protocol after ticket admission.

use serde::{Deserialize, Serialize};

use crate::network_protocol::MatchManifest;

pub const WEB_API_VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceConfigResponse {
    pub api_version: u16,
    pub websocket_url: String,
    pub webtransport_url: Option<String>,
    pub websocket_subprotocol: String,
    pub release: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestSessionResponse {
    pub guest_id: String,
    pub session_token: String,
    pub expires_at_unix_seconds: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRoomRequest {
    pub maximum_players: u8,
    pub arena_index: usize,
    pub rule_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JoinRoomRequest {
    pub room_code: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketModeRequest {
    Initial,
    Reconnect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketRequest {
    pub mode: TicketModeRequest,
    pub last_confirmed_tick: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketResponse {
    pub ticket: String,
    pub expires_at_unix_seconds: u64,
    pub peer_id: u64,
    pub manifest: MatchManifest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomState {
    Open,
    Starting,
    Active,
    Finished,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomMemberResponse {
    pub peer_id: u64,
    pub is_host: bool,
    pub is_self: bool,
    pub connected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomWorkerPhase {
    Starting,
    WaitingForPeers,
    Countdown,
    Fighting,
    Finished,
    Draining,
    Stopped,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomWorkerResponse {
    pub phase: RoomWorkerPhase,
    pub network_tick: u64,
    pub simulation_tick: u64,
    pub connected_peers: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomResponse {
    pub room_code: String,
    pub state: RoomState,
    pub maximum_players: u8,
    pub member_count: u8,
    pub members: Vec<RoomMemberResponse>,
    pub manifest: Option<MatchManifest>,
    pub worker: Option<RoomWorkerResponse>,
}

impl RoomResponse {
    pub fn self_member(&self) -> Option<RoomMemberResponse> {
        self.members.iter().copied().find(|member| member.is_self)
    }

    pub fn self_is_host(&self) -> bool {
        self.self_member().is_some_and(|member| member.is_host)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiErrorEnvelope {
    pub error: ApiErrorBody,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiErrorBody {
    pub code: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_contract_rejects_unknown_fields() {
        let error = serde_json::from_str::<CreateRoomRequest>(
            r#"{"maximum_players":2,"arena_index":0,"rule_index":0,"trust_me":true}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn room_states_have_frozen_wire_names() {
        assert_eq!(
            serde_json::to_string(&RoomState::Open).unwrap(),
            r#""open""#
        );
        assert_eq!(
            serde_json::to_string(&RoomWorkerPhase::WaitingForPeers).unwrap(),
            r#""waiting_for_peers""#
        );
    }
}
