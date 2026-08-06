//! Portal event comparisons at the authenticated imported-Tempo cut.

use crate::{
    check::finding::ImportedOutputFinding,
    model::{
        adapter::{
            ObservedDepositAppend, ObservedDepositRefund, ObservedImportedOutput,
            ObservedProcessedWithdrawal, ObservedSubmittedBatch, ObservedWithdrawalProcessed,
            ObservedWithdrawalProcessing,
        },
        output::{
            ExpectedBatchSubmission, ExpectedDepositAppend, ExpectedDepositRefund,
            ExpectedImportedTempoBlock, ExpectedImportedTempoOperation,
            ExpectedProcessedWithdrawal, ExpectedWithdrawalProcessed, ExpectedWithdrawalProcessing,
        },
    },
};

pub(in crate::check) fn reconcile_imported_outputs(
    expected: &ExpectedImportedTempoBlock,
    actual: &[ObservedImportedOutput],
) -> Result<(), ImportedOutputFinding> {
    if expected.operations().len() != actual.len() {
        return Err(ImportedOutputFinding::Count {
            expected: expected.operations().len(),
            actual: actual.len(),
        });
    }

    for (index, (expected, actual)) in expected.operations().iter().zip(actual).enumerate() {
        if !same_operation(expected, actual) {
            return Err(ImportedOutputFinding::Mismatch {
                index,
                expected: Box::new(expected.clone()),
                actual: Box::new(actual.clone()),
            });
        }
    }
    Ok(())
}

fn same_operation(
    expected: &ExpectedImportedTempoOperation,
    actual: &ObservedImportedOutput,
) -> bool {
    match (expected, actual) {
        (
            ExpectedImportedTempoOperation::DepositAppended(expected),
            ObservedImportedOutput::DepositAppended(actual),
        ) => same_deposit_append(expected, *actual),
        (
            ExpectedImportedTempoOperation::BatchSubmitted(expected),
            ObservedImportedOutput::BatchSubmitted(actual),
        ) => same_batch_submission(expected, *actual),
        (
            ExpectedImportedTempoOperation::WithdrawalsProcessed(expected),
            ObservedImportedOutput::WithdrawalsProcessed(actual),
        ) => same_withdrawal_processing(expected, actual),
        (
            ExpectedImportedTempoOperation::RefundClaimed(expected),
            ObservedImportedOutput::RefundClaimed(actual),
        ) => {
            expected.recipient() == actual.recipient()
                && expected.token() == actual.token()
                && expected.amount() == actual.amount()
        }
        _ => false,
    }
}

fn same_deposit_append(expected: &ExpectedDepositAppend, actual: ObservedDepositAppend) -> bool {
    expected.id().deposit_number.get() == actual.deposit_number()
        && expected.queue_hash() == actual.queue_hash()
}

fn same_batch_submission(
    expected: &ExpectedBatchSubmission,
    actual: ObservedSubmittedBatch,
) -> bool {
    expected.batch().withdrawal_batch_index.get() == actual.withdrawal_batch_index()
        && expected.withdrawal_queue_index() == actual.withdrawal_queue_index()
        && expected.next_processed_deposit_queue_hash()
            == actual.next_processed_deposit_queue_hash()
        && expected.next_block_hash() == actual.next_block_hash()
        && expected.withdrawal_queue_hash() == actual.withdrawal_queue_hash()
        && expected.last_processed_deposit_number() == actual.last_processed_deposit_number()
}

fn same_withdrawal_processing(
    expected: &ExpectedWithdrawalProcessing,
    actual: &ObservedWithdrawalProcessing,
) -> bool {
    expected.members().len() == actual.members().len()
        && expected
            .members()
            .iter()
            .zip(actual.members())
            .all(|(expected, actual)| same_processed_withdrawal(expected, actual))
}

fn same_processed_withdrawal(
    expected: &ExpectedProcessedWithdrawal,
    actual: &ObservedProcessedWithdrawal,
) -> bool {
    match (expected, actual) {
        (
            ExpectedProcessedWithdrawal::UserDelivered(expected),
            ObservedProcessedWithdrawal::UserDelivered(actual),
        ) => {
            expected.callback_deposit_appends().len() == actual.callback_deposits().len()
                && expected
                    .callback_deposit_appends()
                    .iter()
                    .zip(actual.callback_deposits())
                    .all(|(expected, actual)| same_deposit_append(expected, *actual))
                && same_withdrawal_processed(&expected.processed(), actual.processed())
        }
        (
            ExpectedProcessedWithdrawal::UserBounced(expected),
            ObservedProcessedWithdrawal::UserBounced(actual),
        ) => {
            let expected_append = expected.first();
            let expected_deposit = expected_append.deposit();
            let actual_append = actual.append();
            expected_deposit.fallback_nonce().get() == actual_append.fallback_nonce()
                && expected_deposit.token() == actual_append.token()
                && expected_deposit.amount().get() == actual_append.amount()
                && expected_append.append().id().deposit_number.get()
                    == actual_append.deposit_number()
                && expected_append.append().queue_hash() == actual_append.queue_hash()
                && same_withdrawal_processed(&expected.second(), actual.processed())
        }
        (
            ExpectedProcessedWithdrawal::FailedDepositPaid(expected),
            ObservedProcessedWithdrawal::FailedDepositPaid(actual),
        )
        | (
            ExpectedProcessedWithdrawal::FailedDepositPending(expected),
            ObservedProcessedWithdrawal::FailedDepositPending(actual),
        ) => same_deposit_refund(expected, *actual),
        _ => false,
    }
}

fn same_withdrawal_processed(
    expected: &ExpectedWithdrawalProcessed,
    actual: ObservedWithdrawalProcessed,
) -> bool {
    expected.to() == actual.to()
        && expected.sender_tag() == actual.sender_tag()
        && expected.token() == actual.token()
        && expected.amount() == actual.amount()
        && expected.callback_success() == actual.callback_success()
}

fn same_deposit_refund(expected: &ExpectedDepositRefund, actual: ObservedDepositRefund) -> bool {
    expected.recipient() == actual.recipient()
        && expected.token() == actual.token()
        && expected.amount() == actual.amount()
        && expected.bounceback_fee() == actual.bounceback_fee()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use alloy_primitives::{Address, B256};

    use super::*;
    use crate::model::ownership::DepositId;

    #[test]
    fn imported_output_mismatch_preserves_concrete_operation_types() {
        let expected_hash = B256::repeat_byte(0x11);
        let actual_hash = B256::repeat_byte(0x22);
        let mut expected = ExpectedImportedTempoBlock::default();
        expected.push_deposit_append_for_test(
            DepositId {
                portal: Address::repeat_byte(0x33),
                deposit_number: NonZeroU64::new(7).unwrap(),
            },
            expected_hash,
        );
        let actual = [ObservedImportedOutput::deposit_append_for_test(
            actual_hash,
            7,
        )];

        let error = reconcile_imported_outputs(&expected, &actual).unwrap_err();
        let ImportedOutputFinding::Mismatch {
            index,
            expected,
            actual,
        } = error
        else {
            panic!("expected a concrete operation mismatch")
        };
        assert_eq!(index, 0);
        assert!(matches!(
            (*expected, *actual),
            (
                ExpectedImportedTempoOperation::DepositAppended(expected),
                ObservedImportedOutput::DepositAppended(actual),
            ) if expected.queue_hash() == expected_hash && actual.queue_hash() == actual_hash
        ));
    }
}
