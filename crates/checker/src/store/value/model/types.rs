use alloy_primitives::{Address, B256, U256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorValue {
    pub(crate) hash: B256,
    pub(crate) number: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortalSettlementValue {
    pub(crate) withdrawal_batch_index: u64,
    pub(crate) block_hash: B256,
    pub(crate) last_synced_tempo_block_number: u64,
    pub(crate) last_submitted_deposit_cursor: CursorValue,
    pub(crate) zone_height: U256,
    pub(crate) withdrawal_queue_head: U256,
    pub(crate) withdrawal_queue_tail: U256,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZoneBatchAccumulatorValue {
    pub(crate) last_withdrawal_queue_hash: B256,
    pub(crate) last_withdrawal_batch_index: u64,
    pub(crate) first_zone_parent_hash: B256,
    pub(crate) first_processed_deposit: CursorValue,
    pub(crate) first_withdrawal_index: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredTokenPhase {
    PendingZoneEnable,
    ZoneEnabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenValue {
    pub(crate) phase: StoredTokenPhase,
    pub(crate) supply: U256,
    pub(crate) deposit_liability: U256,
    pub(crate) withdrawal_liability: U256,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrdinaryDepositValue {
    pub(crate) token: Address,
    pub(crate) sender: Address,
    pub(crate) amount: u128,
    pub(crate) tempo_refund_recipient: Address,
    pub(crate) key_index: U256,
    pub(crate) ephemeral_pubkey_x: B256,
    pub(crate) ephemeral_pubkey_y_parity: u8,
    pub(crate) ciphertext: Vec<u8>,
    pub(crate) nonce: [u8; 12],
    pub(crate) tag: [u8; 16],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BounceBackDepositValue {
    pub(crate) token: Address,
    pub(crate) fallback_nonce: u64,
    pub(crate) amount: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingDepositValue {
    Ordinary(OrdinaryDepositValue),
    WithdrawalBounceBack {
        withdrawal_zone_id: u32,
        withdrawal_index: u64,
        preimage: BounceBackDepositValue,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserWithdrawalIdentityValue {
    pub(crate) sender: Address,
    pub(crate) transaction_hash: B256,
    pub(crate) fallback_nonce: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserWithdrawalRequestValue {
    pub(crate) token: Address,
    pub(crate) recipient: Address,
    pub(crate) amount: u128,
    pub(crate) memo: B256,
    pub(crate) gas_limit: u64,
    pub(crate) callback_data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredSenderReveal {
    None,
    Encrypted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingWithdrawalValue {
    User {
        identity: UserWithdrawalIdentityValue,
        request: UserWithdrawalRequestValue,
        sender_reveal: StoredSenderReveal,
    },
    FailedDeposit {
        deposit_portal: Address,
        deposit_number: u64,
        token: Address,
        recipient: Address,
        amount: u128,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WithdrawalValue {
    Pending(PendingWithdrawalValue),
    FinalizedUser {
        identity: UserWithdrawalIdentityValue,
        request: UserWithdrawalRequestValue,
        encrypted_sender: Vec<u8>,
    },
    FinalizedFailedDeposit {
        deposit_portal: Address,
        deposit_number: u64,
        token: Address,
        recipient: Address,
        amount: u128,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackOwnerValue {
    Held {
        withdrawal_zone_id: u32,
        withdrawal_index: u64,
        token: Address,
        amount: u128,
    },
    BounceBackQueued {
        withdrawal_zone_id: u32,
        withdrawal_index: u64,
        token: Address,
        amount: u128,
        deposit_portal: Address,
        deposit_number: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchBoundaryValue {
    pub(crate) first_zone_parent_hash: B256,
    pub(crate) final_zone_block_hash: B256,
    pub(crate) first_processed_deposit: CursorValue,
    pub(crate) final_processed_deposit: CursorValue,
    pub(crate) final_imported_tempo_block_number: u64,
    pub(crate) final_zone_height: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchMembersValue {
    pub(crate) first_withdrawal_index: u64,
    pub(crate) member_count: u64,
    pub(crate) withdrawal_queue_hash: B256,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalizedBatchValue {
    pub(crate) boundary: BatchBoundaryValue,
    pub(crate) members: BatchMembersValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchValue {
    Finalized(FinalizedBatchValue),
    Submitted {
        batch: FinalizedBatchValue,
        portal: Address,
        logical_queue_index: U256,
        next_processing_ordinal: u64,
        remaining_queue_hash: B256,
    },
}

/// The value tag repeats the key family and makes key/value corruption fail closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelValue {
    PortalConfig {
        bounceback_gas: u64,
    },
    ZoneConfig {
        tempo_gas_rate: u128,
        max_withdrawals_per_block: u32,
    },
    PortalDepositCursor(CursorValue),
    ZoneProcessedDepositCursor(CursorValue),
    PortalSettlement(PortalSettlementValue),
    ZoneBatchAccumulator(ZoneBatchAccumulatorValue),
    ZoneNextWithdrawalIndex(u64),
    ZoneLastFallbackNonce(u64),
    Token(TokenValue),
    PendingDeposit(PendingDepositValue),
    Withdrawal(WithdrawalValue),
    FallbackOwner(FallbackOwnerValue),
    Batch(BatchValue),
    PortalRefundCredit(u128),
    InboxRefundCredit(u128),
}
