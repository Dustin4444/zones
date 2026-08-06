//! Version-pinned protocol event surface owned by the checker.
//!
//! L1 and L2 classification are deliberately separate. Each classifier first
//! matches a literal `(emitter, topic0)` pair and only then invokes the
//! checker-owned ABI decoder. This is not a production ABI adapter or an
//! extensible event registry: an unknown topic from a protocol emitter fails
//! closed.

use alloy_primitives::{Address, B256, Log};

use super::constants::{
    TEMPO_STATE_ADDRESS, ZONE_FACTORY_ADDRESS, ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS,
};

mod common;
mod factory;
mod inbox;
mod outbox;
mod portal;
mod tempo_state;

#[cfg(test)]
mod tests;

pub(crate) use factory::Factory;
pub(crate) use inbox::Inbox;
pub(crate) use outbox::Outbox;
pub(crate) use tempo_state::TempoState;

pub(crate) use portal::{Portal, PortalModelEvent};

/// A strictly decoded, model-driving L1 protocol event.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum L1ProtocolEvent {
    Portal(PortalModelEvent),
    FactoryZoneCreated(Factory::ZoneCreated),
    /// A listed event whose payload was strictly decoded and then discarded
    /// because it cannot change the release-one model.
    KnownNonModel,
}

/// A strictly decoded L2 protocol event.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum L2ProtocolEvent {
    Inbox(Inbox::InboxEvents),
    Outbox(Outbox::OutboxEvents),
    TempoState(TempoState::TempoStateEvents),
}

/// Fail-closed classifications for logs from protocol emitters.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ProtocolEventError {
    /// The emitter is protocol-owned but the topic is not supported. This also
    /// covers the explicitly excluded pinned-Inbox `DepositRejected` topic.
    #[error("unsupported protocol event from {emitter} with topic {topic0:?}")]
    UnsupportedProtocolEvent {
        emitter: Address,
        topic0: Option<B256>,
    },
    /// The topic is known for this emitter but its topics/data are malformed,
    /// non-canonical, or outside a pinned dynamic-field bound.
    #[error("malformed {event} from {emitter} at topic {topic0}: {reason}")]
    MalformedProtocolEvent {
        emitter: Address,
        topic0: B256,
        event: &'static str,
        reason: String,
    },
}

/// Classify one authenticated L1 log from the configured Portal/factory set.
///
/// `Ok(None)` is reserved for emitters outside this L1 protocol set. Every
/// topicless, unknown, or malformed log from the configured Portal or fixed
/// ZoneFactory address fails closed.
pub(crate) fn classify_l1_protocol_event(
    configured_portal: Address,
    log: &Log,
) -> Result<Option<L1ProtocolEvent>, ProtocolEventError> {
    if log.address == configured_portal {
        Ok(Some(match portal::decode(log)? {
            Some(event) => L1ProtocolEvent::Portal(event),
            None => L1ProtocolEvent::KnownNonModel,
        }))
    } else if log.address == ZONE_FACTORY_ADDRESS {
        Ok(Some(match factory::decode(log)? {
            Some(event) if event.portal == configured_portal => {
                L1ProtocolEvent::FactoryZoneCreated(event)
            }
            Some(_) | None => L1ProtocolEvent::KnownNonModel,
        }))
    } else {
        Ok(None)
    }
}

/// Classify one authenticated L2 log from the fixed native protocol emitters.
///
/// `Ok(None)` is reserved for emitters outside the Inbox/Outbox/TempoState set.
/// Unknown or malformed logs from any fixed protocol emitter fail closed.
pub(crate) fn classify_l2_protocol_event(
    log: &Log,
) -> Result<Option<L2ProtocolEvent>, ProtocolEventError> {
    if log.address == ZONE_INBOX_ADDRESS {
        inbox::decode(log).map(|event| Some(L2ProtocolEvent::Inbox(event)))
    } else if log.address == ZONE_OUTBOX_ADDRESS {
        outbox::decode(log).map(|event| Some(L2ProtocolEvent::Outbox(event)))
    } else if log.address == TEMPO_STATE_ADDRESS {
        tempo_state::decode(log).map(|event| Some(L2ProtocolEvent::TempoState(event)))
    } else {
        Ok(None)
    }
}
