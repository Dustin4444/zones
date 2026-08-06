use crate::{
    check::finding::{TempoAdvancedExpectation, TempoBlockFinalizedExpectation},
    model::{
        adapter::{
            ObservedBatchFinalized, ObservedDepositOutcome, ObservedTempoAdvanced,
            ObservedTempoBlockFinalized, ObservedTokenEnabled, ObservedWithdrawalRequested,
            ObservedZoneOperation,
        },
        output::{
            ExpectedBatchFinalized, ExpectedDepositOutcome, ExpectedRefundClaim,
            ExpectedTokenEnable, ExpectedWithdrawalRequested, ExpectedZoneOperation,
        },
        ownership::{BatchId, WithdrawalId},
    },
    store::error::StoreResult,
};

use super::{Canonical, FindingSummary, encode_position};

pub(in super::super) fn tempo_block_finalized_expectation(
    output: TempoBlockFinalizedExpectation,
) -> StoreResult<FindingSummary> {
    let mut encoder = Canonical::tagged(1);
    encoder.hash(output.block_hash());
    encoder.u64(output.block_number());
    encoder.hash(output.state_root());
    encoder.finish()
}

pub(in super::super) fn observed_tempo_block_finalized(
    output: &ObservedTempoBlockFinalized,
) -> StoreResult<FindingSummary> {
    let mut encoder = Canonical::tagged(1);
    encode_position!(&mut encoder, output.position())?;
    encoder.hash(output.block_hash());
    encoder.u64(output.block_number());
    encoder.hash(output.state_root());
    encoder.finish()
}

pub(in super::super) fn expected_token_enable(
    output: &ExpectedTokenEnable,
) -> StoreResult<FindingSummary> {
    let mut encoder = Canonical::tagged(1);
    token_enable(
        &mut encoder,
        output.token(),
        output.name(),
        output.symbol(),
        output.currency(),
    )?;
    encoder.finish()
}

pub(in super::super) fn observed_token_enable(
    output: &ObservedTokenEnabled,
) -> StoreResult<FindingSummary> {
    let mut encoder = Canonical::tagged(1);
    encode_position!(&mut encoder, output.position())?;
    token_enable(
        &mut encoder,
        output.token(),
        output.name(),
        output.symbol(),
        output.currency(),
    )?;
    encoder.finish()
}

pub(in super::super) fn expected_deposit_outcome(
    output: &ExpectedDepositOutcome,
) -> StoreResult<FindingSummary> {
    let encoder = match output {
        ExpectedDepositOutcome::OrdinaryMinted(output) => {
            let mut encoder = Canonical::tagged(1);
            encoder.hash(output.deposit_hash());
            encoder.address(output.sender());
            encoder.address(output.token());
            encoder.u128(output.amount());
            encoder
        }
        ExpectedDepositOutcome::OrdinaryFailed(output) => {
            let mut encoder = Canonical::tagged(2);
            expected_withdrawal(&mut encoder, output.first())?;
            let failure = output.second();
            encoder.hash(failure.deposit_hash());
            encoder.address(failure.sender());
            encoder.address(failure.token());
            encoder.u128(failure.amount());
            encoder
        }
        ExpectedDepositOutcome::WithdrawalBounceBackMinted(output) => {
            let mut encoder = Canonical::tagged(3);
            encoder.address(output.token());
            encoder.u128(output.amount());
            encoder
        }
        ExpectedDepositOutcome::WithdrawalBounceBackPending(output) => {
            let mut encoder = Canonical::tagged(4);
            encoder.address(output.token());
            encoder.u128(output.amount());
            encoder
        }
    };
    encoder.finish()
}

pub(in super::super) fn observed_deposit_outcome(
    output: &ObservedDepositOutcome,
) -> StoreResult<FindingSummary> {
    let encoder = match output {
        ObservedDepositOutcome::OrdinaryMinted(output) => {
            let mut encoder = Canonical::tagged(1);
            encode_position!(&mut encoder, output.position())?;
            encoder.hash(output.deposit_hash());
            encoder.address(output.sender());
            encoder.address(output.to());
            encoder.address(output.token());
            encoder.u128(output.amount());
            encoder.hash(output.memo());
            encoder
        }
        ObservedDepositOutcome::OrdinaryFailed {
            withdrawal,
            failure,
        } => {
            let mut encoder = Canonical::tagged(2);
            observed_withdrawal(&mut encoder, withdrawal)?;
            encode_position!(&mut encoder, failure.position())?;
            encoder.hash(failure.deposit_hash());
            encoder.address(failure.sender());
            encoder.address(failure.token());
            encoder.u128(failure.amount());
            encoder
        }
        ObservedDepositOutcome::WithdrawalBounceBackMinted(output) => {
            let mut encoder = Canonical::tagged(3);
            encode_position!(&mut encoder, output.position())?;
            encoder.address(output.zone_fallback_recipient());
            encoder.address(output.token());
            encoder.u128(output.amount());
            encoder
        }
        ObservedDepositOutcome::WithdrawalBounceBackPending(output) => {
            let mut encoder = Canonical::tagged(4);
            encode_position!(&mut encoder, output.position())?;
            encoder.address(output.zone_fallback_recipient());
            encoder.address(output.token());
            encoder.u128(output.amount());
            encoder
        }
    };
    encoder.finish()
}

