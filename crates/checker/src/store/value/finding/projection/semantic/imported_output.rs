use crate::{
    model::{
        adapter::{
            ObservedDepositAppend, ObservedDepositRefund, ObservedImportedOutput,
            ObservedProcessedWithdrawal, ObservedSubmittedBatch, ObservedWithdrawalProcessed,
            ObservedWithdrawalProcessing,
        },
        output::{
            ExpectedBatchSubmission, ExpectedDepositAppend, ExpectedDepositRefund,
            ExpectedImportedTempoOperation, ExpectedProcessedWithdrawal, ExpectedRefundClaim,
            ExpectedWithdrawalBounceBackAppend, ExpectedWithdrawalProcessed,
            ExpectedWithdrawalProcessing,
        },
        ownership::{BatchId, DepositId, WithdrawalId},
    },
    store::error::StoreResult,
};

use super::{Canonical, FindingSummary, encode_position};

pub(in super::super) fn expected_imported_output(
    output: &ExpectedImportedTempoOperation,
) -> StoreResult<FindingSummary> {
    let encoder = match output {
        ExpectedImportedTempoOperation::DepositAppended(output) => {
            let mut encoder = Canonical::tagged(1);
            expected_deposit_append(&mut encoder, output);
            encoder
        }
        ExpectedImportedTempoOperation::BatchSubmitted(output) => {
            let mut encoder = Canonical::tagged(2);
            expected_batch_submission(&mut encoder, output);
            encoder
        }
        ExpectedImportedTempoOperation::WithdrawalsProcessed(output) => {
            let mut encoder = Canonical::tagged(3);
            expected_withdrawal_processing(&mut encoder, output)?;
            encoder
        }
        ExpectedImportedTempoOperation::RefundClaimed(output) => {
            let mut encoder = Canonical::tagged(4);
            expected_refund_claim(&mut encoder, output);
            encoder
        }
    };
    encoder.finish()
}

pub(in super::super) fn observed_imported_output(
    output: &ObservedImportedOutput,
) -> StoreResult<FindingSummary> {
    let encoder = match output {
        ObservedImportedOutput::DepositAppended(output) => {
            let mut encoder = Canonical::tagged(1);
            observed_deposit_append(&mut encoder, *output)?;
            encoder
        }
        ObservedImportedOutput::BatchSubmitted(output) => {
            let mut encoder = Canonical::tagged(2);
            observed_batch_submission(&mut encoder, *output)?;
            encoder
        }
        ObservedImportedOutput::WithdrawalsProcessed(output) => {
            let mut encoder = Canonical::tagged(3);
            observed_withdrawal_processing(&mut encoder, output)?;
            encoder
        }
        ObservedImportedOutput::RefundClaimed(output) => {
            let mut encoder = Canonical::tagged(4);
            encode_position!(&mut encoder, output.position())?;
            encoder.address(output.recipient());
            encoder.address(output.token());
            encoder.u128(output.amount());
            encoder
        }
    };
    encoder.finish()
}

fn expected_deposit_append(encoder: &mut Canonical, output: &ExpectedDepositAppend) {
    deposit_id(encoder, output.id());
    encoder.hash(output.queue_hash());
}

fn expected_batch_submission(encoder: &mut Canonical, output: &ExpectedBatchSubmission) {
    batch_id(encoder, output.batch());
    encoder.u256(output.withdrawal_queue_index());
    encoder.hash(output.next_processed_deposit_queue_hash());
    encoder.hash(output.next_block_hash());
    encoder.hash(output.withdrawal_queue_hash());
    encoder.u64(output.last_processed_deposit_number());
}

fn expected_withdrawal_processing(
    encoder: &mut Canonical,
    output: &ExpectedWithdrawalProcessing,
) -> StoreResult<()> {
    encoder.usize(output.members().len())?;
    for member in output.members() {
        match member {
            ExpectedProcessedWithdrawal::UserDelivered(output) => {
                encoder.u8(1);
                encoder.usize(output.callback_deposit_appends().len())?;
                for append in output.callback_deposit_appends() {
                    expected_deposit_append(encoder, append);
                }
                expected_withdrawal_processed(encoder, output.processed());
            }
            ExpectedProcessedWithdrawal::UserBounced(output) => {
                encoder.u8(2);
                expected_bounce_append(encoder, output.first());
                expected_withdrawal_processed(encoder, output.second());
            }
            ExpectedProcessedWithdrawal::FailedDepositPaid(output) => {
                encoder.u8(3);
                expected_deposit_refund(encoder, output);
            }
            ExpectedProcessedWithdrawal::FailedDepositPending(output) => {
                encoder.u8(4);
                expected_deposit_refund(encoder, output);
            }
        }
    }
    Ok(())
}

