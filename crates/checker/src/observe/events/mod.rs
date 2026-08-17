//! Strict protocol event classification owned by the checker.
//!
//! L1 and L2 classifiers match a literal `(emitter, topic0)` pair before using
//! the shared ABI decoder. Unknown topics from protocol emitters fail
//! closed.

use alloy_primitives::{Address, B256, Log};

use tempo_zone_contracts::{
    MAX_SEQUENCERS, MAX_TOKEN_METADATA_BYTES, TEMPO_STATE_ADDRESS, ZONE_FACTORY_ADDRESS,
    ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS,
};
use zone_precompiles::{ecies::COMPRESSED_PUBLIC_KEY_SIZE, outbox::MAX_CALLBACK_DATA_SIZE};

use alloy_primitives::IntoLogData;
use alloy_sol_types::{SolEvent as _, SolEventInterface};

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

/// Decode a listed ZoneFactory event or reject an unsupported topic.
fn decode_factory(log: &Log) -> Result<Option<Factory::ZoneCreated>, ProtocolEventError> {
    match required_topic(log)? {
        Factory::ZoneCreated::SIGNATURE_HASH | Factory::OwnershipTransferred::SIGNATURE_HASH => {}
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

/// Decode a listed ZoneInbox event or reject an unsupported topic.
fn decode_inbox(log: &Log) -> Result<Inbox::IZoneInboxEvents, ProtocolEventError> {
    match required_topic(log)? {
        Inbox::TempoAdvanced::SIGNATURE_HASH
        | Inbox::DepositProcessed::SIGNATURE_HASH
        | Inbox::DepositFailed::SIGNATURE_HASH
        | Inbox::WithdrawalBounceBackProcessed::SIGNATURE_HASH
        | Inbox::WithdrawalBounceBackPending::SIGNATURE_HASH
        | Inbox::RefundClaimed::SIGNATURE_HASH
        | Inbox::TokenEnabled::SIGNATURE_HASH => {}
        // Pinned Inbox topic that is deliberately excluded rather than unknown.
        Inbox::DepositRejected::SIGNATURE_HASH => return Err(unsupported(log)),
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

/// Decode a listed ZoneOutbox event or reject an unsupported topic.
fn decode_outbox(log: &Log) -> Result<Outbox::IZoneOutboxEvents, ProtocolEventError> {
    match required_topic(log)? {
        Outbox::WithdrawalRequested::SIGNATURE_HASH
        | Outbox::BatchFinalized::SIGNATURE_HASH
        | Outbox::TempoGasRateUpdated::SIGNATURE_HASH
        | Outbox::MaxWithdrawalsPerBlockUpdated::SIGNATURE_HASH => {}
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

/// Decode the single supported TempoState event.
fn decode_tempo_state(log: &Log) -> Result<TempoState::TempoStateEvents, ProtocolEventError> {
    if required_topic(log)? != TempoState::TempoBlockFinalized::SIGNATURE_HASH {
        return Err(unsupported(log));
    }
    strict_decode_interface(log, "TempoState event")
}
