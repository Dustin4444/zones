//! Strict protocol event classification owned by the checker.
//!
//! L1 and L2 classifiers match a literal `(emitter, topic0)` pair before using
//! the shared ABI decoder. Unknown topics from protocol emitters fail
//! closed.

use alloy_primitives::{Address, B256, Log};

use tempo_zone_contracts::{
    TEMPO_STATE_ADDRESS, ZONE_FACTORY_ADDRESS, ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS,
};

mod common;
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
        Ok(Some(match portal::decode(log)? {
            Some(event) => L1ProtocolEvent::Portal(event),
            None => L1ProtocolEvent::KnownIgnored,
        }))
    } else if log.address == ZONE_FACTORY_ADDRESS {
        Ok(Some(match factory::decode(log)? {
            Some(event) if event.portal == configured_portal => {
                L1ProtocolEvent::FactoryZoneCreated(event)
            }
            Some(_) | None => L1ProtocolEvent::KnownIgnored,
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

#[cfg(test)]
mod tests {
    use alloy_sol_types::SolEvent as _;
    use tempo_zone_contracts::{IZoneInbox, IZoneOutbox, TempoState, ZoneFactory, ZonePortal};

    use super::{factory, inbox, outbox, portal, tempo_state};

    #[test]
    fn independent_topics_match_shared_wire_types() {
        let topics = [
            (
                portal::DEPOSIT_MADE_TOPIC,
                ZonePortal::DepositMade::SIGNATURE_HASH,
            ),
            (
                portal::TOKEN_ENABLED_TOPIC,
                ZonePortal::TokenEnabled::SIGNATURE_HASH,
            ),
            (
                portal::BATCH_SUBMITTED_TOPIC,
                ZonePortal::BatchSubmitted::SIGNATURE_HASH,
            ),
            (
                portal::WITHDRAWAL_PROCESSED_TOPIC,
                ZonePortal::WithdrawalProcessed::SIGNATURE_HASH,
            ),
            (
                portal::WITHDRAWAL_BOUNCE_BACK_TOPIC,
                ZonePortal::WithdrawalBounceBack::SIGNATURE_HASH,
            ),
            (
                portal::DEPOSIT_BOUNCE_BACK_TOPIC,
                ZonePortal::DepositBounceBack::SIGNATURE_HASH,
            ),
            (
                portal::DEPOSIT_BOUNCE_BACK_PENDING_TOPIC,
                ZonePortal::DepositBounceBackPending::SIGNATURE_HASH,
            ),
            (
                portal::REFUND_CLAIMED_TOPIC,
                ZonePortal::RefundClaimed::SIGNATURE_HASH,
            ),
            (
                portal::BOUNCEBACK_GAS_UPDATED_TOPIC,
                ZonePortal::BouncebackGasUpdated::SIGNATURE_HASH,
            ),
            (
                portal::SEQUENCER_ENCRYPTION_KEY_UPDATED_TOPIC,
                ZonePortal::SequencerEncryptionKeyUpdated::SIGNATURE_HASH,
            ),
            (
                portal::ZONE_GAS_RATE_UPDATED_TOPIC,
                ZonePortal::ZoneGasRateUpdated::SIGNATURE_HASH,
            ),
            (
                portal::MAX_TEMPO_GAS_RATE_UPDATED_TOPIC,
                ZonePortal::MaxTempoGasRateUpdated::SIGNATURE_HASH,
            ),
            (
                portal::ADMIN_TRANSFER_STARTED_TOPIC,
                ZonePortal::AdminTransferStarted::SIGNATURE_HASH,
            ),
            (
                portal::ADMIN_TRANSFERRED_TOPIC,
                ZonePortal::AdminTransferred::SIGNATURE_HASH,
            ),
            (
                portal::ROLE_UPDATED_TOPIC,
                ZonePortal::RoleUpdated::SIGNATURE_HASH,
            ),
            (
                portal::ENFORCEMENT_MODES_UPDATED_TOPIC,
                ZonePortal::EnforcementModesUpdated::SIGNATURE_HASH,
            ),
            (
                portal::SEQUENCER_SET_UPDATED_TOPIC,
                ZonePortal::SequencerSetUpdated::SIGNATURE_HASH,
            ),
            (
                portal::LEADER_UPDATED_TOPIC,
                ZonePortal::LeaderUpdated::SIGNATURE_HASH,
            ),
            (
                portal::DEPOSITS_PAUSED_TOPIC,
                ZonePortal::DepositsPaused::SIGNATURE_HASH,
            ),
            (
                portal::DEPOSITS_RESUMED_TOPIC,
                ZonePortal::DepositsResumed::SIGNATURE_HASH,
            ),
            (
                portal::RPC_URL_UPDATED_TOPIC,
                ZonePortal::RpcUrlUpdated::SIGNATURE_HASH,
            ),
            (
                factory::ZONE_CREATED_TOPIC,
                ZoneFactory::ZoneCreated::SIGNATURE_HASH,
            ),
            (
                factory::OWNERSHIP_TRANSFERRED_TOPIC,
                ZoneFactory::OwnershipTransferred::SIGNATURE_HASH,
            ),
            (
                inbox::TEMPO_ADVANCED_TOPIC,
                IZoneInbox::TempoAdvanced::SIGNATURE_HASH,
            ),
            (
                inbox::DEPOSIT_PROCESSED_TOPIC,
                IZoneInbox::DepositProcessed::SIGNATURE_HASH,
            ),
            (
                inbox::DEPOSIT_FAILED_TOPIC,
                IZoneInbox::DepositFailed::SIGNATURE_HASH,
            ),
            (
                inbox::WITHDRAWAL_BOUNCE_BACK_PROCESSED_TOPIC,
                IZoneInbox::WithdrawalBounceBackProcessed::SIGNATURE_HASH,
            ),
            (
                inbox::WITHDRAWAL_BOUNCE_BACK_PENDING_TOPIC,
                IZoneInbox::WithdrawalBounceBackPending::SIGNATURE_HASH,
            ),
            (
                inbox::REFUND_CLAIMED_TOPIC,
                IZoneInbox::RefundClaimed::SIGNATURE_HASH,
            ),
            (
                inbox::TOKEN_ENABLED_TOPIC,
                IZoneInbox::TokenEnabled::SIGNATURE_HASH,
            ),
            (
                inbox::DEPOSIT_REJECTED_TOPIC,
                IZoneInbox::DepositRejected::SIGNATURE_HASH,
            ),
            (
                outbox::WITHDRAWAL_REQUESTED_TOPIC,
                IZoneOutbox::WithdrawalRequested::SIGNATURE_HASH,
            ),
            (
                outbox::BATCH_FINALIZED_TOPIC,
                IZoneOutbox::BatchFinalized::SIGNATURE_HASH,
            ),
            (
                outbox::TEMPO_GAS_RATE_UPDATED_TOPIC,
                IZoneOutbox::TempoGasRateUpdated::SIGNATURE_HASH,
            ),
            (
                outbox::MAX_WITHDRAWALS_PER_BLOCK_UPDATED_TOPIC,
                IZoneOutbox::MaxWithdrawalsPerBlockUpdated::SIGNATURE_HASH,
            ),
            (
                tempo_state::BLOCK_FINALIZED_TOPIC,
                TempoState::TempoBlockFinalized::SIGNATURE_HASH,
            ),
        ];

        for (literal, generated) in topics {
            assert_eq!(literal, generated);
        }
    }
}
