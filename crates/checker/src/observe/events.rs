//! Strict protocol event classification owned by the checker.
//!
//! L1 and L2 classifiers match a literal `(emitter, topic0)` pair before using
//! the shared ABI decoder. Unknown topics from protocol emitters fail
//! closed.

use alloy_primitives::{Address, B256, Log};

use tempo_zone_contracts::{
    TEMPO_STATE_ADDRESS, ZONE_FACTORY_ADDRESS, ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS,
};

mod common {
    use std::mem::size_of;

    use alloy_primitives::{B256, IntoLogData, Log};
    use alloy_sol_types::SolEventInterface;

    use super::ProtocolEventError;
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
}
mod factory {
    use alloy_primitives::{B256, Log, b256};
    use tempo_zone_contracts::{MAX_SEQUENCERS, ZoneFactory as Factory};

    use super::{
        ProtocolEventError,
        common::{
            malformed, preflight_address_array_count, required_topic, strict_decode_interface,
            unsupported,
        },
    };

    pub(super) const ZONE_CREATED_TOPIC: B256 =
        b256!("4f2c5b8ee43ce856328b02e8d2b193126eb1c13f34475bd902ecb9a1eaa826a4");
    pub(super) const OWNERSHIP_TRANSFERRED_TOPIC: B256 =
        b256!("8be0079c531659141344cd1fd0a4f28419497f9722a3daafe3b4186f6b6457e0");

