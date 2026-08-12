//! Strict protocol event classification owned by the checker.
//!
//! L1 and L2 classifiers match a literal `(emitter, topic0)` pair before using
//! the shared ABI decoder. Unknown topics from protocol emitters fail
//! closed.

use alloy_primitives::{Address, B256, Log};

use tempo_zone_contracts::{
    TEMPO_STATE_ADDRESS, ZONE_FACTORY_ADDRESS, ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS,
};

use std::mem::size_of;

use alloy_primitives::IntoLogData;
use alloy_sol_types::SolEventInterface;

use tempo_zone_contracts::{
    MAX_TOKEN_CURRENCY_BYTES, MAX_TOKEN_NAME_BYTES, MAX_TOKEN_SYMBOL_BYTES,
};

pub(super) fn required_topic(log: &Log) -> Result<B256, ProtocolEventError> {
    log.topics()
        .first()
        .copied()
        .ok_or_else(|| unsupported(log))
}

pub(super) fn unsupported(log: &Log) -> ProtocolEventError {
    ProtocolEventError::UnsupportedProtocolEvent {
        emitter: log.address,
        topic0: log.topics().first().copied(),
    }
}

pub(super) fn malformed(
    log: &Log,
    event: &'static str,
    reason: impl Into<String>,
) -> ProtocolEventError {
    ProtocolEventError::MalformedProtocolEvent {
        emitter: log.address,
        topic0: log.topics().first().copied().unwrap_or(B256::ZERO),
        event,
        reason: reason.into(),
    }
}

/// Decode through the shared generated event interface and reject any
/// encoding that does not round-trip byte-for-byte.
pub(super) fn strict_decode_interface<E>(
    log: &Log,
    emitter: &'static str,
) -> Result<E, ProtocolEventError>
where
    E: SolEventInterface + IntoLogData,
{
    let decoded = E::decode_log(log)
        .map_err(|error| malformed(log, emitter, error.to_string()))?
        .data;
    if decoded.to_log_data() != log.data {
        return Err(malformed(log, emitter, "non-canonical ABI encoding"));
    }
    Ok(decoded)
}

/// Bound a dynamic address-array count before Alloy allocates its decoded Vec.
/// Canonical offsets, elements, padding, and trailing bytes remain the
/// responsibility of strict decode/re-encode equality.
pub(super) fn preflight_address_array_count(
    log: &Log,
    event: &'static str,
    head_word: usize,
    maximum: usize,
) -> Result<(), ProtocolEventError> {
    let data = log.data.data.as_ref();
    let head_offset = head_word
        .checked_mul(32)
        .ok_or_else(|| malformed(log, event, "dynamic head offset overflow"))?;
    let tail_offset =
        read_usize_word(data, head_offset).map_err(|reason| malformed(log, event, reason))?;
    let count =
        read_usize_word(data, tail_offset).map_err(|reason| malformed(log, event, reason))?;
    if count > maximum {
        return Err(malformed(
            log,
            event,
            format!("address array length {count} exceeds {maximum}"),
        ));
    }
    Ok(())
}

pub(super) fn validate_token_metadata(
    log: &Log,
    event: &'static str,
    name: &str,
    symbol: &str,
    currency: &str,
) -> Result<(), ProtocolEventError> {
    validate_max_bytes(log, event, "name", name.len(), MAX_TOKEN_NAME_BYTES)?;
    validate_max_bytes(log, event, "symbol", symbol.len(), MAX_TOKEN_SYMBOL_BYTES)?;
    validate_max_bytes(
        log,
        event,
        "currency",
        currency.len(),
        MAX_TOKEN_CURRENCY_BYTES,
    )
}

pub(super) fn validate_max_bytes(
    log: &Log,
    event: &'static str,
    field: &'static str,
    actual: usize,
    maximum: usize,
) -> Result<(), ProtocolEventError> {
    if actual > maximum {
        return Err(malformed(
            log,
            event,
            format!("{field} byte length {actual} exceeds {maximum}"),
        ));
    }
    Ok(())
}

pub(super) fn validate_exact_bytes(
    log: &Log,
    event: &'static str,
    field: &'static str,
    actual: usize,
    expected: usize,
) -> Result<(), ProtocolEventError> {
    if actual != expected {
        return Err(malformed(
            log,
            event,
            format!("{field} byte length {actual}, expected {expected}"),
        ));
    }
    Ok(())
}

fn read_usize_word(data: &[u8], offset: usize) -> Result<usize, String> {
    let end = offset
        .checked_add(32)
        .ok_or_else(|| "ABI word offset overflow".to_owned())?;
    let word = data
        .get(offset..end)
        .ok_or_else(|| "ABI word exceeds log body".to_owned())?;
    let width = size_of::<usize>();
    if word[..32 - width].iter().any(|byte| *byte != 0) {
        return Err("ABI offset/length does not fit usize".to_owned());
    }
    let mut value = 0usize;
    for byte in &word[32 - width..] {
        value = (value << 8) | usize::from(*byte);
    }
    Ok(value)
}
mod factory;
mod inbox;
mod outbox;
mod portal;
mod tempo_state;

pub(crate) use tempo_zone_contracts::{
    IZoneInbox as Inbox, IZoneOutbox as Outbox, TempoState, ZoneFactory as Factory,
    ZonePortal as Portal,
};

/// A strictly decoded L1 protocol event.
#[derive(Debug)]
pub(crate) enum L1ProtocolEvent {
    Portal(Portal::ZonePortalEvents),
    FactoryZoneCreated(Factory::ZoneCreated),
    /// A listed event that cannot change checker state.
    KnownIgnored,
}

/// A strictly decoded L2 protocol event.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum L2ProtocolEvent {
    Inbox(Inbox::IZoneInboxEvents),
    Outbox(Outbox::IZoneOutboxEvents),
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
        return Ok(Some(match portal::decode(log)? {
            Some(event) => L1ProtocolEvent::Portal(event),
            None => L1ProtocolEvent::KnownIgnored,
        }));
    }
    if log.address == ZONE_FACTORY_ADDRESS {
        return Ok(Some(match factory::decode(log)? {
            Some(event) if event.portal == configured_portal => {
                L1ProtocolEvent::FactoryZoneCreated(event)
            }
            Some(_) | None => L1ProtocolEvent::KnownIgnored,
        }));
    }
    Ok(None)
}

/// Classify one authenticated L2 log from the fixed native protocol emitters.
///
/// `Ok(None)` is reserved for emitters outside the Inbox/Outbox/TempoState set.
/// Unknown or malformed logs from any fixed protocol emitter fail closed.
pub(crate) fn classify_l2_protocol_event(
    log: &Log,
) -> Result<Option<L2ProtocolEvent>, ProtocolEventError> {
    if log.address == ZONE_INBOX_ADDRESS {
        return inbox::decode(log).map(|event| Some(L2ProtocolEvent::Inbox(event)));
    }
    if log.address == ZONE_OUTBOX_ADDRESS {
        return outbox::decode(log).map(|event| Some(L2ProtocolEvent::Outbox(event)));
    }
    if log.address == TEMPO_STATE_ADDRESS {
        return tempo_state::decode(log).map(|event| Some(L2ProtocolEvent::TempoState(event)));
    }
    Ok(None)
}

#[cfg(test)]
mod tests;
