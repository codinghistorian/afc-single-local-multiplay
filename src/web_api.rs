//! Versioned JSON contract shared by the hosted authority and browser client.
//!
//! Lobby and room messages are deliberately separate from AFC's canonical
//! gameplay protocol. They may select the immutable manifest for a future
//! match, but they never execute on the fixed-tick simulation schedule.

use serde::{Deserialize, Serialize};

use crate::network_protocol::MatchManifest;

pub const WEB_API_VERSION: u16 = 2;
pub const AFC_LOBBY_WEBSOCKET_SUBPROTOCOL: &str = "afc.lobby.v2";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceConfigResponse {
    pub api_version: u16,
    pub lobby_websocket_url: String,
    pub gameplay_websocket_url: String,
    pub webtransport_url: Option<String>,
    pub lobby_websocket_subprotocol: String,
    pub gameplay_websocket_subprotocol: String,
    pub release: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestSessionRequest {
    pub nickname: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestSessionResponse {
    pub guest_id: String,
    pub nickname: String,
    pub display_name: String,
    pub session_token: String,
    pub expires_at_unix_seconds: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomVisibility {
    Public,
    #[default]
    Private,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebCharacter {
    #[default]
    Cat,
    Pig,
    Dog,
    Fox,
    Panda,
    Bee,
    Penguin,
    Chick,
}

impl WebCharacter {
    pub const ALL: [Self; 8] = [
        Self::Cat,
        Self::Pig,
        Self::Dog,
        Self::Fox,
        Self::Panda,
        Self::Bee,
        Self::Penguin,
        Self::Chick,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Cat => "Cat",
            Self::Pig => "Pig",
            Self::Dog => "Dog",
            Self::Fox => "Fox",
            Self::Panda => "Panda",
            Self::Bee => "Bee",
            Self::Penguin => "Penguin",
            Self::Chick => "Chick",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRoomRequest {
    pub maximum_players: u8,
    pub visibility: RoomVisibility,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JoinRoomRequest {
    pub room_code: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateRoomSettingsRequest {
    pub expected_revision: u64,
    pub arena_index: usize,
    pub rule_index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectCharacterRequest {
    pub expected_revision: u64,
    pub character: WebCharacter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetReadyRequest {
    pub expected_revision: u64,
    pub ready: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KickMemberRequest {
    pub expected_revision: u64,
    pub peer_id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartRoomRequest {
    pub expected_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultAckRequest {
    pub match_id: String,
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
    pub countdown_start_tick: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomState {
    Open,
    Starting,
    Active,
    Results,
    Returning,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomMemberResponse {
    pub peer_id: u64,
    pub display_name: String,
    pub is_host: bool,
    pub is_self: bool,
    pub present: bool,
    pub gameplay_connected: bool,
    pub character: WebCharacter,
    pub ready: bool,
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
    pub countdown_start_tick: Option<u64>,
    pub connected_peers: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomResultResponse {
    pub match_id: String,
    pub result_id: u64,
    pub final_tick: u64,
    pub final_state_hash: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatScope {
    Global,
    Room,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessageResponse {
    pub message_id: u64,
    pub scope: ChatScope,
    pub room_code: Option<String>,
    pub sender_guest_id: String,
    pub sender_display_name: String,
    pub sent_at_unix_seconds: u64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomResponse {
    pub room_code: String,
    pub revision: u64,
    pub match_epoch: u64,
    pub visibility: RoomVisibility,
    pub state: RoomState,
    pub maximum_players: u8,
    pub member_count: u8,
    pub arena_index: usize,
    pub rule_index: usize,
    pub members: Vec<RoomMemberResponse>,
    pub room_chat: Vec<ChatMessageResponse>,
    pub manifest: Option<MatchManifest>,
    pub result: Option<RoomResultResponse>,
    pub worker: Option<RoomWorkerResponse>,
}

impl RoomResponse {
    pub fn self_member(&self) -> Option<&RoomMemberResponse> {
        self.members.iter().find(|member| member.is_self)
    }

    pub fn self_is_host(&self) -> bool {
        self.self_member().is_some_and(|member| member.is_host)
    }

    pub fn all_members_ready(&self) -> bool {
        self.member_count >= 2
            && self
                .members
                .iter()
                .all(|member| member.present && member.ready)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicRoomSummary {
    pub room_code: String,
    pub revision: u64,
    pub host_display_name: String,
    pub state: RoomState,
    pub member_count: u8,
    pub maximum_players: u8,
    pub arena_index: usize,
    pub rule_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LobbySnapshotResponse {
    pub revision: u64,
    pub online_guests: u32,
    pub public_rooms: Vec<PublicRoomSummary>,
    pub global_chat: Vec<ChatMessageResponse>,
    pub active_room: Option<RoomResponse>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatReportCategory {
    Spam,
    Harassment,
    InappropriateName,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LobbyClientMessage {
    Authenticate {
        session_token: String,
    },
    SendChat {
        client_nonce: u64,
        scope: ChatScope,
        text: String,
    },
    Report {
        client_nonce: u64,
        message_id: Option<u64>,
        guest_id: String,
        category: ChatReportCategory,
    },
    Resync,
    Ping {
        nonce: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LobbyServerMessage {
    Snapshot {
        snapshot: Box<LobbySnapshotResponse>,
    },
    StateChanged {
        revision: u64,
    },
    ChatMessage {
        message: ChatMessageResponse,
    },
    CommandAccepted {
        client_nonce: u64,
    },
    Pong {
        nonce: u64,
    },
    Error {
        client_nonce: Option<u64>,
        code: String,
    },
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
            r#"{"maximum_players":2,"visibility":"public","trust_me":true}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn room_and_control_wire_names_are_frozen() {
        assert_eq!(
            serde_json::to_string(&RoomState::Results).unwrap(),
            r#""results""#
        );
        assert_eq!(
            serde_json::to_string(&RoomWorkerPhase::WaitingForPeers).unwrap(),
            r#""waiting_for_peers""#
        );
        assert_eq!(
            serde_json::to_string(&LobbyClientMessage::Ping { nonce: 7 }).unwrap(),
            r#"{"type":"ping","nonce":7}"#
        );
    }
}
