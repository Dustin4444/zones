//! Strict protocol event classification owned by the checker.
//!
//! L1 and L2 classifiers decode generated event interfaces, then apply the
//! checker's exhaustive modeled-or-ignored policy. Unknown topics from protocol
//! emitters fail closed.

use alloy_primitives::{Address, B256, IntoLogData, Log};

use tempo_zone_contracts::{
    MAX_SEQUENCERS, MAX_TOKEN_METADATA_BYTES, TEMPO_STATE_ADDRESS, ZONE_FACTORY_ADDRESS,
    ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS,
};
use zone_precompiles::{
    ecies::{COMPRESSED_PUBLIC_KEY_SIZE, ENCRYPTED_PAYLOAD_PLAINTEXT_SIZE},
    outbox::MAX_CALLBACK_DATA_SIZE,
};

use alloy_sol_types::SolEventInterface;

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
        return Ok(Some(match decode_portal(log)? {
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

/// Decode one generated ZoneFactory event and classify its checker relevance.
fn decode_factory(log: &Log) -> Result<Option<Factory::ZoneCreated>, ProtocolEventError> {
    match decode_canonical_interface::<Factory::ZoneFactoryEvents>(
        log,
        "ZoneFactory event",
        Factory::ZoneFactoryEvents::signature_by_selector,
    )? {
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

/// Decode one generated ZoneInbox event and reject excluded protocol behavior.
fn decode_inbox(log: &Log) -> Result<Inbox::IZoneInboxEvents, ProtocolEventError> {
    let decoded = decode_canonical_interface::<Inbox::IZoneInboxEvents>(
        log,
        "ZoneInbox event",
        Inbox::IZoneInboxEvents::signature_by_selector,
    )?;
    match &decoded {
        // Pinned protocol behavior that the checker deliberately does not model.
        Inbox::IZoneInboxEvents::DepositRejected(_) => return Err(unsupported(log)),
        Inbox::IZoneInboxEvents::TokenEnabled(event) => validate_token_metadata(
            log,
            "TokenEnabled",
            &event.name,
            &event.symbol,
            &event.currency,
        )?,
        Inbox::IZoneInboxEvents::TempoAdvanced(_)
        | Inbox::IZoneInboxEvents::DepositProcessed(_)
        | Inbox::IZoneInboxEvents::DepositFailed(_)
        | Inbox::IZoneInboxEvents::WithdrawalBounceBackProcessed(_)
        | Inbox::IZoneInboxEvents::WithdrawalBounceBackPending(_)
        | Inbox::IZoneInboxEvents::RefundClaimed(_) => {}
    }
    Ok(decoded)
}

/// Decode one generated ZoneOutbox event and validate bounded fields.
fn decode_outbox(log: &Log) -> Result<Outbox::IZoneOutboxEvents, ProtocolEventError> {
    let decoded = decode_canonical_interface::<Outbox::IZoneOutboxEvents>(
        log,
        "ZoneOutbox event",
        Outbox::IZoneOutboxEvents::signature_by_selector,
    )?;
    match &decoded {
        Outbox::IZoneOutboxEvents::WithdrawalRequested(event) => {
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
        Outbox::IZoneOutboxEvents::BatchFinalized(_)
        | Outbox::IZoneOutboxEvents::TempoGasRateUpdated(_)
        | Outbox::IZoneOutboxEvents::MaxWithdrawalsPerBlockUpdated(_) => {}
    }
    Ok(decoded)
}

/// Decode the generated TempoState event interface.
fn decode_tempo_state(log: &Log) -> Result<TempoState::TempoStateEvents, ProtocolEventError> {
    match decode_canonical_interface::<TempoState::TempoStateEvents>(
        log,
        "TempoState event",
        TempoState::TempoStateEvents::signature_by_selector,
    )? {
        event @ TempoState::TempoStateEvents::TempoBlockFinalized(_) => Ok(event),
    }
}

/// Decode one generated Portal event and retain checker-relevant state changes.
fn decode_portal(log: &Log) -> Result<Option<Portal::ZonePortalEvents>, ProtocolEventError> {
    let decoded = decode_canonical_interface::<Portal::ZonePortalEvents>(
        log,
        "Portal event",
        Portal::ZonePortalEvents::signature_by_selector,
    )?;
    let changes_checker_state = match &decoded {
        Portal::ZonePortalEvents::DepositMade(event) => {
            validate_exact_bytes(
                log,
                "DepositMade",
                "ciphertext",
                event.ciphertext.len(),
                ENCRYPTED_PAYLOAD_PLAINTEXT_SIZE,
            )?;
            true
        }
        Portal::ZonePortalEvents::TokenEnabled(event) => {
            validate_token_metadata(
                log,
                "TokenEnabled",
                &event.name,
                &event.symbol,
                &event.currency,
            )?;
            true
        }
        Portal::ZonePortalEvents::SequencerSetUpdated(event)
            if event.sequencers.len() > MAX_SEQUENCERS =>
        {
            return Err(malformed(
                log,
                "SequencerSetUpdated",
                format!(
                    "address array length {} exceeds {MAX_SEQUENCERS}",
                    event.sequencers.len()
                ),
            ));
        }
        Portal::ZonePortalEvents::BatchSubmitted(_)
        | Portal::ZonePortalEvents::WithdrawalProcessed(_)
        | Portal::ZonePortalEvents::WithdrawalBounceBack(_)
        | Portal::ZonePortalEvents::DepositBounceBack(_)
        | Portal::ZonePortalEvents::DepositBounceBackPending(_)
        | Portal::ZonePortalEvents::RefundClaimed(_)
        | Portal::ZonePortalEvents::BouncebackGasUpdated(_) => true,
        Portal::ZonePortalEvents::DepositsPaused(_)
        | Portal::ZonePortalEvents::DepositsResumed(_)
        | Portal::ZonePortalEvents::PortalPaused(_)
        | Portal::ZonePortalEvents::PortalResumed(_)
        | Portal::ZonePortalEvents::AbdicationScheduled(_)
        | Portal::ZonePortalEvents::RpcUrlUpdated(_)
        | Portal::ZonePortalEvents::SequencerEncryptionKeyUpdated(_)
        | Portal::ZonePortalEvents::ZoneGasRateUpdated(_)
        | Portal::ZonePortalEvents::MaxTempoGasRateUpdated(_)
        | Portal::ZonePortalEvents::AdminTransferStarted(_)
        | Portal::ZonePortalEvents::AdminTransferred(_)
        | Portal::ZonePortalEvents::RoleUpdated(_)
        | Portal::ZonePortalEvents::EnforcementModesUpdated(_)
        | Portal::ZonePortalEvents::LeaderUpdated(_) => false,
        Portal::ZonePortalEvents::SequencerSetUpdated(_) => false,
    };
    Ok(changes_checker_state.then_some(decoded))
}

fn required_topic(log: &Log) -> Result<B256, ProtocolEventError> {
    log.topics()
        .first()
        .copied()
        .ok_or_else(|| unsupported(log))
}

fn unsupported(log: &Log) -> ProtocolEventError {
    ProtocolEventError::UnsupportedProtocolEvent {
        emitter: log.address,
        topic0: log.topics().first().copied(),
    }
}

fn malformed(log: &Log, event: &'static str, reason: impl Into<String>) -> ProtocolEventError {
    ProtocolEventError::MalformedProtocolEvent {
        emitter: log.address,
        topic0: log.topics().first().copied().unwrap_or(B256::ZERO),
        event,
        reason: reason.into(),
    }
}

/// Decode through the generated event interface and require canonical encoding.
fn decode_canonical_interface<E>(
    log: &Log,
    emitter: &'static str,
    signature_by_selector: fn([u8; 32]) -> Option<&'static str>,
) -> Result<E, ProtocolEventError>
where
    E: SolEventInterface + IntoLogData,
{
    if signature_by_selector(required_topic(log)?.0).is_none() {
        return Err(unsupported(log));
    }
    let decoded = E::decode_log(log)
        .map_err(|error| malformed(log, emitter, error.to_string()))?
        .data;
    if decoded.to_log_data() != log.data {
        return Err(malformed(log, emitter, "non-canonical ABI encoding"));
    }
    Ok(decoded)
}

fn validate_token_metadata(
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

fn validate_max_bytes(
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

fn validate_exact_bytes(
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
