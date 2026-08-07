use alloy_primitives::{Address, B256, Bytes, FixedBytes, U256};
use serde::{Deserialize, Serialize};

use crate::state::PortalIdentity;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenEnable {
    pub token: Address,
    pub name: String,
    pub symbol: String,
    pub currency: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepositPayload {
    pub ephemeral_pubkey_x: B256,
    pub ephemeral_pubkey_y_parity: u8,
    pub ciphertext: FixedBytes<64>,
    pub nonce: FixedBytes<12>,
    pub tag: FixedBytes<16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrdinaryDeposit {
    pub token: Address,
    pub sender: Address,
    pub amount: u128,
    pub tempo_refund_recipient: Address,
    pub key_index: U256,
    pub encrypted: DepositPayload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BounceBackDeposit {
    pub token: Address,
    pub fallback_nonce: std::num::NonZeroU64,
    pub amount: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Deposit {
    Ordinary(OrdinaryDeposit),
    BounceBack(BounceBackDeposit),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportedOperation {
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
pub struct ImportedFacts {
    pub block_hash: B256,
    pub block_number: u64,
    pub base_fee: U256,
    pub operations: Vec<ImportedOperation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundClaim {
    pub token: Address,
    pub recipient: Address,
    pub amount: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchSubmission {
    pub tempo_block: u64,
    pub previous_block: B256,
    pub next_block: B256,
    pub previous_deposit: crate::state::Cursor,
    pub next_deposit: crate::state::Cursor,
    pub withdrawal_queue_hash: B256,
    pub next_zone_height: U256,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WithdrawalProcessing {
    pub withdrawals: Vec<crate::state::Withdrawal>,
    pub remaining_queue: B256,
    pub outcomes: Vec<WithdrawalOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WithdrawalOutcome {
    UserDelivered {
        callback_deposits: Vec<OrdinaryDeposit>,
    },
    UserBounced,
    FailedDepositPaid,
    FailedDepositPending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DepositOutcome {
    Minted,
    Failed,
    BounceBackMinted { recipient: Address },
    BounceBackPending { recipient: Address },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserWithdrawal {
    pub sender: Address,
    pub transaction_hash: B256,
    pub token: Address,
    pub to: Address,
    pub amount: u128,
    pub memo: B256,
    pub gas_limit: u64,
    pub callback_data: Bytes,
    pub reveal_to: Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ZoneOperation {
    UpdateTempoGasRate(u128),
    UpdateMaxWithdrawals(u32),
    AcceptWithdrawal(UserWithdrawal),
    ClaimInboxRefund(RefundClaim),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finalization {
    pub block_number: u64,
    pub declared_count: usize,
    pub encrypted_senders: Vec<Bytes>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZoneFacts {
    pub block_hash: B256,
    pub block_number: u64,
    pub enabled_tokens: Vec<TokenEnable>,
    pub deposits: Vec<Deposit>,
    pub outcomes: Vec<DepositOutcome>,
    pub operations: Vec<ZoneOperation>,
    pub finalization: Option<Finalization>,
}
