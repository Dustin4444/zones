use alloy_primitives::{Address, B256, U256};
use serde::{Deserialize, Serialize};

use crate::state::{DepositId, WithdrawalId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExpectedEffect {
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
        withdrawal: WithdrawalId,
        recipient: Address,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedState {
    pub processed_deposit_hash: B256,
    pub processed_deposit_number: u64,
    pub withdrawal_queue_hash: B256,
    pub withdrawal_batch_index: u64,
    pub collateral_requirement: U256,
}
