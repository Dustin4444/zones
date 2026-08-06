use crate::{
    model::{
        accounting::{AccountingError, Component},
        encoding::{WithdrawalDataError, WithdrawalQueueError},
        fees::FeeError,
        ownership::{BatchStateError, PortalQueueIdError},
        transition::{
            DepositKind, DepositOutcomeKind, WithdrawalOriginKind, WithdrawalProcessingOutcomeKind,
        },
    },
    store::error::StoreResult,
};

use super::Canonical;

pub(super) fn deposit_kind(value: DepositKind) -> u8 {
    match value {
        DepositKind::Ordinary => 1,
        DepositKind::WithdrawalBounceBack => 2,
    }
}

pub(super) fn deposit_outcome_kind(value: DepositOutcomeKind) -> u8 {
    match value {
        DepositOutcomeKind::OrdinaryMinted => 1,
        DepositOutcomeKind::OrdinaryFailed => 2,
        DepositOutcomeKind::WithdrawalBounceBackMinted => 3,
        DepositOutcomeKind::WithdrawalBounceBackPending => 4,
    }
}

pub(super) fn withdrawal_origin(value: WithdrawalOriginKind) -> u8 {
    match value {
        WithdrawalOriginKind::User => 1,
        WithdrawalOriginKind::FailedDeposit => 2,
    }
}

pub(super) fn processing_outcome(value: WithdrawalProcessingOutcomeKind) -> u8 {
    match value {
        WithdrawalProcessingOutcomeKind::UserDelivered => 1,
        WithdrawalProcessingOutcomeKind::UserBounced => 2,
        WithdrawalProcessingOutcomeKind::FailedDepositPaid => 3,
        WithdrawalProcessingOutcomeKind::FailedDepositPending => 4,
    }
}

pub(super) fn accounting(encoder: &mut Canonical, error: AccountingError) {
    match error {
        AccountingError::NotInitialized => encoder.u8(1),
        AccountingError::AlreadyInitialized => encoder.u8(2),
        AccountingError::Overflow(component) => {
            encoder.u8(3);
            encoder.u8(component_tag(component));
        }
        AccountingError::Underflow(component) => {
            encoder.u8(4);
            encoder.u8(component_tag(component));
        }
    }
}

pub(super) fn fee(encoder: &mut Canonical, error: FeeError) {
    match error {
        FeeError::Overflow => encoder.u8(1),
    }
}

pub(super) fn withdrawal_data(
    encoder: &mut Canonical,
    error: WithdrawalDataError,
) -> StoreResult<()> {
    super::errors::encode_withdrawal_data(encoder, &error)
}

pub(super) fn batch_state(encoder: &mut Canonical, error: BatchStateError) -> StoreResult<()> {
    match error {
        BatchStateError::MemberCountOverflow { actual } => {
            encoder.u8(1);
            encoder.usize(actual)?;
        }
        BatchStateError::EmptyBatchCannotBeSubmitted => encoder.u8(2),
        BatchStateError::EmptyBatchHasQueueCommitment => encoder.u8(3),
        BatchStateError::NonEmptyBatchHasNoQueueCommitment => encoder.u8(4),
        BatchStateError::SubmittedBatchHasNoRemainingQueue => encoder.u8(5),
        BatchStateError::ProcessingOrdinalDidNotAdvance { current, next } => {
            encoder.u8(6);
            encoder.u64(current);
            encoder.u64(next);
        }
        BatchStateError::ProcessingOrdinalOutOfRange {
            ordinal,
            member_count,
        } => {
            encoder.u8(7);
            encoder.u64(ordinal);
            encoder.u64(member_count);
        }
        BatchStateError::WithdrawalRangeOverflow {
            first_withdrawal_index,
            member_count,
        } => {
            encoder.u8(8);
            encoder.u64(first_withdrawal_index);
            encoder.u64(member_count);
        }
    }
    Ok(())
}

pub(super) fn portal_queue_id(encoder: &mut Canonical, error: PortalQueueIdError) {
    match error {
        PortalQueueIdError::EmptyBatchHasNoQueueIndex => encoder.u8(1),
    }
}

pub(super) fn withdrawal_queue(encoder: &mut Canonical, error: WithdrawalQueueError) {
    match error {
        WithdrawalQueueError::EmptyPrefixUsesNoQueueState => encoder.u8(1),
        WithdrawalQueueError::SentinelCannotBeCurrentQueue => encoder.u8(2),
        WithdrawalQueueError::SentinelCannotBeSuppliedAsSuffix => encoder.u8(3),
        WithdrawalQueueError::CommitmentMismatch { expected, actual } => {
            encoder.u8(4);
            encoder.hash(expected);
            encoder.hash(actual);
        }
    }
}

fn component_tag(component: Component) -> u8 {
    match component {
        Component::Supply => 1,
        Component::DepositLiability => 2,
        Component::WithdrawalLiability => 3,
        Component::WithdrawalBurn => 4,
        Component::CollateralRequirement => 5,
    }
}