fn expected_bounce_append(encoder: &mut Canonical, output: ExpectedWithdrawalBounceBackAppend) {
    let deposit = output.deposit();
    encoder.address(deposit.token());
    encoder.u64(deposit.fallback_nonce().get());
    encoder.u128(deposit.amount().get());
    expected_deposit_append(encoder, &output.append());
}

fn expected_withdrawal_processed(encoder: &mut Canonical, output: ExpectedWithdrawalProcessed) {
    withdrawal_id(encoder, output.withdrawal());
    encoder.address(output.to());
    encoder.hash(output.sender_tag());
    encoder.address(output.token());
    encoder.u128(output.amount());
    encoder.bool(output.callback_success());
}

fn expected_deposit_refund(encoder: &mut Canonical, output: &ExpectedDepositRefund) {
    deposit_id(encoder, output.failed_deposit());
    encoder.address(output.recipient());
    encoder.address(output.token());
    encoder.u128(output.amount());
    encoder.u128(output.bounceback_fee());
}

fn expected_refund_claim(encoder: &mut Canonical, output: &ExpectedRefundClaim) {
    encoder.address(output.recipient());
    encoder.address(output.token());
    encoder.u128(output.amount());
}

fn observed_deposit_append(
    encoder: &mut Canonical,
    output: ObservedDepositAppend,
) -> StoreResult<()> {
    encode_position!(encoder, output.position())?;
    encoder.hash(output.queue_hash());
    encoder.u64(output.deposit_number());
    Ok(())
}

fn observed_batch_submission(
    encoder: &mut Canonical,
    output: ObservedSubmittedBatch,
) -> StoreResult<()> {
    encode_position!(encoder, output.position())?;
    encoder.u64(output.withdrawal_batch_index());
    encoder.u256(output.withdrawal_queue_index());
    encoder.hash(output.next_processed_deposit_queue_hash());
    encoder.hash(output.next_block_hash());
    encoder.hash(output.withdrawal_queue_hash());
    encoder.u64(output.last_processed_deposit_number());
    Ok(())
}

fn observed_withdrawal_processing(
    encoder: &mut Canonical,
    output: &ObservedWithdrawalProcessing,
) -> StoreResult<()> {
    encoder.usize(output.transaction_index())?;
    encoder.hash(output.transaction_hash());
    encoder.usize(output.members().len())?;
    for member in output.members() {
        match member {
            ObservedProcessedWithdrawal::UserDelivered(output) => {
                encoder.u8(1);
                encoder.usize(output.callback_deposits().len())?;
                for append in output.callback_deposits() {
                    observed_deposit_append(encoder, *append)?;
                }
                observed_withdrawal_processed(encoder, output.processed())?;
            }
            ObservedProcessedWithdrawal::UserBounced(output) => {
                encoder.u8(2);
                let append = output.append();
                encode_position!(encoder, append.position())?;
                encoder.hash(append.queue_hash());
                encoder.u64(append.fallback_nonce());
                encoder.address(append.token());
                encoder.u128(append.amount());
                encoder.u64(append.deposit_number());
                observed_withdrawal_processed(encoder, output.processed())?;
            }
            ObservedProcessedWithdrawal::FailedDepositPaid(output) => {
                encoder.u8(3);
                observed_deposit_refund(encoder, *output)?;
            }
            ObservedProcessedWithdrawal::FailedDepositPending(output) => {
                encoder.u8(4);
                observed_deposit_refund(encoder, *output)?;
            }
        }
    }
    Ok(())
}

fn observed_withdrawal_processed(
    encoder: &mut Canonical,
    output: ObservedWithdrawalProcessed,
) -> StoreResult<()> {
    encode_position!(encoder, output.position())?;
    encoder.address(output.to());
    encoder.hash(output.sender_tag());
    encoder.address(output.token());
    encoder.u128(output.amount());
    encoder.bool(output.callback_success());
    Ok(())
}

fn observed_deposit_refund(
    encoder: &mut Canonical,
    output: ObservedDepositRefund,
) -> StoreResult<()> {
    encode_position!(encoder, output.position())?;
    encoder.address(output.recipient());
    encoder.address(output.token());
    encoder.u128(output.amount());
    encoder.u128(output.bounceback_fee());
    Ok(())
}

fn deposit_id(encoder: &mut Canonical, id: DepositId) {
    encoder.address(id.portal);
    encoder.u64(id.deposit_number.get());
}

fn withdrawal_id(encoder: &mut Canonical, id: WithdrawalId) {
    encoder.u32(id.zone_id);
    encoder.u64(id.withdrawal_index);
}

fn batch_id(encoder: &mut Canonical, id: BatchId) {
    encoder.u32(id.zone_id);
    encoder.u64(id.withdrawal_batch_index.get());
}
