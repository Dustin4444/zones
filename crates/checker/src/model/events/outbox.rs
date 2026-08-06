// Generated constructors inherit the immutable protocol event arity.
#![allow(clippy::too_many_arguments)]

use alloy_primitives::{B256, Log, b256};

use crate::model::constants::{COMPRESSED_PUBLIC_KEY_SIZE, MAX_CALLBACK_DATA_SIZE};

use super::{
    ProtocolEventError,
    common::{malformed, required_topic, strict_decode_interface, unsupported, validate_max_bytes},
};

// Checker-owned native ZoneOutbox event ABI.
//
// Pinned source: `crates/contracts/src/precompiles/outbox.rs:29-47`.
alloy_sol_types::sol! {
    #[derive(Debug, PartialEq, Eq)]
    contract Outbox {
        event WithdrawalRequested(
            uint64 indexed withdrawalIndex,
            address indexed sender,
            address token,
            address to,
            uint128 amount,
            uint128 fee,
            bytes32 memo,
            uint64 gasLimit,
            uint64 fallbackNonce,
            bytes data,
            bytes revealTo
        );
        event BatchFinalized(
            bytes32 indexed withdrawalQueueHash,
            uint64 withdrawalBatchIndex
        );
        event TempoGasRateUpdated(uint128 tempoGasRate);
        event MaxWithdrawalsPerBlockUpdated(uint32 maxWithdrawalsPerBlock);
    }
}

pub(super) const WITHDRAWAL_REQUESTED_TOPIC: B256 =
    b256!("34ca953f3eed14157d2f660c7e92a5bd9c05be0d61a188830f3bf1cb7d094f96");
pub(super) const BATCH_FINALIZED_TOPIC: B256 =
    b256!("ec4aff46c65f485f4b15e3c2edadda1d57d002995f5aa262a27c76b9a680ec16");
pub(super) const TEMPO_GAS_RATE_UPDATED_TOPIC: B256 =
    b256!("6f864cce5237e12ffc9a99fc6c59af17222c2bbb3457690cc8753ab16b5d715e");
pub(super) const MAX_WITHDRAWALS_PER_BLOCK_UPDATED_TOPIC: B256 =
    b256!("5340f6cf6e1274bffd0c7188c75f885ed6c90cd6b0879a646290a92f84a6dce3");

pub(super) fn decode(log: &Log) -> Result<Outbox::OutboxEvents, ProtocolEventError> {
    match required_topic(log)? {
        WITHDRAWAL_REQUESTED_TOPIC
        | BATCH_FINALIZED_TOPIC
        | TEMPO_GAS_RATE_UPDATED_TOPIC
        | MAX_WITHDRAWALS_PER_BLOCK_UPDATED_TOPIC => {}
        _ => return Err(unsupported(log)),
    }

    let decoded = strict_decode_interface::<Outbox::OutboxEvents>(log, "ZoneOutbox event")?;
    if let Outbox::OutboxEvents::WithdrawalRequested(event) = &decoded {
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
