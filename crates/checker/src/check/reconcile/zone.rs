//! Native Zone event comparisons against independently derived expectations.

use alloy_consensus::BlockHeader as _;
use alloy_primitives::U256;

use crate::{
    check::finding::{TempoAdvancedExpectation, TempoBlockFinalizedExpectation, ZoneOutputFinding},
    model::{
        adapter::{
            ObservedBatchFinalized, ObservedDepositFailed, ObservedDepositOutcome,
            ObservedDepositProcessed, ObservedRefundClaimed, ObservedWithdrawalBounceBackPending,
            ObservedWithdrawalBounceBackProcessed, ObservedWithdrawalRequested,
            ObservedZoneOperation, ObservedZoneOutputs,
        },
        output::{
            ExpectedBatchFinalized, ExpectedDepositOutcome, ExpectedDepositProcessed,
            ExpectedOrdinaryDepositFailure, ExpectedOutputs, ExpectedRefundClaim,
            ExpectedTokenEnable, ExpectedWithdrawalBounceBack, ExpectedWithdrawalRequested,
            ExpectedZoneOperation,
        },
    },
    observe::ImportedTempoHeader,
};

pub(in crate::check) fn reconcile_zone_outputs(
    imported: &ImportedTempoHeader,
    expected: &ExpectedOutputs,
    actual: &ObservedZoneOutputs,
) -> Result<(), ZoneOutputFinding> {
    let finalized = actual.tempo_block_finalized();
    let expected_finalized = TempoBlockFinalizedExpectation {
        block_hash: imported.hash(),
        block_number: imported.number(),
        state_root: imported.header().state_root(),
    };
    if finalized.block_hash() != expected_finalized.block_hash
        || finalized.block_number() != expected_finalized.block_number
        || finalized.state_root() != expected_finalized.state_root
    {
        return Err(ZoneOutputFinding::TempoBlockFinalized {
            expected: expected_finalized,
            actual: Box::new(finalized),
        });
    }

    let prefix = expected.zone_deposit_prefix();
    if prefix.token_enables().len() != actual.token_enables().len() {
        return Err(ZoneOutputFinding::TokenEnableCount {
            expected: prefix.token_enables().len(),
            actual: actual.token_enables().len(),
        });
    }
    for (index, (expected, actual)) in prefix
        .token_enables()
        .iter()
        .zip(actual.token_enables())
        .enumerate()
    {
        if !same_token_enable(expected, actual) {
            return Err(ZoneOutputFinding::TokenEnable {
                index,
                expected: Box::new(expected.clone()),
                actual: Box::new(actual.clone()),
            });
        }
    }

    if prefix.deposit_outcomes().len() != actual.deposit_outcomes().len() {
        return Err(ZoneOutputFinding::DepositOutcomeCount {
            expected: prefix.deposit_outcomes().len(),
            actual: actual.deposit_outcomes().len(),
        });
    }
    for (index, (expected, actual)) in prefix
        .deposit_outcomes()
        .iter()
        .zip(actual.deposit_outcomes())
        .enumerate()
    {
        if !same_deposit_outcome(expected, actual) {
            return Err(ZoneOutputFinding::DepositOutcome {
                index,
                expected: Box::new(expected.clone()),
                actual: Box::new(actual.clone()),
            });
        }
    }

    let advanced = actual.tempo_advanced();
    let expected_advanced = TempoAdvancedExpectation {
        block_hash: imported.hash(),
        block_number: imported.number(),
        deposits_processed: U256::from(prefix.deposits_processed()),
        processed_deposit_hash: prefix.processed_cursor().hash(),
        processed_deposit_number: prefix.processed_cursor().number(),
    };
    if advanced.tempo_block_hash() != expected_advanced.block_hash
        || advanced.tempo_block_number() != expected_advanced.block_number
        || advanced.deposits_processed() != expected_advanced.deposits_processed
        || advanced.new_processed_deposit_queue_hash() != expected_advanced.processed_deposit_hash
        || advanced.last_processed_deposit_number() != expected_advanced.processed_deposit_number
    {
        return Err(ZoneOutputFinding::TempoAdvanced {
            expected: expected_advanced,
            actual: Box::new(advanced),
        });
    }

    let zone = expected.zone_block();
    if zone.operations().len() != actual.operations().len() {
        return Err(ZoneOutputFinding::OperationCount {
            expected: zone.operations().len(),
            actual: actual.operations().len(),
        });
    }
    for (index, (expected, actual)) in zone
        .operations()
        .iter()
        .zip(actual.operations())
        .enumerate()
    {
        if !same_zone_operation(expected, actual) {
            return Err(ZoneOutputFinding::Operation {
                index,
                expected: Box::new(expected.clone()),
                actual: Box::new(actual.clone()),
            });
        }
    }
    let expected_batch = zone.finalized_batch();
    let actual_batch = actual.batch_finalized();
    if !same_batch_finalized(expected_batch, actual_batch) {
        return Err(ZoneOutputFinding::BatchFinalized {
            expected: expected_batch,
            actual: actual_batch.map(Box::new),
        });
    }
    Ok(())
}

fn same_token_enable(
    expected: &ExpectedTokenEnable,
    actual: &crate::model::adapter::ObservedTokenEnabled,
) -> bool {
    expected.token() == actual.token()
        && expected.name() == actual.name()
        && expected.symbol() == actual.symbol()
        && expected.currency() == actual.currency()
}

