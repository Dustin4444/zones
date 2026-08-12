//! Strict protocol event classification owned by the checker.
//!
//! L1 and L2 classifiers match a literal `(emitter, topic0)` pair before using
//! the shared ABI decoder. Unknown topics from protocol emitters fail
//! closed.

use alloy_primitives::{Address, B256, Log, b256};

use tempo_zone_contracts::{
    MAX_SEQUENCERS, MAX_TOKEN_METADATA_BYTES, TEMPO_STATE_ADDRESS, ZONE_FACTORY_ADDRESS,
    ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS,
};
use zone_precompiles::{ecies::COMPRESSED_PUBLIC_KEY_SIZE, outbox::MAX_CALLBACK_DATA_SIZE};

use std::mem::size_of;

use alloy_primitives::IntoLogData;
use alloy_sol_types::SolEventInterface;

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
    validate_max_bytes(log, event, "name", name.len(), MAX_TOKEN_METADATA_BYTES)?;
    validate_max_bytes(log, event, "symbol", symbol.len(), MAX_TOKEN_METADATA_BYTES)?;
    validate_max_bytes(
        log,
        event,
        "currency",
        currency.len(),
        MAX_TOKEN_METADATA_BYTES,
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
mod portal;

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
        return Ok(Some(match decode_factory(log)? {
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
        return decode_inbox(log).map(|event| Some(L2ProtocolEvent::Inbox(event)));
    }
    if log.address == ZONE_OUTBOX_ADDRESS {
        return decode_outbox(log).map(|event| Some(L2ProtocolEvent::Outbox(event)));
    }
    if log.address == TEMPO_STATE_ADDRESS {
        return decode_tempo_state(log).map(|event| Some(L2ProtocolEvent::TempoState(event)));
    }
    Ok(None)
}

const ZONE_CREATED_TOPIC: B256 =
    b256!("4f2c5b8ee43ce856328b02e8d2b193126eb1c13f34475bd902ecb9a1eaa826a4");
const OWNERSHIP_TRANSFERRED_TOPIC: B256 =
    b256!("8be0079c531659141344cd1fd0a4f28419497f9722a3daafe3b4186f6b6457e0");

/// Decode a listed ZoneFactory event or reject an unsupported topic.
fn decode_factory(log: &Log) -> Result<Option<Factory::ZoneCreated>, ProtocolEventError> {
    let topic = required_topic(log)?;
    match topic {
        ZONE_CREATED_TOPIC => preflight_address_array_count(log, "ZoneCreated", 4, MAX_SEQUENCERS)?,
        OWNERSHIP_TRANSFERRED_TOPIC => {}
        _ => return Err(unsupported(log)),
    }

    match strict_decode_interface::<Factory::ZoneFactoryEvents>(log, "ZoneFactory event")? {
        Factory::ZoneFactoryEvents::ZoneCreated(event) => {
            if event.sequencers.len() > MAX_SEQUENCERS {
                return Err(malformed(
                    log,
                    "ZoneCreated",
                    format!(
                        "address array length {} exceeds {MAX_SEQUENCERS}",
                        event.sequencers.len()
                    ),
                ));
            }
            Ok(Some(event))
        }
        Factory::ZoneFactoryEvents::OwnershipTransferred(_) => Ok(None),
    }
}

const TEMPO_ADVANCED_TOPIC: B256 =
    b256!("d2d2bf1e295f62cd08f0f0ab45818efeaba78b58310526f7b7e9686b8aeded1a");
const DEPOSIT_PROCESSED_TOPIC: B256 =
    b256!("d5277bc9597c7da3fab9cdbba4de6005f48b9eb7389cf2389c4ea9eea3172c21");
const DEPOSIT_FAILED_TOPIC: B256 =
    b256!("1fa7f545d9203bbedf7510083fa6f3ff73ddd3b3c99a7971ee5f91ef89e53c5b");
const WITHDRAWAL_BOUNCE_BACK_PROCESSED_TOPIC: B256 =
    b256!("bc1cf3ff6e619587619408b396a13775509216474757afb9d29bda1a96f590f0");
const WITHDRAWAL_BOUNCE_BACK_PENDING_TOPIC: B256 =
    b256!("92d91a50ae3561d4edd8eadad5f27f09116faaa4f733bd37682c49b4cbc20e39");
const REFUND_CLAIMED_TOPIC: B256 =
    b256!("ffd3bbab073ab4b2d0792c270104924c14c285a153b9acddabae166395d2eb5c");
const INBOX_TOKEN_ENABLED_TOPIC: B256 =
    b256!("4ac4dcc08b0c26c3fb6b58c64c1392b7934b1ce6b0382a5986ea5c3de795e053");
const DEPOSIT_REJECTED_TOPIC: B256 =
    b256!("4620415fad9c416306a56ca0ee640b3418628a5f2e45ddde3ddf7452a7a654fb");

/// Decode a listed ZoneInbox event or reject an unsupported topic.
fn decode_inbox(log: &Log) -> Result<Inbox::IZoneInboxEvents, ProtocolEventError> {
    match required_topic(log)? {
        TEMPO_ADVANCED_TOPIC
        | DEPOSIT_PROCESSED_TOPIC
        | DEPOSIT_FAILED_TOPIC
        | WITHDRAWAL_BOUNCE_BACK_PROCESSED_TOPIC
        | WITHDRAWAL_BOUNCE_BACK_PENDING_TOPIC
        | REFUND_CLAIMED_TOPIC
        | INBOX_TOKEN_ENABLED_TOPIC => {}
        DEPOSIT_REJECTED_TOPIC => return Err(unsupported(log)),
        _ => return Err(unsupported(log)),
    }

    let decoded = strict_decode_interface::<Inbox::IZoneInboxEvents>(log, "ZoneInbox event")?;
    if let Inbox::IZoneInboxEvents::TokenEnabled(event) = &decoded {
        validate_token_metadata(
            log,
            "TokenEnabled",
            &event.name,
            &event.symbol,
            &event.currency,
        )?;
    }
    Ok(decoded)
}

const WITHDRAWAL_REQUESTED_TOPIC: B256 =
    b256!("34ca953f3eed14157d2f660c7e92a5bd9c05be0d61a188830f3bf1cb7d094f96");
const BATCH_FINALIZED_TOPIC: B256 =
    b256!("ec4aff46c65f485f4b15e3c2edadda1d57d002995f5aa262a27c76b9a680ec16");
const TEMPO_GAS_RATE_UPDATED_TOPIC: B256 =
    b256!("6f864cce5237e12ffc9a99fc6c59af17222c2bbb3457690cc8753ab16b5d715e");
const MAX_WITHDRAWALS_PER_BLOCK_UPDATED_TOPIC: B256 =
    b256!("5340f6cf6e1274bffd0c7188c75f885ed6c90cd6b0879a646290a92f84a6dce3");

/// Decode a listed ZoneOutbox event or reject an unsupported topic.
fn decode_outbox(log: &Log) -> Result<Outbox::IZoneOutboxEvents, ProtocolEventError> {
    match required_topic(log)? {
        WITHDRAWAL_REQUESTED_TOPIC
        | BATCH_FINALIZED_TOPIC
        | TEMPO_GAS_RATE_UPDATED_TOPIC
        | MAX_WITHDRAWALS_PER_BLOCK_UPDATED_TOPIC => {}
        _ => return Err(unsupported(log)),
    }
    let decoded = strict_decode_interface::<Outbox::IZoneOutboxEvents>(log, "ZoneOutbox event")?;
    if let Outbox::IZoneOutboxEvents::WithdrawalRequested(event) = &decoded {
        validate_max_bytes(
            log,
            "WithdrawalRequested",
            "data",
            event.data.len(),
            MAX_CALLBACK_DATA_SIZE,
        )?;
        if !matches!(event.revealTo.len(), 0 | COMPRESSED_PUBLIC_KEY_SIZE) {
            return Err(malformed(
                log,
                "WithdrawalRequested",
                format!(
                    "revealTo byte length {}, expected 0 or {COMPRESSED_PUBLIC_KEY_SIZE}",
                    event.revealTo.len()
                ),
            ));
        }
    }
    Ok(decoded)
}

const BLOCK_FINALIZED_TOPIC: B256 =
    b256!("dd85219569c3c880f014955916f426d1ca039714b59ce33e24f151f155ac26b9");

/// Decode the single supported TempoState event.
fn decode_tempo_state(log: &Log) -> Result<TempoState::TempoStateEvents, ProtocolEventError> {
    if required_topic(log)? != BLOCK_FINALIZED_TOPIC {
        return Err(unsupported(log));
    }
    strict_decode_interface(log, "TempoState event")
}

#[cfg(test)]
mod tests;
