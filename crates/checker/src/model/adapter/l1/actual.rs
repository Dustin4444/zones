//! Disposable normalized outputs from one authenticated Tempo block.

use alloy_primitives::{Address, B256, U256};

use crate::model::{
    input::ImportedTempoBlockInput,
    state::ModelState,
    transition::{ImportedTempoTransition, ModelError, ModelTransition},
};

/// Stable event coordinates copied from one authenticated receipt position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ObservedEventPosition {
    pub(super) transaction_index: usize,
    pub(super) receipt_log_index: usize,
    pub(super) block_log_index: usize,
    pub(super) transaction_hash: B256,
}

impl ObservedEventPosition {
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

/// Actual output-only fields from one Portal deposit append.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObservedDepositAppend {
    pub(super) position: ObservedEventPosition,
    pub(super) queue_hash: B256,
    pub(super) deposit_number: u64,
}

impl ObservedDepositAppend {
    pub(crate) const fn position(self) -> ObservedEventPosition {
        self.position
    }

    pub(crate) const fn queue_hash(self) -> B256 {
        self.queue_hash
    }

    pub(crate) const fn deposit_number(self) -> u64 {
        self.deposit_number
    }
}

/// Actual `BatchSubmitted` fields, kept distinct from call-derived input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObservedSubmittedBatch {
    pub(super) position: ObservedEventPosition,
    pub(super) withdrawal_batch_index: u64,
    pub(super) withdrawal_queue_index: U256,
    pub(super) next_processed_deposit_queue_hash: B256,
    pub(super) next_block_hash: B256,
    pub(super) withdrawal_queue_hash: B256,
    pub(super) last_processed_deposit_number: u64,
}

impl ObservedSubmittedBatch {
    pub(crate) const fn position(self) -> ObservedEventPosition {
        self.position
    }

    pub(crate) const fn withdrawal_batch_index(self) -> u64 {
        self.withdrawal_batch_index
    }

    pub(crate) const fn withdrawal_queue_index(self) -> U256 {
        self.withdrawal_queue_index
    }

    pub(crate) const fn next_processed_deposit_queue_hash(self) -> B256 {
        self.next_processed_deposit_queue_hash
    }

    pub(crate) const fn next_block_hash(self) -> B256 {
        self.next_block_hash
    }

    pub(crate) const fn withdrawal_queue_hash(self) -> B256 {
        self.withdrawal_queue_hash
    }

    pub(crate) const fn last_processed_deposit_number(self) -> u64 {
        self.last_processed_deposit_number
    }
}

/// Actual terminal `WithdrawalProcessed` fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObservedWithdrawalProcessed {
    pub(super) position: ObservedEventPosition,
    pub(super) to: Address,
    pub(super) sender_tag: B256,
    pub(super) token: Address,
    pub(super) amount: u128,
    pub(super) callback_success: bool,
}

impl ObservedWithdrawalProcessed {
    pub(crate) const fn position(self) -> ObservedEventPosition {
        self.position
    }

    pub(crate) const fn to(self) -> Address {
        self.to
    }

    pub(crate) const fn sender_tag(self) -> B256 {
        self.sender_tag
    }

    pub(crate) const fn token(self) -> Address {
        self.token
    }

    pub(crate) const fn amount(self) -> u128 {
        self.amount
    }

    pub(crate) const fn callback_success(self) -> bool {
        self.callback_success
    }
}

/// Actual `WithdrawalBounceBack` append fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObservedWithdrawalBounceBackAppend {
    pub(super) position: ObservedEventPosition,
    pub(super) queue_hash: B256,
    pub(super) fallback_nonce: u64,
    pub(super) token: Address,
    pub(super) amount: u128,
    pub(super) deposit_number: u64,
}

impl ObservedWithdrawalBounceBackAppend {
    pub(crate) const fn position(self) -> ObservedEventPosition {
        self.position
    }

    pub(crate) const fn queue_hash(self) -> B256 {
        self.queue_hash
    }

    pub(crate) const fn fallback_nonce(self) -> u64 {
        self.fallback_nonce
    }

    pub(crate) const fn token(self) -> Address {
        self.token
    }

    pub(crate) const fn amount(self) -> u128 {
        self.amount
    }

