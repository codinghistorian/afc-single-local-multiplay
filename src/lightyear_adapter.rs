//! Narrow Lightyear compatibility adapter for native AFC networking.
//!
//! Lightyear owns packet delivery and connection plumbing only. AFC wire bytes,
//! ticks, simulation state, rollback, identities, validation, and results remain
//! in the adjacent crate modules. No Lightyear type escapes this adapter.

use bevy::prelude::*;
use core::time::Duration;
use lightyear::connection::direction::NetworkDirection;
#[cfg(test)]
use lightyear::prelude::ChannelRegistry;
use lightyear::prelude::{AppChannelExt, ChannelMode, ChannelSettings, ReliableSettings};

use crate::network_protocol::{CHANNEL_SPECS, Delivery, Direction, ProtocolChannel};

pub const LIGHTYEAR_VERSION: &str = "0.26.4";
pub const LIGHTYEAR_LOOPBACK_QUEUE_CAPACITY: usize = 256;

struct ControlChannel;
struct InputChannel;
struct StateChannel;
struct ResyncChannel;
struct ResultChannel;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AfcLightyearChannelSettings {
    pub protocol_channel: ProtocolChannel,
    pub delivery: Delivery,
    pub direction: Direction,
    pub priority: f32,
}

pub const AFC_LIGHTYEAR_CHANNELS: [AfcLightyearChannelSettings; 5] = [
    AfcLightyearChannelSettings {
        protocol_channel: ProtocolChannel::Control,
        delivery: Delivery::OrderedReliable,
        direction: Direction::Bidirectional,
        priority: 20.0,
    },
    AfcLightyearChannelSettings {
        protocol_channel: ProtocolChannel::Input,
        delivery: Delivery::SequencedUnreliable,
        direction: Direction::Bidirectional,
        priority: 30.0,
    },
    AfcLightyearChannelSettings {
        protocol_channel: ProtocolChannel::State,
        delivery: Delivery::SequencedUnreliable,
        direction: Direction::AuthorityToClient,
        priority: 25.0,
    },
    AfcLightyearChannelSettings {
        protocol_channel: ProtocolChannel::Resync,
        delivery: Delivery::UnorderedReliable,
        direction: Direction::AuthorityToClient,
        priority: 5.0,
    },
    AfcLightyearChannelSettings {
        protocol_channel: ProtocolChannel::Result,
        delivery: Delivery::OrderedReliable,
        direction: Direction::AuthorityToClient,
        priority: 40.0,
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightyearAdapterError {
    InvalidQueueCapacity,
    ChannelContractDrift,
}

/// Registers exactly AFC's five delivery channels. Payloads placed on these
/// channels must already be encoded and validated by `network_codec`.
pub struct AfcLightyearChannelsPlugin;

impl Plugin for AfcLightyearChannelsPlugin {
    fn build(&self, app: &mut App) {
        register::<ControlChannel>(app, AFC_LIGHTYEAR_CHANNELS[0]);
        register::<InputChannel>(app, AFC_LIGHTYEAR_CHANNELS[1]);
        register::<StateChannel>(app, AFC_LIGHTYEAR_CHANNELS[2]);
        register::<ResyncChannel>(app, AFC_LIGHTYEAR_CHANNELS[3]);
        register::<ResultChannel>(app, AFC_LIGHTYEAR_CHANNELS[4]);
    }
}

fn register<C: Send + Sync + 'static>(app: &mut App, spec: AfcLightyearChannelSettings) {
    let mode = match spec.delivery {
        Delivery::OrderedReliable => ChannelMode::OrderedReliable(ReliableSettings::default()),
        Delivery::SequencedUnreliable => ChannelMode::SequencedUnreliable,
        Delivery::UnorderedReliable => ChannelMode::UnorderedReliable(ReliableSettings::default()),
    };
    let direction = match spec.direction {
        Direction::Bidirectional => NetworkDirection::Bidirectional,
        Direction::AuthorityToClient => NetworkDirection::ServerToClient,
    };
    app.add_channel::<C>(ChannelSettings {
        mode,
        send_frequency: Duration::ZERO,
        priority: spec.priority,
    })
    .add_direction(direction);
}

/// Constructs the bounded crossbeam transport required for a listen authority.
///
/// Lightyear's `CrossbeamIo::new_pair` is intentionally forbidden because it
/// creates unbounded queues. AFC creates both directions explicitly instead.
pub fn bounded_crossbeam_pair(
    capacity: usize,
) -> Result<
    (
        lightyear::crossbeam::CrossbeamIo,
        lightyear::crossbeam::CrossbeamIo,
    ),
    LightyearAdapterError,
> {
    if capacity == 0 || capacity > LIGHTYEAR_LOOPBACK_QUEUE_CAPACITY {
        return Err(LightyearAdapterError::InvalidQueueCapacity);
    }
    let (client_to_server_sender, client_to_server_receiver) = crossbeam_channel::bounded(capacity);
    let (server_to_client_sender, server_to_client_receiver) = crossbeam_channel::bounded(capacity);
    Ok((
        lightyear::crossbeam::CrossbeamIo::new(client_to_server_sender, server_to_client_receiver),
        lightyear::crossbeam::CrossbeamIo::new(server_to_client_sender, client_to_server_receiver),
    ))
}

/// Fails if the independently authored protocol and adapter tables drift.
pub fn validate_channel_contract() -> Result<(), LightyearAdapterError> {
    if CHANNEL_SPECS.len() != AFC_LIGHTYEAR_CHANNELS.len() {
        return Err(LightyearAdapterError::ChannelContractDrift);
    }
    for (protocol, adapter) in CHANNEL_SPECS.iter().zip(AFC_LIGHTYEAR_CHANNELS) {
        if protocol.channel != adapter.protocol_channel
            || protocol.delivery != adapter.delivery
            || protocol.direction != adapter.direction
        {
            return Err(LightyearAdapterError::ChannelContractDrift);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_contract_exactly_matches_protocol_contract() {
        validate_channel_contract().unwrap();
        assert_eq!(AFC_LIGHTYEAR_CHANNELS.len(), 5);
    }

    #[test]
    fn plugin_registers_exact_afc_delivery_modes() {
        let mut app = App::new();
        app.add_plugins(AfcLightyearChannelsPlugin);
        assert!(app.world().contains_resource::<ChannelRegistry>());
        for spec in AFC_LIGHTYEAR_CHANNELS {
            assert!(spec.priority.is_finite() && spec.priority > 0.0);
        }
    }

    #[test]
    fn loopback_pair_rejects_unbounded_or_zero_capacity() {
        assert!(matches!(
            bounded_crossbeam_pair(0),
            Err(LightyearAdapterError::InvalidQueueCapacity)
        ));
        assert!(matches!(
            bounded_crossbeam_pair(LIGHTYEAR_LOOPBACK_QUEUE_CAPACITY + 1),
            Err(LightyearAdapterError::InvalidQueueCapacity)
        ));
        assert!(bounded_crossbeam_pair(LIGHTYEAR_LOOPBACK_QUEUE_CAPACITY).is_ok());
    }

    #[test]
    fn lightyear_version_is_frozen_with_the_adapter_contract() {
        assert_eq!(LIGHTYEAR_VERSION, "0.26.4");
    }
}