    pub(super) fn decode(log: &Log) -> Result<Option<Factory::ZoneCreated>, ProtocolEventError> {
        let topic = required_topic(log)?;
        match topic {
            ZONE_CREATED_TOPIC => {
                preflight_address_array_count(log, "ZoneCreated", 4, MAX_SEQUENCERS)?
            }
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
}
mod inbox {
    use alloy_primitives::{B256, Log, b256};
    use tempo_zone_contracts::IZoneInbox as Inbox;

    use super::{
        ProtocolEventError,
        common::{required_topic, strict_decode_interface, unsupported, validate_token_metadata},
    };

    pub(super) const TEMPO_ADVANCED_TOPIC: B256 =
        b256!("d2d2bf1e295f62cd08f0f0ab45818efeaba78b58310526f7b7e9686b8aeded1a");
    pub(super) const DEPOSIT_PROCESSED_TOPIC: B256 =
        b256!("d5277bc9597c7da3fab9cdbba4de6005f48b9eb7389cf2389c4ea9eea3172c21");
    pub(super) const DEPOSIT_FAILED_TOPIC: B256 =
        b256!("1fa7f545d9203bbedf7510083fa6f3ff73ddd3b3c99a7971ee5f91ef89e53c5b");
    pub(super) const WITHDRAWAL_BOUNCE_BACK_PROCESSED_TOPIC: B256 =
        b256!("bc1cf3ff6e619587619408b396a13775509216474757afb9d29bda1a96f590f0");
    pub(super) const WITHDRAWAL_BOUNCE_BACK_PENDING_TOPIC: B256 =
        b256!("92d91a50ae3561d4edd8eadad5f27f09116faaa4f733bd37682c49b4cbc20e39");
    pub(super) const REFUND_CLAIMED_TOPIC: B256 =
        b256!("ffd3bbab073ab4b2d0792c270104924c14c285a153b9acddabae166395d2eb5c");
    pub(super) const TOKEN_ENABLED_TOPIC: B256 =
        b256!("4ac4dcc08b0c26c3fb6b58c64c1392b7934b1ce6b0382a5986ea5c3de795e053");
    pub(super) const DEPOSIT_REJECTED_TOPIC: B256 =
        b256!("4620415fad9c416306a56ca0ee640b3418628a5f2e45ddde3ddf7452a7a654fb");

    pub(super) fn decode(log: &Log) -> Result<Inbox::IZoneInboxEvents, ProtocolEventError> {
        match required_topic(log)? {
            TEMPO_ADVANCED_TOPIC
            | DEPOSIT_PROCESSED_TOPIC
            | DEPOSIT_FAILED_TOPIC
            | WITHDRAWAL_BOUNCE_BACK_PROCESSED_TOPIC
            | WITHDRAWAL_BOUNCE_BACK_PENDING_TOPIC
            | REFUND_CLAIMED_TOPIC
            | TOKEN_ENABLED_TOPIC => {}
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
}
mod outbox {
    use alloy_primitives::{B256, Log, b256};
    use tempo_zone_contracts::IZoneOutbox as Outbox;
    use zone_precompiles::{ecies::COMPRESSED_PUBLIC_KEY_SIZE, outbox::MAX_CALLBACK_DATA_SIZE};

    use super::{
        ProtocolEventError,
        common::{
            malformed, required_topic, strict_decode_interface, unsupported, validate_max_bytes,
        },
    };

    pub(super) const WITHDRAWAL_REQUESTED_TOPIC: B256 =
        b256!("34ca953f3eed14157d2f660c7e92a5bd9c05be0d61a188830f3bf1cb7d094f96");
    pub(super) const BATCH_FINALIZED_TOPIC: B256 =
        b256!("ec4aff46c65f485f4b15e3c2edadda1d57d002995f5aa262a27c76b9a680ec16");
    pub(super) const TEMPO_GAS_RATE_UPDATED_TOPIC: B256 =
        b256!("6f864cce5237e12ffc9a99fc6c59af17222c2bbb3457690cc8753ab16b5d715e");
    pub(super) const MAX_WITHDRAWALS_PER_BLOCK_UPDATED_TOPIC: B256 =
        b256!("5340f6cf6e1274bffd0c7188c75f885ed6c90cd6b0879a646290a92f84a6dce3");

    pub(super) fn decode(log: &Log) -> Result<Outbox::IZoneOutboxEvents, ProtocolEventError> {
        match required_topic(log)? {
            WITHDRAWAL_REQUESTED_TOPIC
            | BATCH_FINALIZED_TOPIC
            | TEMPO_GAS_RATE_UPDATED_TOPIC
            | MAX_WITHDRAWALS_PER_BLOCK_UPDATED_TOPIC => {}
            _ => return Err(unsupported(log)),
        }
        let decoded =
            strict_decode_interface::<Outbox::IZoneOutboxEvents>(log, "ZoneOutbox event")?;
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
}
#[allow(clippy::too_many_arguments)]
mod portal {
    use alloy_primitives::{B256, Log, b256};
    use tempo_zone_contracts::{MAX_SEQUENCERS, ZonePortal as Portal};
    use zone_precompiles::ecies::ENCRYPTED_PAYLOAD_PLAINTEXT_SIZE;

    use super::{
        ProtocolEventError,
        common::{
            preflight_address_array_count, required_topic, strict_decode_interface, unsupported,
            validate_exact_bytes, validate_token_metadata,
        },
    };

    // Independent topic0 literals. Tests compare them with `SolEvent::SIGNATURE_HASH`.
    pub(super) const DEPOSIT_MADE_TOPIC: B256 =
        b256!("51046223e5e0abca942f13a8f3d1c8dfd59c8b6c4f3e64fc2f5bf453767a97ca");
    pub(super) const TOKEN_ENABLED_TOPIC: B256 =
        b256!("4ac4dcc08b0c26c3fb6b58c64c1392b7934b1ce6b0382a5986ea5c3de795e053");
    pub(super) const BATCH_SUBMITTED_TOPIC: B256 =
        b256!("5a66941dc92cb865480c966eff640c02b1d00d544b74332fd67c6f1cbfccdf39");
    pub(super) const WITHDRAWAL_PROCESSED_TOPIC: B256 =
        b256!("65042ea6dad60c26f055e80ec401b3437c854ed586a0704d305bb4e9ea4518cf");
    pub(super) const WITHDRAWAL_BOUNCE_BACK_TOPIC: B256 =
        b256!("adf6f2901dd7af2f28a594f47a925894a08d4de10609dff591a80642648775c5");
    pub(super) const DEPOSIT_BOUNCE_BACK_TOPIC: B256 =
        b256!("0f7ef08806234f85aaee43d3ba4589c3bc6d5ac3fc8edd56fc3d91cc7553bdcb");
    pub(super) const DEPOSIT_BOUNCE_BACK_PENDING_TOPIC: B256 =
        b256!("5fea28d0adb7d877ae3259768f41ad6741aa1784c4475746dd931364f62e68a1");
    pub(super) const REFUND_CLAIMED_TOPIC: B256 =
        b256!("ffd3bbab073ab4b2d0792c270104924c14c285a153b9acddabae166395d2eb5c");
    pub(super) const BOUNCEBACK_GAS_UPDATED_TOPIC: B256 =
        b256!("66bcd750662bb66118e25a8e421ae73974634d9af2d44fb9e600d250917fe690");
    pub(super) const SEQUENCER_ENCRYPTION_KEY_UPDATED_TOPIC: B256 =
        b256!("82b5f4090f18a082bc8156b956154bfe0319307f5e5a7e903ef33f14ad2cb17e");
    pub(super) const ZONE_GAS_RATE_UPDATED_TOPIC: B256 =
        b256!("c62141e607d6fcbf7d11fd2b6d8e18e5ebef6d3fff8136ca98822801abbaea38");
    pub(super) const MAX_TEMPO_GAS_RATE_UPDATED_TOPIC: B256 =
        b256!("ede0c86e4d0b914b0ba2f68c3359e9ccbcdece694913dcbdf50affe96900e1e8");
    pub(super) const ADMIN_TRANSFER_STARTED_TOPIC: B256 =
        b256!("e5cd1c804f1c9cc6d7009e4c0fb532f0e2d8863524c3323a6b3790c3f80bf25c");
    pub(super) const ADMIN_TRANSFERRED_TOPIC: B256 =
        b256!("f8ccb027dfcd135e000e9d45e6cc2d662578a8825d4c45b5e32e0adf67e79ec6");
    pub(super) const ROLE_UPDATED_TOPIC: B256 =
        b256!("2359a069f5d7871f8f60ad861112ebe12dcf2ba55225c32ec04564d494afc69b");
    pub(super) const ENFORCEMENT_MODES_UPDATED_TOPIC: B256 =
        b256!("3e5479494e0a078954a7ff8437aeca3bf7519b51a2fc06b3821251147ff9c5f7");
    pub(super) const SEQUENCER_SET_UPDATED_TOPIC: B256 =
        b256!("9282e5956b9751944c6e527bb3fa37aed57d3cfb67979c8962f561a194fc0bc5");
    pub(super) const LEADER_UPDATED_TOPIC: B256 =
        b256!("0e49bd8bbce34618e6af3bb74d587a65fa2a594df80b7cc21d690ee78c6d7a69");
    pub(super) const DEPOSITS_PAUSED_TOPIC: B256 =
        b256!("eb225a736fbfee3f85ccb72bdf84ff0396ab358b7970e2cc351ab3e3fd92358d");
    pub(super) const DEPOSITS_RESUMED_TOPIC: B256 =
        b256!("22ab73af03f04a21e91c7923327f99279b7f5d07d9551762c39bccdf051f1fe9");
    pub(super) const RPC_URL_UPDATED_TOPIC: B256 =
        b256!("f4e00967b25e707df96d88676243b33be84847ef27615af8ef91290b52294fc6");

    pub(super) fn decode(
        log: &Log,
    ) -> Result<Option<Portal::ZonePortalEvents>, ProtocolEventError> {
        let topic = required_topic(log)?;
        match topic {
            DEPOSIT_MADE_TOPIC
            | TOKEN_ENABLED_TOPIC
            | BATCH_SUBMITTED_TOPIC
            | WITHDRAWAL_PROCESSED_TOPIC
            | WITHDRAWAL_BOUNCE_BACK_TOPIC
            | DEPOSIT_BOUNCE_BACK_TOPIC
            | DEPOSIT_BOUNCE_BACK_PENDING_TOPIC
            | REFUND_CLAIMED_TOPIC
            | BOUNCEBACK_GAS_UPDATED_TOPIC
            | SEQUENCER_ENCRYPTION_KEY_UPDATED_TOPIC
            | ZONE_GAS_RATE_UPDATED_TOPIC
            | MAX_TEMPO_GAS_RATE_UPDATED_TOPIC
            | ADMIN_TRANSFER_STARTED_TOPIC
            | ADMIN_TRANSFERRED_TOPIC
            | ROLE_UPDATED_TOPIC
            | ENFORCEMENT_MODES_UPDATED_TOPIC
            | SEQUENCER_SET_UPDATED_TOPIC
            | LEADER_UPDATED_TOPIC
            | DEPOSITS_PAUSED_TOPIC
            | DEPOSITS_RESUMED_TOPIC
            | RPC_URL_UPDATED_TOPIC => {}
            _ => return Err(unsupported(log)),
        }

        // `threshold` is the first body word and the address-array offset the
        // second. Guard its count before Alloy allocates the generated Vec.
        if topic == SEQUENCER_SET_UPDATED_TOPIC {
            preflight_address_array_count(log, "SequencerSetUpdated", 1, MAX_SEQUENCERS)?;
        }

        let decoded = strict_decode_interface::<Portal::ZonePortalEvents>(log, "Portal event")?;
        validate_dynamic_bounds(log, &decoded)?;

        let changes_checker_state = matches!(
            decoded,
            Portal::ZonePortalEvents::DepositMade(_)
                | Portal::ZonePortalEvents::TokenEnabled(_)
                | Portal::ZonePortalEvents::BatchSubmitted(_)
                | Portal::ZonePortalEvents::WithdrawalProcessed(_)
                | Portal::ZonePortalEvents::WithdrawalBounceBack(_)
                | Portal::ZonePortalEvents::DepositBounceBack(_)
                | Portal::ZonePortalEvents::DepositBounceBackPending(_)
                | Portal::ZonePortalEvents::RefundClaimed(_)
                | Portal::ZonePortalEvents::BouncebackGasUpdated(_)
        );
        Ok(changes_checker_state.then_some(decoded))
    }

    fn validate_dynamic_bounds(
        log: &Log,
        event: &Portal::ZonePortalEvents,
    ) -> Result<(), ProtocolEventError> {
        match event {
            Portal::ZonePortalEvents::DepositMade(event) => validate_exact_bytes(
                log,
                "DepositMade",
                "ciphertext",
                event.ciphertext.len(),
                ENCRYPTED_PAYLOAD_PLAINTEXT_SIZE,
            ),
            Portal::ZonePortalEvents::TokenEnabled(event) => validate_token_metadata(
                log,
                "TokenEnabled",
                &event.name,
                &event.symbol,
                &event.currency,
            ),
            Portal::ZonePortalEvents::SequencerSetUpdated(event)
                if event.sequencers.len() > MAX_SEQUENCERS =>
            {
                Err(super::common::malformed(
                    log,
                    "SequencerSetUpdated",
                    format!(
                        "address array length {} exceeds {MAX_SEQUENCERS}",
                        event.sequencers.len()
                    ),
                ))
            }
            _ => Ok(()),
        }
    }
}
mod tempo_state {
    use alloy_primitives::{B256, Log, b256};
    use tempo_zone_contracts::TempoState;

    use super::{
        ProtocolEventError,
        common::{required_topic, strict_decode_interface, unsupported},
    };

    pub(super) const BLOCK_FINALIZED_TOPIC: B256 =
        b256!("dd85219569c3c880f014955916f426d1ca039714b59ce33e24f151f155ac26b9");

    pub(super) fn decode(log: &Log) -> Result<TempoState::TempoStateEvents, ProtocolEventError> {
        if required_topic(log)? != BLOCK_FINALIZED_TOPIC {
            return Err(unsupported(log));
        }
        strict_decode_interface(log, "TempoState event")
    }
}

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
