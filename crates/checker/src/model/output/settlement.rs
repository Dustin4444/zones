//! Expected Portal settlement and refund outputs.

use alloy_primitives::{Address, B256, U256};

use super::ExpectedDepositAppend;
use crate::model::{
    encoding::{Withdrawal, WithdrawalBounceBackDeposit},
    ownership::{BatchId, DepositId, WithdrawalId},
};

/// Exact `BatchSubmitted` fields derived from the next finalized batch and
/// the modeled Portal queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExpectedBatchSubmission {
    batch: BatchId,
    withdrawal_queue_index: U256,
    next_processed_deposit_queue_hash: B256,
    next_block_hash: B256,
    withdrawal_queue_hash: B256,
    last_processed_deposit_number: u64,
}

impl ExpectedBatchSubmission {
    pub(in crate::model) const fn new(
        batch: BatchId,
        withdrawal_queue_index: U256,
        next_processed_deposit_queue_hash: B256,
        next_block_hash: B256,
        withdrawal_queue_hash: B256,
        last_processed_deposit_number: u64,
    ) -> Self {
        Self {
            batch,
            withdrawal_queue_index,
            next_processed_deposit_queue_hash,
            next_block_hash,
            withdrawal_queue_hash,
            last_processed_deposit_number,
        }
    }

    pub(crate) const fn batch(&self) -> BatchId {
        self.batch
    }

    pub(crate) const fn withdrawal_queue_index(&self) -> U256 {
        self.withdrawal_queue_index
    }

    pub(crate) const fn next_processed_deposit_queue_hash(&self) -> B256 {
        self.next_processed_deposit_queue_hash
    }

    pub(crate) const fn next_block_hash(&self) -> B256 {
        self.next_block_hash
    }

    pub(crate) const fn withdrawal_queue_hash(&self) -> B256 {
        self.withdrawal_queue_hash
    }

    pub(crate) const fn last_processed_deposit_number(&self) -> u64 {
        self.last_processed_deposit_number
    }
}

/// Exact terminal `WithdrawalProcessed` fields for one user-origin member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExpectedWithdrawalProcessed {
    withdrawal: WithdrawalId,
    to: Address,
    sender_tag: B256,
    token: Address,
    amount: u128,
    callback_success: bool,
}

impl ExpectedWithdrawalProcessed {
    pub(in crate::model) const fn delivered(
        withdrawal: WithdrawalId,
        preimage: &Withdrawal,
    ) -> Self {
        Self::new(withdrawal, preimage, true)
    }

    pub(in crate::model) const fn bounced(withdrawal: WithdrawalId, preimage: &Withdrawal) -> Self {
        Self::new(withdrawal, preimage, false)
    }

    const fn new(withdrawal: WithdrawalId, preimage: &Withdrawal, callback_success: bool) -> Self {
        Self {
            withdrawal,
            to: preimage.to(),
            sender_tag: preimage.sender_tag(),
            token: preimage.token(),
            amount: preimage.amount(),
            callback_success,
        }
    }

    pub(crate) const fn withdrawal(&self) -> WithdrawalId {
        self.withdrawal
    }

    pub(crate) const fn to(&self) -> Address {
        self.to
    }

    pub(crate) const fn sender_tag(&self) -> B256 {
        self.sender_tag
    }

    pub(crate) const fn token(&self) -> Address {
        self.token
    }

    pub(crate) const fn amount(&self) -> u128 {
        self.amount
    }

    pub(crate) const fn callback_success(&self) -> bool {
        self.callback_success
    }
}

/// Exact `WithdrawalBounceBack` append fields. `deposit` carries the event's
/// nonce/token/amount and `append` carries its independently derived queue
/// number and commitment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExpectedWithdrawalBounceBackAppend {
    deposit: WithdrawalBounceBackDeposit,
    append: ExpectedDepositAppend,
}

impl ExpectedWithdrawalBounceBackAppend {
    pub(in crate::model) const fn new(
        deposit: WithdrawalBounceBackDeposit,
        append: ExpectedDepositAppend,
    ) -> Self {
        Self { deposit, append }
    }

    pub(crate) const fn deposit(&self) -> WithdrawalBounceBackDeposit {
        self.deposit
    }

