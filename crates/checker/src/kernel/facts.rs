use alloy_primitives::{Address, B256, Bytes, FixedBytes, U256};
use serde::{Deserialize, Serialize};

use crate::kernel::state::PortalIdentity;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TokenEnable {
    pub(crate) token: Address,
    pub(crate) name: String,
    pub(crate) symbol: String,
    pub(crate) currency: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DepositPayload {
    pub(crate) ephemeral_pubkey_x: B256,
    pub(crate) ephemeral_pubkey_y_parity: u8,
    pub(crate) ciphertext: FixedBytes<64>,
    pub(crate) nonce: FixedBytes<12>,
    pub(crate) tag: FixedBytes<16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OrdinaryDeposit {
    pub(crate) token: Address,
    pub(crate) sender: Address,
    pub(crate) amount: u128,
    pub(crate) tempo_refund_recipient: Address,
    pub(crate) key_index: U256,
    pub(crate) encrypted: DepositPayload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BounceBackDeposit {
    pub(crate) token: Address,
    pub(crate) fallback_nonce: std::num::NonZeroU64,
    pub(crate) amount: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Deposit {
    Ordinary(OrdinaryDeposit),
    BounceBack(BounceBackDeposit),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ImportedOperation {
    Create {
        identity: PortalIdentity,
        initial_token: TokenEnable,
    },
    UpdateBouncebackGas(u64),
    EnableToken(TokenEnable),
    AppendDeposit(OrdinaryDeposit),
    SubmitBatch(BatchSubmission),
    ProcessWithdrawals(WithdrawalProcessing),
    ClaimPortalRefund(RefundClaim),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ImportedFacts {
    pub(crate) block_hash: B256,
    pub(crate) block_number: u64,
    pub(crate) operations: Vec<ImportedOperation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RefundClaim {
    pub(crate) token: Address,
    pub(crate) recipient: Address,
    pub(crate) amount: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BatchSubmission {
    pub(crate) tempo_block: u64,
    pub(crate) previous_block: B256,
    pub(crate) next_block: B256,
    pub(crate) previous_deposit: crate::kernel::state::Cursor,
    pub(crate) next_deposit: crate::kernel::state::Cursor,
    pub(crate) withdrawal_queue_hash: B256,
    pub(crate) next_zone_height: U256,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WithdrawalProcessing {
    pub(crate) base_fee: U256,
    pub(crate) withdrawals: Vec<crate::kernel::state::Withdrawal>,
    pub(crate) remaining_queue: B256,
    pub(crate) outcomes: Vec<WithdrawalOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum WithdrawalOutcome {
    UserDelivered {
        callback_deposits: Vec<OrdinaryDeposit>,
    },
    UserBounced,
    FailedDepositPaid {
        collected_fee: u128,
    },
    FailedDepositPending {
        collected_fee: u128,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum DepositOutcome {
    Minted,
    Failed,
    BounceBackMinted { recipient: Address },
    BounceBackPending { recipient: Address },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UserWithdrawal {
    pub(crate) sender: Address,
    pub(crate) transaction_hash: B256,
    pub(crate) token: Address,
    pub(crate) to: Address,
    pub(crate) amount: u128,
    pub(crate) memo: B256,
    pub(crate) gas_limit: u64,
    pub(crate) callback_data: Bytes,
    pub(crate) reveal_to: Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ZoneOperation {
    UpdateTempoGasRate(u128),
    UpdateMaxWithdrawals(u32),
    AcceptWithdrawal(UserWithdrawal),
    ClaimInboxRefund(RefundClaim),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Finalization {
    pub(crate) block_number: u64,
    pub(crate) declared_count: usize,
    pub(crate) encrypted_senders: Vec<Bytes>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ZoneFacts {
    pub(crate) block_hash: B256,
    pub(crate) block_number: u64,
    pub(crate) enabled_tokens: Vec<TokenEnable>,
    pub(crate) deposits: Vec<Deposit>,
    pub(crate) outcomes: Vec<DepositOutcome>,
    pub(crate) operations: Vec<ZoneOperation>,
    pub(crate) finalization: Option<Finalization>,
}