fn same_deposit_outcome(
    expected: &ExpectedDepositOutcome,
    actual: &ObservedDepositOutcome,
) -> bool {
    match (expected, actual) {
        (
            ExpectedDepositOutcome::OrdinaryMinted(expected),
            ObservedDepositOutcome::OrdinaryMinted(actual),
        ) => same_deposit_processed(expected, actual),
        (
            ExpectedDepositOutcome::OrdinaryFailed(expected),
            ObservedDepositOutcome::OrdinaryFailed {
                withdrawal,
                failure,
            },
        ) => same_ordinary_failure(expected, withdrawal, failure),
        (
            ExpectedDepositOutcome::WithdrawalBounceBackMinted(expected),
            ObservedDepositOutcome::WithdrawalBounceBackMinted(actual),
        ) => same_bounce_back_processed(expected, actual),
        (
            ExpectedDepositOutcome::WithdrawalBounceBackPending(expected),
            ObservedDepositOutcome::WithdrawalBounceBackPending(actual),
        ) => same_bounce_back_pending(expected, actual),
        _ => false,
    }
}

fn same_deposit_processed(
    expected: &ExpectedDepositProcessed,
    actual: &ObservedDepositProcessed,
) -> bool {
    expected.deposit_hash() == actual.deposit_hash()
        && expected.sender() == actual.sender()
        && expected.token() == actual.token()
        && expected.amount() == actual.amount()
}

fn same_ordinary_failure(
    expected: &ExpectedOrdinaryDepositFailure,
    withdrawal: &ObservedWithdrawalRequested,
    failure: &ObservedDepositFailed,
) -> bool {
    same_withdrawal_request(expected.first(), withdrawal)
        && expected.second().deposit_hash() == failure.deposit_hash()
        && expected.second().sender() == failure.sender()
        && expected.second().token() == failure.token()
        && expected.second().amount() == failure.amount()
}

fn same_bounce_back_processed(
    expected: &ExpectedWithdrawalBounceBack,
    actual: &ObservedWithdrawalBounceBackProcessed,
) -> bool {
    expected.token() == actual.token() && expected.amount() == actual.amount()
}

fn same_bounce_back_pending(
    expected: &ExpectedWithdrawalBounceBack,
    actual: &ObservedWithdrawalBounceBackPending,
) -> bool {
    expected.token() == actual.token() && expected.amount() == actual.amount()
}

fn same_zone_operation(expected: &ExpectedZoneOperation, actual: &ObservedZoneOperation) -> bool {
    match (expected, actual) {
        (
            ExpectedZoneOperation::WithdrawalRequested(expected),
            ObservedZoneOperation::WithdrawalRequested(actual),
        ) => same_withdrawal_request(expected, actual),
        (
            ExpectedZoneOperation::RefundClaimed(expected),
            ObservedZoneOperation::RefundClaimed(actual),
        ) => same_refund(expected, actual),
        _ => false,
    }
}

fn same_withdrawal_request(
    expected: &ExpectedWithdrawalRequested,
    actual: &ObservedWithdrawalRequested,
) -> bool {
    expected.withdrawal().withdrawal_index == actual.withdrawal_index()
        && expected.sender() == actual.sender()
        && expected.token() == actual.token()
        && expected.to() == actual.to()
        && expected.amount() == actual.amount()
        && expected.fee() == actual.fee()
        && expected.memo() == actual.memo()
        && expected.gas_limit() == actual.gas_limit()
        && expected.fallback_nonce() == actual.fallback_nonce()
        && expected.data() == actual.data()
        && expected.reveal_to() == actual.reveal_to()
}

fn same_refund(expected: &ExpectedRefundClaim, actual: &ObservedRefundClaimed) -> bool {
    expected.recipient() == actual.recipient()
        && expected.token() == actual.token()
        && expected.amount() == actual.amount()
}

fn same_batch_finalized(
    expected: Option<ExpectedBatchFinalized>,
    actual: Option<ObservedBatchFinalized>,
) -> bool {
    match (expected, actual) {
        (None, None) => true,
        (Some(expected), Some(actual)) => {
            expected.batch().withdrawal_batch_index.get() == actual.withdrawal_batch_index()
                && expected.withdrawal_queue_hash() == actual.withdrawal_queue_hash()
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use alloy_consensus::Header;
    use alloy_primitives::B256;
    use tempo_primitives::TempoHeader;

    use super::*;
    use crate::model::{adapter::ObservedZoneOutputs, output::ExpectedOutputs};

    #[test]
    fn zone_output_mismatch_preserves_the_commitment_family() {
        let state_root = B256::repeat_byte(0x11);
        let imported = ImportedTempoHeader::for_test(TempoHeader {
            inner: Header {
                number: 42,
                state_root,
                ..Default::default()
            },
            ..Default::default()
        });
        let actual_hash = B256::repeat_byte(0x22);
        let actual =
            ObservedZoneOutputs::empty_for_test(imported.hash(), imported.number(), state_root)
                .with_finalized_hash_for_test(actual_hash);

        let error = reconcile_zone_outputs(&imported, &ExpectedOutputs::empty_for_test(), &actual)
            .unwrap_err();
        assert!(matches!(
            error,
            ZoneOutputFinding::TempoBlockFinalized { expected, actual }
                if expected.block_hash == imported.hash()
                    && actual.block_hash() == actual_hash
        ));
    }
}