pub(in super::super) fn tempo_advanced_expectation(
    output: TempoAdvancedExpectation,
) -> StoreResult<FindingSummary> {
    let mut encoder = Canonical::tagged(1);
    encoder.hash(output.block_hash());
    encoder.u64(output.block_number());
    encoder.u256(output.deposits_processed());
    encoder.hash(output.processed_deposit_hash());
    encoder.u64(output.processed_deposit_number());
    encoder.finish()
}

pub(in super::super) fn observed_tempo_advanced(
    output: &ObservedTempoAdvanced,
) -> StoreResult<FindingSummary> {
    let mut encoder = Canonical::tagged(1);
    encode_position!(&mut encoder, output.position())?;
    encoder.hash(output.tempo_block_hash());
    encoder.u64(output.tempo_block_number());
    encoder.u256(output.deposits_processed());
    encoder.hash(output.new_processed_deposit_queue_hash());
    encoder.u64(output.last_processed_deposit_number());
    encoder.finish()
}

pub(in super::super) fn expected_zone_operation(
    output: &ExpectedZoneOperation,
) -> StoreResult<FindingSummary> {
    let encoder = match output {
        ExpectedZoneOperation::WithdrawalRequested(output) => {
            let mut encoder = Canonical::tagged(1);
            expected_withdrawal(&mut encoder, output)?;
            encoder
        }
        ExpectedZoneOperation::RefundClaimed(output) => {
            let mut encoder = Canonical::tagged(2);
            expected_refund(&mut encoder, output);
            encoder
        }
    };
    encoder.finish()
}

pub(in super::super) fn observed_zone_operation(
    output: &ObservedZoneOperation,
) -> StoreResult<FindingSummary> {
    let encoder = match output {
        ObservedZoneOperation::WithdrawalRequested(output) => {
            let mut encoder = Canonical::tagged(1);
            observed_withdrawal(&mut encoder, output)?;
            encoder
        }
        ObservedZoneOperation::RefundClaimed(output) => {
            let mut encoder = Canonical::tagged(2);
            encode_position!(&mut encoder, output.position())?;
            encoder.address(output.recipient());
            encoder.address(output.token());
            encoder.u128(output.amount());
            encoder
        }
    };
    encoder.finish()
}

pub(in super::super) fn expected_batch_finalized(
    output: Option<ExpectedBatchFinalized>,
) -> StoreResult<FindingSummary> {
    let mut encoder = Canonical::tagged(1);
    encoder.option(output, |encoder, output| {
        batch_id(encoder, output.batch());
        encoder.hash(output.withdrawal_queue_hash());
        Ok(())
    })?;
    encoder.finish()
}

pub(in super::super) fn observed_batch_finalized(
    output: Option<&ObservedBatchFinalized>,
) -> StoreResult<FindingSummary> {
    let mut encoder = Canonical::tagged(1);
    encoder.option(output, |encoder, output| {
        encode_position!(encoder, output.position())?;
        encoder.hash(output.withdrawal_queue_hash());
        encoder.u64(output.withdrawal_batch_index());
        Ok(())
    })?;
    encoder.finish()
}

fn expected_withdrawal(
    encoder: &mut Canonical,
    output: &ExpectedWithdrawalRequested,
) -> StoreResult<()> {
    withdrawal_id(encoder, output.withdrawal());
    encoder.address(output.sender());
    encoder.address(output.token());
    encoder.address(output.to());
    encoder.u128(output.amount());
    encoder.u128(output.fee());
    encoder.hash(output.memo());
    encoder.u64(output.gas_limit());
    encoder.u64(output.fallback_nonce());
    encoder.bytes(output.data())?;
    encoder.bytes(output.reveal_to())
}

fn observed_withdrawal(
    encoder: &mut Canonical,
    output: &ObservedWithdrawalRequested,
) -> StoreResult<()> {
    encode_position!(encoder, output.position())?;
    encoder.u64(output.withdrawal_index());
    encoder.address(output.sender());
    encoder.address(output.token());
    encoder.address(output.to());
    encoder.u128(output.amount());
    encoder.u128(output.fee());
    encoder.hash(output.memo());
    encoder.u64(output.gas_limit());
    encoder.u64(output.fallback_nonce());
    encoder.bytes(output.data())?;
    encoder.bytes(output.reveal_to())
}

fn expected_refund(encoder: &mut Canonical, output: &ExpectedRefundClaim) {
    encoder.address(output.recipient());
    encoder.address(output.token());
    encoder.u128(output.amount());
}

fn token_enable(
    encoder: &mut Canonical,
    token: alloy_primitives::Address,
    name: &str,
    symbol: &str,
    currency: &str,
) -> StoreResult<()> {
    encoder.address(token);
    encoder.str(name)?;
    encoder.str(symbol)?;
    encoder.str(currency)
}

fn withdrawal_id(encoder: &mut Canonical, id: WithdrawalId) {
    encoder.u32(id.zone_id);
    encoder.u64(id.withdrawal_index);
}

fn batch_id(encoder: &mut Canonical, id: BatchId) {
    encoder.u32(id.zone_id);
    encoder.u64(id.withdrawal_batch_index.get());
}
