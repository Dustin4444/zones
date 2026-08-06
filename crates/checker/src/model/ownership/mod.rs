//! Logical identities and phase-specific open-owner vocabulary.

use std::num::{NonZeroU64, NonZeroU128};

use alloy_primitives::{Address, Bytes, U256};

use super::{
    constants::NO_WITHDRAWAL_QUEUE_INDEX,
    encoding::{
        OrdinaryDeposit, SenderReveal, UserWithdrawalIdentity, UserWithdrawalRequest, Withdrawal,
        WithdrawalBounceBackDeposit, WithdrawalDataError,
    },
};

mod batch;

pub(crate) use batch::{
    BatchBoundary, BatchMembers, BatchOwner, BatchStateError, DepositCursor, FinalizedBatchState,
    SubmittedBatchState,
};

/// Identity of one Portal deposit queue member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct DepositId {
    pub(crate) portal: Address,
    pub(crate) deposit_number: NonZeroU64,
}

/// Identity of one accepted Zone withdrawal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct WithdrawalId {
    pub(crate) zone_id: u32,
    pub(crate) withdrawal_index: u64,
}

/// Identity of one finalized Zone withdrawal batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct BatchId {
    pub(crate) zone_id: u32,
    pub(crate) withdrawal_batch_index: NonZeroU64,
}

/// Identity of a user fallback lookup. Nonce zero is reserved for failed
/// ordinary deposits and never creates this owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct FallbackId {
    pub(crate) zone_id: u32,
    pub(crate) fallback_nonce: NonZeroU64,
}

/// Identity of one failed-deposit contribution to an aggregate Portal refund.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct PortalRefundId {
    pub(crate) token: Address,
    pub(crate) recipient: Address,
    pub(crate) failed_deposit: DepositId,
}

/// Native refund-map key shared by the Portal and Inbox aggregate ledgers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct RefundAccount {
    pub(crate) token: Address,
    pub(crate) recipient: Address,
}

/// Identity of one user-withdrawal contribution to an aggregate Inbox refund.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct InboxRefundId {
    pub(crate) token: Address,
    pub(crate) recipient: Address,
    pub(crate) user_withdrawal: WithdrawalId,
}

/// Stable identity of a submitted non-empty Portal queue slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct PortalQueueId {
    portal: Address,
    logical_queue_index: U256,
}

impl PortalQueueId {
    pub(crate) fn new(
        portal: Address,
        logical_queue_index: U256,
    ) -> Result<Self, PortalQueueIdError> {
        if logical_queue_index == NO_WITHDRAWAL_QUEUE_INDEX {
            return Err(PortalQueueIdError::EmptyBatchHasNoQueueIndex);
        }
        Ok(Self {
            portal,
            logical_queue_index,
        })
    }

    pub(crate) const fn portal(&self) -> Address {
        self.portal
    }

    pub(crate) const fn logical_queue_index(&self) -> U256 {
        self.logical_queue_index
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PortalQueueIdError {
    #[error("an empty batch has no Portal queue index")]
    EmptyBatchHasNoQueueIndex,
}

/// The two deposit origins share one ordered queue but have distinct owners.
/// Queue commitments live in the global/cursor state and are not duplicated
/// per owner, so an owner cannot disagree with the authoritative commitment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DepositOwner {
    /// External value escrowed by an ordinary `DepositMade`.
    PendingOrdinary {
        /// Complete checker-owned queue preimage retained for prefix matching.
        preimage: OrdinaryDeposit,
    },
    /// A failed user delivery recycled into the deposit queue. The fallback
    /// nonce is carried once, by `preimage`; the fallback owner remains open.
    PendingWithdrawalBounceBack {
        withdrawal: WithdrawalId,
        preimage: WithdrawalBounceBackDeposit,
    },
}

/// Origin retained after finalization; failed deposits use their unique
/// deposit identity rather than the reusable public nonce zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WithdrawalIdentity {
    User(UserWithdrawalIdentity),
    FailedDeposit { deposit: DepositId },
}

/// Authenticated user request retained until finalization supplies its opaque
/// encrypted sender. Private fields prevent failed-deposit zero rules from
/// being mixed into this shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UserPendingWithdrawal {
    identity: UserWithdrawalIdentity,
    request: UserWithdrawalRequest,
    sender_reveal: SenderReveal,
}

