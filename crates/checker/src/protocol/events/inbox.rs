use alloy_primitives::{B256, Log, b256};

use super::{
    ProtocolEventError,
    common::{required_topic, strict_decode_interface, unsupported, validate_token_metadata},
};

// Checker-owned native ZoneInbox event ABI.
//
// The seven supported events are pinned to
// `crates/contracts/src/precompiles/zone_inbox.rs:57-90`. `DepositRejected`
// is defined only to pin its excluded topic from the stale Solidity reference
// (`specs/ref-impls/src/interfaces/IZone.sol:1089-1096`); it is never decoded
// into a supported outcome.
alloy_sol_types::sol! {
    #[derive(Debug, PartialEq, Eq)]
    enum DepositType {
        WithdrawalBounceBack,
        Deposit
    }

    #[derive(Debug, PartialEq, Eq)]
    event DepositRejected(
        bytes32 indexed depositHash,
        address indexed sender,
        DepositType depositType,
        address token,
        uint128 amount,
        address tempoRefundRecipient
    );

    #[derive(Debug, PartialEq, Eq)]
    contract Inbox {
        event TempoAdvanced(
            bytes32 indexed tempoBlockHash,
            uint64 indexed tempoBlockNumber,
            uint256 depositsProcessed,
            bytes32 newProcessedDepositQueueHash,
            uint64 lastProcessedDepositNumber
        );
        event DepositProcessed(
            bytes32 indexed depositHash,
            address indexed sender,
            address indexed to,
            address token,
            uint128 amount,
            bytes32 memo
        );
        event DepositFailed(
            bytes32 indexed depositHash,
            address indexed sender,
            address token,
            uint128 amount
        );
        event WithdrawalBounceBackProcessed(
            address indexed zoneFallbackRecipient,
            address token,
            uint128 amount
        );
        event WithdrawalBounceBackPending(
            address indexed zoneFallbackRecipient,
            address token,
            uint128 amount
        );
        event RefundClaimed(address indexed recipient, address indexed token, uint128 amount);
        event TokenEnabled(address indexed token, string name, string symbol, string currency);
    }
}

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

pub(super) fn decode(log: &Log) -> Result<Inbox::InboxEvents, ProtocolEventError> {
    match required_topic(log)? {
        TEMPO_ADVANCED_TOPIC
        | DEPOSIT_PROCESSED_TOPIC
        | DEPOSIT_FAILED_TOPIC
        | WITHDRAWAL_BOUNCE_BACK_PROCESSED_TOPIC
        | WITHDRAWAL_BOUNCE_BACK_PENDING_TOPIC
        | REFUND_CLAIMED_TOPIC
        | TOKEN_ENABLED_TOPIC => {}
        // The written spec and stale Solidity Inbox retain this event, but the
        // pinned native `advanceTempo` ABI has no `rejected` flag. Never decode
        // this topic into the failed-deposit branch.
        DEPOSIT_REJECTED_TOPIC => return Err(unsupported(log)),
        _ => return Err(unsupported(log)),
    }

    let decoded = strict_decode_interface::<Inbox::InboxEvents>(log, "ZoneInbox event")?;
    if let Inbox::InboxEvents::TokenEnabled(event) = &decoded {
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
