use std::collections::BTreeSet;

use alloy_primitives::B256;

use crate::model::{
    accounting::{AccountingError, Component},
    encoding::{WithdrawalDataError, WithdrawalQueueError},
    fees::FeeError,
    ownership::{BatchStateError, PortalQueueIdError},
    transition::{
        DepositKind, DepositOutcomeKind, ModelError, WithdrawalOriginKind,
        WithdrawalProcessingOutcomeKind,
    },
};

use super::super::assert_golden;
use super::{Case, expected, model};

pub(super) fn top_level_cases() -> Vec<Case> {
    vec![
        (
            ModelError::Accounting(AccountingError::NotInitialized),
            expected(0x47, |bytes| bytes.u8(1)),
        ),
        (
            ModelError::Fee(FeeError::Overflow),
            expected(0x48, |bytes| bytes.u8(1)),
        ),
        (
            ModelError::WithdrawalData(WithdrawalDataError::ZeroAmount),
            expected(0x49, |bytes| bytes.u8(1)),
        ),
        (
            ModelError::BatchState(BatchStateError::MemberCountOverflow { actual: 1 }),
            expected(0x4a, |bytes| {
                bytes.u8(1);
                bytes.usize(1);
            }),
        ),
        (
            ModelError::PortalQueueId(PortalQueueIdError::EmptyBatchHasNoQueueIndex),
            expected(0x4b, |bytes| bytes.u8(1)),
        ),
        (
            ModelError::WithdrawalQueue(WithdrawalQueueError::EmptyPrefixUsesNoQueueState),
            expected(0x4c, |bytes| bytes.u8(1)),
        ),
    ]
}