    pub(crate) const fn deposit_number(self) -> u64 {
        self.deposit_number
    }
}

/// Actual direct or pending failed-deposit refund fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObservedDepositRefund {
    pub(super) position: ObservedEventPosition,
    pub(super) recipient: Address,
    pub(super) token: Address,
    pub(super) amount: u128,
    pub(super) bounceback_fee: u128,
}

impl ObservedDepositRefund {
    pub(crate) const fn position(self) -> ObservedEventPosition {
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

    pub(crate) const fn bounceback_fee(self) -> u128 {
        self.bounceback_fee
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservedUserWithdrawalDelivery {
    pub(super) callback_deposits: Vec<ObservedDepositAppend>,
    pub(super) processed: ObservedWithdrawalProcessed,
}

impl ObservedUserWithdrawalDelivery {
    pub(crate) fn callback_deposits(&self) -> &[ObservedDepositAppend] {
        &self.callback_deposits
    }

    pub(crate) const fn processed(&self) -> ObservedWithdrawalProcessed {
        self.processed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservedUserWithdrawalBounce {
    pub(super) append: ObservedWithdrawalBounceBackAppend,
    pub(super) processed: ObservedWithdrawalProcessed,
}

impl ObservedUserWithdrawalBounce {
    pub(crate) const fn append(&self) -> ObservedWithdrawalBounceBackAppend {
        self.append
    }

    pub(crate) const fn processed(&self) -> ObservedWithdrawalProcessed {
        self.processed
    }
}

/// One transaction-aware actual branch for one calldata member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ObservedProcessedWithdrawal {
    UserDelivered(ObservedUserWithdrawalDelivery),
    UserBounced(ObservedUserWithdrawalBounce),
    FailedDepositPaid(ObservedDepositRefund),
    FailedDepositPending(ObservedDepositRefund),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservedWithdrawalProcessing {
    pub(super) transaction_index: usize,
    pub(super) transaction_hash: B256,
    pub(super) members: Vec<ObservedProcessedWithdrawal>,
}

impl ObservedWithdrawalProcessing {
    pub(crate) const fn transaction_index(&self) -> usize {
        self.transaction_index
    }

    pub(crate) const fn transaction_hash(&self) -> B256 {
        self.transaction_hash
    }

    pub(crate) fn members(&self) -> &[ObservedProcessedWithdrawal] {
        &self.members
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObservedRefundClaim {
    pub(super) position: ObservedEventPosition,
    pub(super) recipient: Address,
    pub(super) token: Address,
    pub(super) amount: u128,
}

impl ObservedRefundClaim {
    pub(crate) const fn position(self) -> ObservedEventPosition {
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

/// Ordered implementation outputs aligned one-for-one with model expectations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ObservedImportedOutput {
    DepositAppended(ObservedDepositAppend),
    BatchSubmitted(ObservedSubmittedBatch),
    WithdrawalsProcessed(ObservedWithdrawalProcessing),
    RefundClaimed(ObservedRefundClaim),
}

#[cfg(test)]
impl ObservedImportedOutput {
    pub(crate) const fn deposit_append_for_test(queue_hash: B256, deposit_number: u64) -> Self {
        Self::DepositAppended(ObservedDepositAppend {
            position: ObservedEventPosition {
                transaction_index: 0,
                receipt_log_index: 0,
                block_log_index: 0,
                transaction_hash: B256::ZERO,
            },
            queue_hash,
            deposit_number,
        })
    }
}

/// One authenticated imported-block input plus the distinct actual outputs it
/// must eventually be compared against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportedProjection {
    pub(super) input: ImportedTempoBlockInput,
    pub(super) outputs: Vec<ObservedImportedOutput>,
}

impl ImportedProjection {
    pub(crate) const fn input(&self) -> &ImportedTempoBlockInput {
        &self.input
    }

    pub(crate) fn outputs(&self) -> &[ObservedImportedOutput] {
        &self.outputs
    }

    pub(crate) fn apply<'a>(
        &self,
        state: &'a ModelState,
    ) -> Result<ImportedTempoTransition<'a>, ModelError> {
        ModelTransition::new(state).apply_imported_tempo_block(&self.input)
    }
}