impl UserPendingWithdrawal {
    pub(crate) fn new(
        identity: UserWithdrawalIdentity,
        request: UserWithdrawalRequest,
        reveal_to: Bytes,
    ) -> Result<Self, WithdrawalDataError> {
        Ok(Self {
            identity,
            request,
            sender_reveal: SenderReveal::from_reveal_to(&reveal_to)?,
        })
    }

    /// Consume the pending user owner and form a coherent finalized owner.
    pub(crate) fn finalize(
        self,
        encrypted_sender: Bytes,
    ) -> Result<FinalizedWithdrawal, WithdrawalDataError> {
        let preimage = Withdrawal::for_user(
            self.identity,
            self.request,
            self.sender_reveal,
            encrypted_sender,
        )?;
        Ok(FinalizedWithdrawal {
            identity: WithdrawalIdentity::User(self.identity),
            preimage,
        })
    }
}

/// Failed-deposit pending withdrawal. Its shape deliberately has no gas,
/// callback, reveal, sender, transaction hash, or fallback-nonce fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FailedDepositPendingWithdrawal {
    deposit: DepositId,
    token: Address,
    to: Address,
    amount: u128,
}

impl FailedDepositPendingWithdrawal {
    /// Consume the exact ordinary-deposit preimage whose failed outcome creates
    /// this withdrawal. Economic fields are derived, never accepted from the
    /// implementation outcome a second time.
    pub(crate) const fn from_failed_deposit(deposit: DepositId, ordinary: OrdinaryDeposit) -> Self {
        Self {
            deposit,
            token: ordinary.token(),
            to: ordinary.tempo_refund_recipient(),
            amount: ordinary.amount(),
        }
    }

    /// Failed-deposit literal zero fields are supplied only here.
    pub(crate) fn finalize(self) -> FinalizedWithdrawal {
        FinalizedWithdrawal {
            identity: WithdrawalIdentity::FailedDeposit {
                deposit: self.deposit,
            },
            preimage: Withdrawal::for_failed_deposit(self.token, self.to, self.amount),
        }
    }
}

/// Pending owner is origin-specific so invalid user/failed-deposit field
/// combinations cannot be represented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PendingWithdrawal {
    User(UserPendingWithdrawal),
    FailedDeposit(FailedDepositPendingWithdrawal),
}

impl PendingWithdrawal {
    pub(crate) const fn identity(&self) -> WithdrawalIdentity {
        match self {
            Self::User(pending) => WithdrawalIdentity::User(pending.identity),
            Self::FailedDeposit(pending) => WithdrawalIdentity::FailedDeposit {
                deposit: pending.deposit,
            },
        }
    }

    /// Consume either origin while preserving its distinct encrypted-sender
    /// contract. Failed deposits always require the literal empty value.
    pub(crate) fn finalize(
        self,
        encrypted_sender: Bytes,
    ) -> Result<FinalizedWithdrawal, WithdrawalDataError> {
        match self {
            Self::User(pending) => pending.finalize(encrypted_sender),
            Self::FailedDeposit(pending) if encrypted_sender.is_empty() => Ok(pending.finalize()),
            Self::FailedDeposit(_) => Err(WithdrawalDataError::InvalidEncryptedSenderLength {
                actual: encrypted_sender.len(),
                expected: 0,
            }),
        }
    }
}

/// Complete finalized withdrawal owner, including its immutable origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FinalizedWithdrawal {
    identity: WithdrawalIdentity,
    preimage: Withdrawal,
}

impl FinalizedWithdrawal {
    pub(crate) const fn identity(&self) -> WithdrawalIdentity {
        self.identity
    }

    pub(crate) const fn preimage(&self) -> &Withdrawal {
        &self.preimage
    }
}

/// One withdrawal row changes variant in place as ownership advances.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WithdrawalOwner {
    Pending(PendingWithdrawal),
    Finalized(FinalizedWithdrawal),
}

/// Auxiliary lookup retained for a user withdrawal until delivery or imported
/// bounce-back consumes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FallbackOwner {
    Held {
        withdrawal: WithdrawalId,
        token: Address,
        amount: NonZeroU128,
    },
    /// One Portal bounce-back deposit is queued while the fallback remains
    /// open for the Inbox to consume.
    BounceBackQueued {
        withdrawal: WithdrawalId,
        token: Address,
        amount: NonZeroU128,
        deposit: DepositId,
    },
}

/// Per-origin contribution to the Portal's recipient-aggregated refund map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PortalRefundOwner {
    Pending { amount: u128 },
}

/// Per-origin contribution to the Inbox's recipient-aggregated refund map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InboxRefundOwner {
    Pending { amount: NonZeroU128 },
}

#[cfg(test)]
mod tests;
