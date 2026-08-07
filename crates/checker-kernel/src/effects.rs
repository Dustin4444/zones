use alloy_primitives::{Address, B256, Bytes, U256};
use serde::{Deserialize, Serialize};

use crate::state::{BatchId, DepositId, WithdrawalId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effect {
    TokenEnabled {
        token: Address,
        name: String,
        symbol: String,
        currency: String,
    },
    DepositAppended {
        id: DepositId,
        queue_hash: B256,
    },
    DepositProcessed {
        deposit_hash: B256,
        sender: Address,
        token: Address,
        amount: u128,
    },
    DepositFailed {
        deposit_hash: B256,
        sender: Address,
        token: Address,
        amount: u128,
    },
    WithdrawalRequested {
        id: WithdrawalId,
        sender: Address,
        token: Address,
        to: Address,
        amount: u128,
        fee: u128,
        memo: B256,
        gas_limit: u64,
        fallback_nonce: u64,
        callback_data: Bytes,
        reveal_to: Bytes,
    },
    BatchFinalized {
        id: BatchId,
        queue_hash: B256,
    },
    BatchSubmitted {
        id: BatchId,
        queue_index: U256,
        processed_deposit_hash: B256,
        final_block_hash: B256,
        queue_hash: B256,
        processed_deposit_number: u64,
    },
    UserWithdrawalProcessed {
        to: Address,
        sender_tag: B256,
        token: Address,
        amount: u128,
        callback_success: bool,
    },
    FailedDepositRefunded {
        recipient: Address,
        token: Address,
        amount: u128,
        fee: u128,
        pending: bool,
    },
    BounceBackAppended {
        fallback_nonce: u64,
        token: Address,
        amount: u128,
        id: DepositId,
        queue_hash: B256,
    },
    BounceBackMinted {
        token: Address,
        amount: u128,
    },
    BounceBackPending {
        token: Address,
        amount: u128,
    },
    RefundClaimed {
        token: Address,
        recipient: Address,
        amount: u128,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedState {
    pub tempo_block_hash: B256,
    pub tempo_block_number: u64,
    pub processed_deposit_hash: B256,
    pub processed_deposit_number: u64,
    pub withdrawal_queue_hash: B256,
    pub withdrawal_batch_index: u64,
}
