//! Disposable normalized outputs from one authenticated Zone block.

use alloy_primitives::{Address, B256, Bytes, U256};

use crate::model::{
    input::ZoneBlockInput,
    transition::{CompletedTransition, ImportedTempoTransition, ModelError},
};

/// Canonical coordinates copied out of the observation boundary for findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ObservedZoneEventPosition {
    pub(super) transaction_index: usize,
    pub(super) receipt_log_index: usize,
    pub(super) block_log_index: usize,
    pub(super) transaction_hash: B256,
}

impl ObservedZoneEventPosition {
    pub(crate) const fn transaction_index(self) -> usize {
        self.transaction_index
    }

    pub(crate) const fn receipt_log_index(self) -> usize {
        self.receipt_log_index
    }

    pub(crate) const fn block_log_index(self) -> usize {
        self.block_log_index
    }

    pub(crate) const fn transaction_hash(self) -> B256 {
        self.transaction_hash
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObservedTempoBlockFinalized {
    pub(super) position: ObservedZoneEventPosition,
    pub(super) block_hash: B256,
    pub(super) block_number: u64,
    pub(super) state_root: B256,
}

impl ObservedTempoBlockFinalized {
    pub(crate) const fn position(self) -> ObservedZoneEventPosition {
        self.position
    }

    pub(crate) const fn block_hash(self) -> B256 {
        self.block_hash
    }

    pub(crate) const fn block_number(self) -> u64 {
        self.block_number
    }

    pub(crate) const fn state_root(self) -> B256 {
        self.state_root
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObservedTempoAdvanced {
    pub(super) position: ObservedZoneEventPosition,
    pub(super) tempo_block_hash: B256,
    pub(super) tempo_block_number: u64,
    pub(super) deposits_processed: U256,
    pub(super) new_processed_deposit_queue_hash: B256,
    pub(super) last_processed_deposit_number: u64,
}

impl ObservedTempoAdvanced {
    pub(crate) const fn position(self) -> ObservedZoneEventPosition {
        self.position
    }

    pub(crate) const fn tempo_block_hash(self) -> B256 {
        self.tempo_block_hash
    }

    pub(crate) const fn tempo_block_number(self) -> u64 {
        self.tempo_block_number
    }

    pub(crate) const fn deposits_processed(self) -> U256 {
        self.deposits_processed
    }

    pub(crate) const fn new_processed_deposit_queue_hash(self) -> B256 {
        self.new_processed_deposit_queue_hash
    }

    pub(crate) const fn last_processed_deposit_number(self) -> u64 {
        self.last_processed_deposit_number
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservedTokenEnabled {
    pub(super) position: ObservedZoneEventPosition,
    pub(super) token: Address,
    pub(super) name: String,
    pub(super) symbol: String,
    pub(super) currency: String,
}

impl ObservedTokenEnabled {
    pub(crate) const fn position(&self) -> ObservedZoneEventPosition {
        self.position
    }

    pub(crate) const fn token(&self) -> Address {
        self.token
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn symbol(&self) -> &str {
        &self.symbol
    }

    pub(crate) fn currency(&self) -> &str {
        &self.currency
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObservedDepositProcessed {
    pub(super) position: ObservedZoneEventPosition,
    pub(super) deposit_hash: B256,
    pub(super) sender: Address,
    pub(super) to: Address,
    pub(super) token: Address,
    pub(super) amount: u128,
    pub(super) memo: B256,
}

impl ObservedDepositProcessed {
    pub(crate) const fn position(self) -> ObservedZoneEventPosition {
        self.position
    }

    pub(crate) const fn deposit_hash(self) -> B256 {
        self.deposit_hash
    }

    pub(crate) const fn sender(self) -> Address {
        self.sender
    }

    pub(crate) const fn to(self) -> Address {
        self.to
    }

    pub(crate) const fn token(self) -> Address {
        self.token
    }

    pub(crate) const fn amount(self) -> u128 {
        self.amount
    }

    pub(crate) const fn memo(self) -> B256 {
        self.memo
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObservedDepositFailed {
    pub(super) position: ObservedZoneEventPosition,
    pub(super) deposit_hash: B256,
    pub(super) sender: Address,
    pub(super) token: Address,
    pub(super) amount: u128,
}

impl ObservedDepositFailed {
    pub(crate) const fn position(self) -> ObservedZoneEventPosition {
        self.position
    }

    pub(crate) const fn deposit_hash(self) -> B256 {
        self.deposit_hash
    }

    pub(crate) const fn sender(self) -> Address {
        self.sender
    }

    pub(crate) const fn token(self) -> Address {
        self.token
    }

    pub(crate) const fn amount(self) -> u128 {
        self.amount
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObservedWithdrawalBounceBackProcessed {
    pub(super) position: ObservedZoneEventPosition,
    pub(super) zone_fallback_recipient: Address,
    pub(super) token: Address,
    pub(super) amount: u128,
}

impl ObservedWithdrawalBounceBackProcessed {
    pub(crate) const fn position(self) -> ObservedZoneEventPosition {
        self.position
    }

    pub(crate) const fn zone_fallback_recipient(self) -> Address {
        self.zone_fallback_recipient
    }

    pub(crate) const fn token(self) -> Address {
        self.token
    }

    pub(crate) const fn amount(self) -> u128 {
        self.amount
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObservedWithdrawalBounceBackPending {
    pub(super) position: ObservedZoneEventPosition,
    pub(super) zone_fallback_recipient: Address,
    pub(super) token: Address,
    pub(super) amount: u128,
}

impl ObservedWithdrawalBounceBackPending {
    pub(crate) const fn position(self) -> ObservedZoneEventPosition {
        self.position
    }

    pub(crate) const fn zone_fallback_recipient(self) -> Address {
        self.zone_fallback_recipient
    }

    pub(crate) const fn token(self) -> Address {
        self.token
    }

    pub(crate) const fn amount(self) -> u128 {
        self.amount
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservedWithdrawalRequested {
    pub(super) position: ObservedZoneEventPosition,
    pub(super) withdrawal_index: u64,
    pub(super) sender: Address,
    pub(super) token: Address,
    pub(super) to: Address,
    pub(super) amount: u128,
    pub(super) fee: u128,
    pub(super) memo: B256,
    pub(super) gas_limit: u64,
    pub(super) fallback_nonce: u64,
    pub(super) data: Bytes,
    pub(super) reveal_to: Bytes,
}

impl ObservedWithdrawalRequested {
    pub(crate) const fn position(&self) -> ObservedZoneEventPosition {
        self.position
    }

    pub(crate) const fn withdrawal_index(&self) -> u64 {
        self.withdrawal_index
    }

    pub(crate) const fn sender(&self) -> Address {
        self.sender
    }

    pub(crate) const fn token(&self) -> Address {
        self.token
    }

    pub(crate) const fn to(&self) -> Address {
        self.to
    }

    pub(crate) const fn amount(&self) -> u128 {
        self.amount
    }

    pub(crate) const fn fee(&self) -> u128 {
        self.fee
    }

    pub(crate) const fn memo(&self) -> B256 {
        self.memo
    }

    pub(crate) const fn gas_limit(&self) -> u64 {
        self.gas_limit
    }

    pub(crate) const fn fallback_nonce(&self) -> u64 {
        self.fallback_nonce
    }

    pub(crate) const fn data(&self) -> &Bytes {
        &self.data
    }

    pub(crate) const fn reveal_to(&self) -> &Bytes {
        &self.reveal_to
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObservedRefundClaimed {
    pub(super) position: ObservedZoneEventPosition,
    pub(super) recipient: Address,
    pub(super) token: Address,
    pub(super) amount: u128,
}

impl ObservedRefundClaimed {
    pub(crate) const fn position(self) -> ObservedZoneEventPosition {
        self.position
    }

    pub(crate) const fn recipient(self) -> Address {
        self.recipient
    }

    pub(crate) const fn token(self) -> Address {
        self.token
    }

    pub(crate) const fn amount(self) -> u128 {
        self.amount
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObservedBatchFinalized {
    pub(super) position: ObservedZoneEventPosition,
    pub(super) withdrawal_queue_hash: B256,
    pub(super) withdrawal_batch_index: u64,
}

impl ObservedBatchFinalized {
    pub(crate) const fn position(self) -> ObservedZoneEventPosition {
        self.position
    }

    pub(crate) const fn withdrawal_queue_hash(self) -> B256 {
        self.withdrawal_queue_hash
    }

    pub(crate) const fn withdrawal_batch_index(self) -> u64 {
        self.withdrawal_batch_index
    }
}

/// Exact implementation branch sequence for one advance-calldata deposit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ObservedDepositOutcome {
    OrdinaryMinted(ObservedDepositProcessed),
    OrdinaryFailed {
        withdrawal: Box<ObservedWithdrawalRequested>,
        failure: ObservedDepositFailed,
    },
    WithdrawalBounceBackMinted(ObservedWithdrawalBounceBackProcessed),
    WithdrawalBounceBackPending(ObservedWithdrawalBounceBackPending),
}

/// Output-producing post-advance operations in their original order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ObservedZoneOperation {
    WithdrawalRequested(ObservedWithdrawalRequested),
    RefundClaimed(ObservedRefundClaimed),
}

/// Concrete normalized outputs retained independently from model inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservedZoneOutputs {
    pub(super) tempo_block_finalized: ObservedTempoBlockFinalized,
    pub(super) token_enables: Vec<ObservedTokenEnabled>,
    pub(super) deposit_outcomes: Vec<ObservedDepositOutcome>,
    pub(super) tempo_advanced: ObservedTempoAdvanced,
    pub(super) operations: Vec<ObservedZoneOperation>,
    pub(super) batch_finalized: Option<ObservedBatchFinalized>,
}

impl ObservedZoneOutputs {
    pub(crate) const fn tempo_block_finalized(&self) -> ObservedTempoBlockFinalized {
        self.tempo_block_finalized
    }

    pub(crate) fn token_enables(&self) -> &[ObservedTokenEnabled] {
        &self.token_enables
    }

    pub(crate) fn deposit_outcomes(&self) -> &[ObservedDepositOutcome] {
        &self.deposit_outcomes
    }

    pub(crate) const fn tempo_advanced(&self) -> ObservedTempoAdvanced {
        self.tempo_advanced
    }

    pub(crate) fn operations(&self) -> &[ObservedZoneOperation] {
        &self.operations
    }

    pub(crate) const fn batch_finalized(&self) -> Option<ObservedBatchFinalized> {
        self.batch_finalized
    }

    #[cfg(test)]
    pub(crate) fn empty_for_test(
        tempo_block_hash: B256,
        tempo_block_number: u64,
        state_root: B256,
    ) -> Self {
        let position = ObservedZoneEventPosition {
            transaction_index: 0,
            receipt_log_index: 0,
            block_log_index: 0,
            transaction_hash: B256::ZERO,
        };
        Self {
            tempo_block_finalized: ObservedTempoBlockFinalized {
                position,
                block_hash: tempo_block_hash,
                block_number: tempo_block_number,
                state_root,
            },
            token_enables: Vec::new(),
            deposit_outcomes: Vec::new(),
            tempo_advanced: ObservedTempoAdvanced {
                position,
                tempo_block_hash,
                tempo_block_number,
                deposits_processed: U256::ZERO,
                new_processed_deposit_queue_hash: B256::ZERO,
                last_processed_deposit_number: 0,
            },
            operations: Vec::new(),
            batch_finalized: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_finalized_hash_for_test(mut self, block_hash: B256) -> Self {
        self.tempo_block_finalized.block_hash = block_hash;
        self
    }
}

/// One deterministic Zone model input paired with independent concrete output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ZoneProjection {
    pub(super) input: ZoneBlockInput,
    pub(super) outputs: ObservedZoneOutputs,
}

impl ZoneProjection {
    #[cfg(test)]
    pub(crate) const fn input(&self) -> &ZoneBlockInput {
        &self.input
    }

    pub(crate) const fn outputs(&self) -> &ObservedZoneOutputs {
        &self.outputs
    }

    pub(crate) fn apply<'a>(
        &self,
        imported: ImportedTempoTransition<'a>,
    ) -> Result<CompletedTransition<'a>, ModelError> {
        imported.apply_zone_block(&self.input)
    }
}