fn set(values: impl IntoIterator<Item = u8>) -> BTreeSet<u8> {
    values.into_iter().collect()
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

fn accounting_tag(error: AccountingError) -> u8 {
    match error {
        AccountingError::NotInitialized => 1,
        AccountingError::AlreadyInitialized => 2,
        AccountingError::Overflow(_) => 3,
        AccountingError::Underflow(_) => 4,
    }
}

#[test]
fn every_accounting_error_and_component_is_golden() {
    let components = [
        Component::Supply,
        Component::DepositLiability,
        Component::WithdrawalLiability,
        Component::WithdrawalBurn,
        Component::CollateralRequirement,
    ];
    assert_eq!(set(components.map(component_tag)), set(1..=5));

    let errors = [
        AccountingError::NotInitialized,
        AccountingError::AlreadyInitialized,
    ]
    .into_iter()
    .chain(components.map(AccountingError::Overflow))
    .chain(components.map(AccountingError::Underflow))
    .collect::<Vec<_>>();
    assert_eq!(set(errors.iter().copied().map(accounting_tag)), set(1..=4));
    for error in errors {
        let mut bytes = expected(0x47, |bytes| bytes.u8(accounting_tag(error)));
        if let AccountingError::Overflow(component) | AccountingError::Underflow(component) = error
        {
            bytes.push(component_tag(component));
        }
        assert_golden(model(&ModelError::Accounting(error)).unwrap(), &bytes);
    }
}

fn withdrawal_data_tag(error: WithdrawalDataError) -> u8 {
    match error {
        WithdrawalDataError::ZeroAmount => 1,
        WithdrawalDataError::ZeroTransactionHash => 2,
        WithdrawalDataError::GasLimitTooHigh { .. } => 3,
        WithdrawalDataError::CallbackDataTooLong { .. } => 4,
        WithdrawalDataError::InvalidRevealToLength { .. } => 5,
        WithdrawalDataError::InvalidRevealToPrefix { .. } => 6,
        WithdrawalDataError::InvalidEncryptedSenderLength { .. } => 7,
        WithdrawalDataError::InvalidAuthenticatedEncryptedSenderLength { .. } => 8,
    }
}

#[test]
fn every_withdrawal_data_error_is_golden() {
    let cases = vec![
        (
            WithdrawalDataError::ZeroAmount,
            expected(0x49, |bytes| bytes.u8(1)),
        ),
        (
            WithdrawalDataError::ZeroTransactionHash,
            expected(0x49, |bytes| bytes.u8(2)),
        ),
        (
            WithdrawalDataError::GasLimitTooHigh {
                actual: 3,
                maximum: 4,
            },
            expected(0x49, |bytes| {
                bytes.u8(3);
                bytes.u64(3);
                bytes.u64(4);
            }),
        ),
        (
            WithdrawalDataError::CallbackDataTooLong {
                actual: 5,
                maximum: 6,
            },
            expected(0x49, |bytes| {
                bytes.u8(4);
                bytes.usize(5);
                bytes.usize(6);
            }),
        ),
        (
            WithdrawalDataError::InvalidRevealToLength {
                actual: 7,
                expected: 8,
            },
            expected(0x49, |bytes| {
                bytes.u8(5);
                bytes.usize(7);
                bytes.usize(8);
            }),
        ),
        (
            WithdrawalDataError::InvalidRevealToPrefix { actual: 9 },
            expected(0x49, |bytes| {
                bytes.u8(6);
                bytes.u8(9);
            }),
        ),
        (
            WithdrawalDataError::InvalidEncryptedSenderLength {
                actual: 10,
                expected: 11,
            },
            expected(0x49, |bytes| {
                bytes.u8(7);
                bytes.usize(10);
                bytes.usize(11);
            }),
        ),
        (
            WithdrawalDataError::InvalidAuthenticatedEncryptedSenderLength {
                actual: 12,
                nonempty_expected: 13,
            },
            expected(0x49, |bytes| {
                bytes.u8(8);
                bytes.usize(12);
                bytes.usize(13);
            }),
        ),
    ];
    assert_eq!(
        set(cases.iter().map(|(error, _)| withdrawal_data_tag(*error))),
        set(1..=8),
    );
    for (error, bytes) in cases {
        assert_golden(model(&ModelError::WithdrawalData(error)).unwrap(), &bytes);
    }
}

fn batch_state_tag(error: BatchStateError) -> u8 {
    match error {
        BatchStateError::MemberCountOverflow { .. } => 1,
        BatchStateError::EmptyBatchCannotBeSubmitted => 2,
        BatchStateError::EmptyBatchHasQueueCommitment => 3,
        BatchStateError::NonEmptyBatchHasNoQueueCommitment => 4,
        BatchStateError::SubmittedBatchHasNoRemainingQueue => 5,
        BatchStateError::ProcessingOrdinalDidNotAdvance { .. } => 6,
        BatchStateError::ProcessingOrdinalOutOfRange { .. } => 7,
        BatchStateError::WithdrawalRangeOverflow { .. } => 8,
    }
}

#[test]
fn every_batch_state_error_is_golden() {
    let cases = vec![
        (
            BatchStateError::MemberCountOverflow { actual: 1 },
            expected(0x4a, |bytes| {
                bytes.u8(1);
                bytes.usize(1);
            }),
        ),
        (
            BatchStateError::EmptyBatchCannotBeSubmitted,
            expected(0x4a, |b| b.u8(2)),
        ),
        (
            BatchStateError::EmptyBatchHasQueueCommitment,
            expected(0x4a, |b| b.u8(3)),
        ),
        (
            BatchStateError::NonEmptyBatchHasNoQueueCommitment,
            expected(0x4a, |b| b.u8(4)),
        ),
        (
            BatchStateError::SubmittedBatchHasNoRemainingQueue,
            expected(0x4a, |b| b.u8(5)),
        ),
        (
            BatchStateError::ProcessingOrdinalDidNotAdvance {
                current: 6,
                next: 7,
            },
            expected(0x4a, |bytes| {
                bytes.u8(6);
                bytes.u64(6);
                bytes.u64(7);
            }),
        ),
        (
            BatchStateError::ProcessingOrdinalOutOfRange {
                ordinal: 8,
                member_count: 9,
            },
            expected(0x4a, |bytes| {
                bytes.u8(7);
                bytes.u64(8);
                bytes.u64(9);
            }),
        ),
        (
            BatchStateError::WithdrawalRangeOverflow {
                first_withdrawal_index: 10,
                member_count: 11,
            },
            expected(0x4a, |bytes| {
                bytes.u8(8);
                bytes.u64(10);
                bytes.u64(11);
            }),
        ),
    ];
    assert_eq!(
        set(cases.iter().map(|(error, _)| batch_state_tag(*error))),
        set(1..=8)
    );
    for (error, bytes) in cases {
        assert_golden(model(&ModelError::BatchState(error)).unwrap(), &bytes);
    }
}

fn withdrawal_queue_tag(error: WithdrawalQueueError) -> u8 {
    match error {
        WithdrawalQueueError::EmptyPrefixUsesNoQueueState => 1,
        WithdrawalQueueError::SentinelCannotBeCurrentQueue => 2,
        WithdrawalQueueError::SentinelCannotBeSuppliedAsSuffix => 3,
        WithdrawalQueueError::CommitmentMismatch { .. } => 4,
    }
}

fn fee_tag(error: FeeError) -> u8 {
    match error {
        FeeError::Overflow => 1,
    }
}

fn portal_queue_tag(error: PortalQueueIdError) -> u8 {
    match error {
        PortalQueueIdError::EmptyBatchHasNoQueueIndex => 1,
    }
}

#[test]
fn portal_queue_fee_and_every_withdrawal_queue_error_are_golden() {
    let portal_error = PortalQueueIdError::EmptyBatchHasNoQueueIndex;
    assert_eq!(set([portal_queue_tag(portal_error)]), set(1..=1));
    let portal = ModelError::PortalQueueId(portal_error);
    assert_golden(
        model(&portal).unwrap(),
        &expected(0x4b, |bytes| bytes.u8(1)),
    );
    let fee_error = FeeError::Overflow;
    assert_eq!(set([fee_tag(fee_error)]), set(1..=1));
    let fee = ModelError::Fee(fee_error);
    assert_golden(model(&fee).unwrap(), &expected(0x48, |bytes| bytes.u8(1)));

    let cases = vec![
        (
            WithdrawalQueueError::EmptyPrefixUsesNoQueueState,
            expected(0x4c, |b| b.u8(1)),
        ),
        (
            WithdrawalQueueError::SentinelCannotBeCurrentQueue,
            expected(0x4c, |b| b.u8(2)),
        ),
        (
            WithdrawalQueueError::SentinelCannotBeSuppliedAsSuffix,
            expected(0x4c, |b| b.u8(3)),
        ),
        (
            WithdrawalQueueError::CommitmentMismatch {
                expected: B256::repeat_byte(4),
                actual: B256::repeat_byte(5),
            },
            expected(0x4c, |bytes| {
                bytes.u8(4);
                bytes.hash(B256::repeat_byte(4));
                bytes.hash(B256::repeat_byte(5));
            }),
        ),
    ];
    assert_eq!(
        set(cases.iter().map(|(error, _)| withdrawal_queue_tag(*error))),
        set(1..=4)
    );
    for (error, bytes) in cases {
        assert_golden(model(&ModelError::WithdrawalQueue(error)).unwrap(), &bytes);
    }
}

#[test]
fn every_deposit_and_processing_branch_tag_is_golden() {
    let deposits = [
        (DepositKind::Ordinary, DepositOutcomeKind::OrdinaryMinted),
        (
            DepositKind::WithdrawalBounceBack,
            DepositOutcomeKind::OrdinaryFailed,
        ),
        (
            DepositKind::Ordinary,
            DepositOutcomeKind::WithdrawalBounceBackMinted,
        ),
        (
            DepositKind::WithdrawalBounceBack,
            DepositOutcomeKind::WithdrawalBounceBackPending,
        ),
    ];
    let deposit_tag = |kind| match kind {
        DepositKind::Ordinary => 1,
        DepositKind::WithdrawalBounceBack => 2,
    };
    let outcome_tag = |kind| match kind {
        DepositOutcomeKind::OrdinaryMinted => 1,
        DepositOutcomeKind::OrdinaryFailed => 2,
        DepositOutcomeKind::WithdrawalBounceBackMinted => 3,
        DepositOutcomeKind::WithdrawalBounceBackPending => 4,
    };
    assert_eq!(set(deposits.map(|case| deposit_tag(case.0))), set(1..=2));
    assert_eq!(set(deposits.map(|case| outcome_tag(case.1))), set(1..=4));
    for (index, (origin, outcome)) in deposits.into_iter().enumerate() {
        let number = u64::try_from(index).unwrap();
        let bytes = expected(0x15, |bytes| {
            bytes.u64(number);
            bytes.u8(deposit_tag(origin));
            bytes.u8(outcome_tag(outcome));
        });
        let error = ModelError::DepositOutcomeKindMismatch {
            number,
            expected: origin,
            actual: outcome,
        };
        assert_golden(model(&error).unwrap(), &bytes);
    }

    let processing = [
        (
            WithdrawalOriginKind::User,
            WithdrawalProcessingOutcomeKind::UserDelivered,
        ),
        (
            WithdrawalOriginKind::FailedDeposit,
            WithdrawalProcessingOutcomeKind::UserBounced,
        ),
        (
            WithdrawalOriginKind::User,
            WithdrawalProcessingOutcomeKind::FailedDepositPaid,
        ),
        (
            WithdrawalOriginKind::FailedDeposit,
            WithdrawalProcessingOutcomeKind::FailedDepositPending,
        ),
    ];
    let origin_tag = |kind| match kind {
        WithdrawalOriginKind::User => 1,
        WithdrawalOriginKind::FailedDeposit => 2,
    };
    let processing_tag = |kind| match kind {
        WithdrawalProcessingOutcomeKind::UserDelivered => 1,
        WithdrawalProcessingOutcomeKind::UserBounced => 2,
        WithdrawalProcessingOutcomeKind::FailedDepositPaid => 3,
        WithdrawalProcessingOutcomeKind::FailedDepositPending => 4,
    };
    assert_eq!(set(processing.map(|case| origin_tag(case.0))), set(1..=2));
    assert_eq!(
        set(processing.map(|case| processing_tag(case.1))),
        set(1..=4)
    );
    for (index, (origin, outcome)) in processing.into_iter().enumerate() {
        let withdrawal_index = u64::try_from(index).unwrap();
        let bytes = expected(0x3e, |bytes| {
            bytes.u64(withdrawal_index);
            bytes.u8(origin_tag(origin));
            bytes.u8(processing_tag(outcome));
        });
        let error = ModelError::WithdrawalProcessingOutcomeMismatch {
            withdrawal_index,
            expected: origin,
            actual: outcome,
        };
        assert_golden(model(&error).unwrap(), &bytes);
    }
}
