// Generated constructors inherit the immutable protocol event arity.
#![allow(clippy::too_many_arguments)]

use alloy_primitives::{B256, Log, b256};

use crate::model::constants::{ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE, MAX_SEQUENCERS};

use super::{
    ProtocolEventError,
    common::{
        preflight_address_array_count, required_topic, strict_decode_interface, unsupported,
        validate_exact_bytes, validate_token_metadata,
    },
};

// Checker-owned Portal event ABI.
//
// Pinned source: `specs/ref-impls/src/interfaces/IZone.sol:504-605,652`.
// The production Rust ABI mirror currently omits `DepositsPaused`,
// `DepositsResumed`, and `RpcUrlUpdated`; keeping these definitions here is
// intentional.
alloy_sol_types::sol! {
    #[derive(Debug, PartialEq, Eq)]
    contract Portal {
        enum Role {
            None,
            Account,
            CallbackGateway
        }

        event DepositMade(
            bytes32 indexed newCurrentDepositQueueHash,
            address indexed sender,
            address token,
            uint128 netAmount,
            uint128 fee,
            uint256 keyIndex,
            bytes32 ephemeralPubkeyX,
            uint8 ephemeralPubkeyYParity,
            bytes ciphertext,
            bytes12 nonce,
            bytes16 tag,
            address tempoRefundRecipient,
            uint64 depositNumber
        );
        event TokenEnabled(address indexed token, string name, string symbol, string currency);
        event BatchSubmitted(
            uint64 indexed withdrawalBatchIndex,
            uint256 indexed withdrawalQueueIndex,
            bytes32 nextProcessedDepositQueueHash,
            bytes32 nextBlockHash,
            bytes32 withdrawalQueueHash,
            uint64 lastProcessedDepositNumber
        );
        event WithdrawalProcessed(
            address indexed to,
            bytes32 indexed senderTag,
            address token,
            uint128 amount,
            bool callbackSuccess
        );
        event WithdrawalBounceBack(
            bytes32 indexed newCurrentDepositQueueHash,
            uint64 indexed fallbackNonce,
            address token,
            uint128 amount,
            uint64 depositNumber
        );
        event DepositBounceBack(
            address indexed tempoRefundRecipient,
            address token,
            uint128 amount,
            uint128 bouncebackFee
        );
        event DepositBounceBackPending(
            address indexed tempoRefundRecipient,
            address token,
            uint128 amount,
            uint128 bouncebackFee
        );
        event RefundClaimed(address indexed recipient, address indexed token, uint128 amount);
        event BouncebackGasUpdated(uint64 bouncebackGas);

        event SequencerEncryptionKeyUpdated(
            bytes32 x,
            uint8 yParity,
            uint256 keyIndex,
            uint64 activationBlock
        );
        event ZoneGasRateUpdated(uint128 zoneGasRate);
        event MaxTempoGasRateUpdated(uint128 maxTempoGasRate);
        event AdminTransferStarted(
            address indexed currentAdmin,
            address indexed pendingAdmin
        );
        event AdminTransferred(
            address indexed previousAdmin,
            address indexed newAdmin
        );
        event RoleUpdated(address indexed account, Role prev, Role next);
        event EnforcementModesUpdated(bool accessMode, bool gatewayMode);
        event SequencerSetUpdated(
            uint64 indexed nonce,
            uint8 threshold,
            address[] sequencers
        );
        event LeaderUpdated(
            address indexed previousLeader,
            address indexed newLeader,
            uint64 indexed epoch,
            uint64 activationTempoBlock
        );
        event DepositsPaused(address indexed token);
        event DepositsResumed(address indexed token);
        event RpcUrlUpdated(string rpcUrl);
    }
}

// Independent topic0 literals. These are intentionally not defined through
// `SolEvent::SIGNATURE_HASH`; tests compare the two authorities.
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

/// Decoded Portal payloads allowed to drive or check the release-one model.
///
/// Known non-model variants never enter this enum, so downstream transition
/// code does not need unreachable match arms for them.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PortalModelEvent {
    DepositMade(Portal::DepositMade),
    TokenEnabled(Portal::TokenEnabled),
    BatchSubmitted(Portal::BatchSubmitted),
    WithdrawalProcessed(Portal::WithdrawalProcessed),
    WithdrawalBounceBack(Portal::WithdrawalBounceBack),
    DepositBounceBack(Portal::DepositBounceBack),
    DepositBounceBackPending(Portal::DepositBounceBackPending),
    RefundClaimed(Portal::RefundClaimed),
    BouncebackGasUpdated(Portal::BouncebackGasUpdated),
}

pub(super) fn decode(log: &Log) -> Result<Option<PortalModelEvent>, ProtocolEventError> {
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

    let decoded = strict_decode_interface::<Portal::PortalEvents>(log, "Portal event")?;
    validate_dynamic_bounds(log, &decoded)?;

    Ok(match decoded {
        Portal::PortalEvents::DepositMade(event) => Some(PortalModelEvent::DepositMade(event)),
        Portal::PortalEvents::TokenEnabled(event) => Some(PortalModelEvent::TokenEnabled(event)),
        Portal::PortalEvents::BatchSubmitted(event) => {
            Some(PortalModelEvent::BatchSubmitted(event))
        }
        Portal::PortalEvents::WithdrawalProcessed(event) => {
            Some(PortalModelEvent::WithdrawalProcessed(event))
        }
        Portal::PortalEvents::WithdrawalBounceBack(event) => {
            Some(PortalModelEvent::WithdrawalBounceBack(event))
        }
        Portal::PortalEvents::DepositBounceBack(event) => {
            Some(PortalModelEvent::DepositBounceBack(event))
        }
        Portal::PortalEvents::DepositBounceBackPending(event) => {
            Some(PortalModelEvent::DepositBounceBackPending(event))
        }
        Portal::PortalEvents::RefundClaimed(event) => Some(PortalModelEvent::RefundClaimed(event)),
        Portal::PortalEvents::BouncebackGasUpdated(event) => {
            Some(PortalModelEvent::BouncebackGasUpdated(event))
        }
        Portal::PortalEvents::SequencerEncryptionKeyUpdated(_)
        | Portal::PortalEvents::ZoneGasRateUpdated(_)
        | Portal::PortalEvents::MaxTempoGasRateUpdated(_)
        | Portal::PortalEvents::AdminTransferStarted(_)
        | Portal::PortalEvents::AdminTransferred(_)
        | Portal::PortalEvents::RoleUpdated(_)
        | Portal::PortalEvents::EnforcementModesUpdated(_)
        | Portal::PortalEvents::SequencerSetUpdated(_)
        | Portal::PortalEvents::LeaderUpdated(_)
        | Portal::PortalEvents::DepositsPaused(_)
        | Portal::PortalEvents::DepositsResumed(_)
        | Portal::PortalEvents::RpcUrlUpdated(_) => None,
    })
}

fn validate_dynamic_bounds(
    log: &Log,
    event: &Portal::PortalEvents,
) -> Result<(), ProtocolEventError> {
    match event {
        Portal::PortalEvents::DepositMade(event) => validate_exact_bytes(
            log,
            "DepositMade",
            "ciphertext",
            event.ciphertext.len(),
            ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE,
        ),
        Portal::PortalEvents::TokenEnabled(event) => validate_token_metadata(
            log,
            "TokenEnabled",
            &event.name,
            &event.symbol,
            &event.currency,
        ),
        Portal::PortalEvents::SequencerSetUpdated(event)
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