    pub(crate) const fn append(&self) -> ExpectedDepositAppend {
        self.append
    }
}

/// Exact direct or pending failed-deposit refund event fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExpectedDepositRefund {
    failed_deposit: DepositId,
    recipient: Address,
    token: Address,
    amount: u128,
    bounceback_fee: u128,
}

impl ExpectedDepositRefund {
    pub(in crate::model) const fn new(
        failed_deposit: DepositId,
        recipient: Address,
        token: Address,
        amount: u128,
        bounceback_fee: u128,
    ) -> Self {
        Self {
            failed_deposit,
            recipient,
            token,
            amount,
            bounceback_fee,
        }
    }

    pub(crate) const fn failed_deposit(&self) -> DepositId {
        self.failed_deposit
    }

    pub(crate) const fn recipient(&self) -> Address {
        self.recipient
    }

    pub(crate) const fn token(&self) -> Address {
        self.token
    }

    pub(crate) const fn amount(&self) -> u128 {
        self.amount
    }

    pub(crate) const fn bounceback_fee(&self) -> u128 {
        self.bounceback_fee
    }
}

/// Native success order is zero or more callback `DepositMade` events followed
/// by one terminal `WithdrawalProcessed(success=true)` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExpectedUserWithdrawalDelivery {
    callback_deposit_appends: Vec<ExpectedDepositAppend>,
    processed: ExpectedWithdrawalProcessed,
}

impl ExpectedUserWithdrawalDelivery {
    pub(in crate::model) const fn new(
        callback_deposit_appends: Vec<ExpectedDepositAppend>,
        processed: ExpectedWithdrawalProcessed,
    ) -> Self {
        Self {
            callback_deposit_appends,
            processed,
        }
    }

    pub(crate) fn callback_deposit_appends(&self) -> &[ExpectedDepositAppend] {
        &self.callback_deposit_appends
    }

    pub(crate) const fn processed(&self) -> ExpectedWithdrawalProcessed {
        self.processed
    }
}

/// Native bounce order is the deposit append first and then the terminal
/// `WithdrawalProcessed(success=false)` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExpectedUserWithdrawalBounce {
    first: ExpectedWithdrawalBounceBackAppend,
    second: ExpectedWithdrawalProcessed,
}

impl ExpectedUserWithdrawalBounce {
    pub(in crate::model) const fn new(
        first: ExpectedWithdrawalBounceBackAppend,
        second: ExpectedWithdrawalProcessed,
    ) -> Self {
        Self { first, second }
    }

    pub(crate) const fn first(&self) -> ExpectedWithdrawalBounceBackAppend {
        self.first
    }

    pub(crate) const fn second(&self) -> ExpectedWithdrawalProcessed {
        self.second
    }
}

/// One origin-typed terminal outcome from a `processWithdrawals` member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExpectedProcessedWithdrawal {
    UserDelivered(Box<ExpectedUserWithdrawalDelivery>),
    UserBounced(Box<ExpectedUserWithdrawalBounce>),
    FailedDepositPaid(ExpectedDepositRefund),
    FailedDepositPending(ExpectedDepositRefund),
}

/// Expected native event grammar for one direct `processWithdrawals` call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ExpectedWithdrawalProcessing {
    members: Vec<ExpectedProcessedWithdrawal>,
}

impl ExpectedWithdrawalProcessing {
    pub(in crate::model) const fn new(members: Vec<ExpectedProcessedWithdrawal>) -> Self {
        Self { members }
    }

    pub(crate) fn members(&self) -> &[ExpectedProcessedWithdrawal] {
        &self.members
    }
}

/// Exact aggregate `RefundClaimed` fields after all matching per-origin owners
/// have been summed and closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExpectedRefundClaim {
    recipient: Address,
    token: Address,
    amount: u128,
}

impl ExpectedRefundClaim {
    pub(in crate::model) const fn new(recipient: Address, token: Address, amount: u128) -> Self {
        Self {
            recipient,
            token,
            amount,
        }
    }

    pub(crate) const fn recipient(&self) -> Address {
        self.recipient
    }

    pub(crate) const fn token(&self) -> Address {
        self.token
    }

    pub(crate) const fn amount(&self) -> u128 {
        self.amount
    }
}
