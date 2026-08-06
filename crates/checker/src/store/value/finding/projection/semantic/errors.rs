use crate::{
    model::{
        adapter::{
            DepositInputKind, ImportedEventKind, ImportedProjectionError, ZoneEventKind,
            ZoneProjectionError,
        },
        encoding::WithdrawalDataError,
    },
    observe::{AuthenticatedDataEvidence, PortalCallError, PortalCallFamily},
    store::{
        error::StoreResult,
        value::finding::leaf::{
            StoredImportedProjectionError, StoredPortalCallError, StoredZoneProjectionError,
        },
    },
};

use super::{Canonical, FindingSummary, encode_position};

pub(in super::super) const fn malformed_authenticated_data(
    evidence: AuthenticatedDataEvidence,
) -> FindingSummary {
    FindingSummary::new(evidence.length(), evidence.hash())
}

pub(in super::super) fn malformed_event(event: &'static str) -> StoreResult<FindingSummary> {
    // `event` is checker-owned static vocabulary; `reason` may be an Alloy
    // Display string and therefore must not become durable identity.
    let mut encoder = Canonical::tagged(1);
    encoder.str(event)?;
    encoder.finish()
}

pub(in super::super) fn portal_call(error: &PortalCallError) -> StoreResult<FindingSummary> {
    let tag = StoredPortalCallError::from(error).wire_tag();
    let encoder = match error {
        PortalCallError::UnsupportedNestedPortalCall {
            transaction_hash,
            target,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.hash(*transaction_hash);
            encoder.option(*target, |encoder, target| {
                encoder.address(target);
                Ok(())
            })?;
            encoder
        }
        PortalCallError::ConflictingFamilies { transaction_hash } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.hash(*transaction_hash);
            encoder
        }
        PortalCallError::FamilyMismatch {
            transaction_hash,
            expected,
            actual,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.hash(*transaction_hash);
            encoder.u8(portal_family(*expected));
            encoder.u8(portal_family(*actual));
            encoder
        }
        PortalCallError::EmptyProcessWithOutcomes { transaction_hash } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.hash(*transaction_hash);
            encoder
        }
    };
    encoder.finish()
}

pub(in super::super) fn imported_projection(
    error: &ImportedProjectionError,
) -> StoreResult<FindingSummary> {
    use ImportedProjectionError::*;
    let tag = StoredImportedProjectionError::from(error).wire_tag();
    let encoder = match error {
        MissingBaseFee => Canonical::tagged(tag),
        BlockHashMismatch { expected, actual } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.hash(*expected);
            encoder.hash(*actual);
            encoder
        }
        BlockNumberMismatch { expected, actual } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u64(*expected);
            encoder.u64(*actual);
            encoder
        }
        TransactionOrderMismatch { previous, next } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*previous)?;
            encoder.usize(*next)?;
            encoder
        }
        OutcomeCoordinateMismatch {
            transaction_index,
            transaction_hash,
            event_transaction_index,
            event_transaction_hash,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*transaction_index)?;
            encoder.hash(*transaction_hash);
            encoder.usize(*event_transaction_index)?;
            encoder.hash(*event_transaction_hash);
            encoder
        }
        InvalidCreationGrammar { transaction_index } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*transaction_index)?;
            encoder
        }
        InvalidSubmitBatchGrammar { transaction_index } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*transaction_index)?;
            encoder
        }
        DirectCallRequired {
            transaction_index,
            event,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*transaction_index)?;
            encoder.u8(imported_event(*event));
            encoder
        }
        UnexpectedEvent {
            transaction_index,
            event,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*transaction_index)?;
            encoder.u8(imported_event(*event));
            encoder
        }
        InvalidDepositCiphertextLength {
            block_log_index,
            actual,
            expected,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*block_log_index)?;
            encoder.usize(*actual)?;
            encoder.usize(*expected)?;
            encoder
        }
        InvalidDepositKeyParity {
            block_log_index,
            actual,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*block_log_index)?;
            encoder.u8(*actual);
            encoder
        }
        InvalidWithdrawalPreimage {
            transaction_index,
            member_index,
            source,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*transaction_index)?;
            encoder.usize(*member_index)?;
            encode_withdrawal_data(&mut encoder, source)?;
            encoder
        }
        MissingWithdrawalOutcome {
            transaction_index,
            member_index,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*transaction_index)?;
            encoder.usize(*member_index)?;
            encoder
        }
        UnexpectedWithdrawalOutcome {
            transaction_index,
            member_index,
            event,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*transaction_index)?;
            encoder.usize(*member_index)?;
            encoder.u8(imported_event(*event));
            encoder
        }
        WithdrawalCallbackSuccessMismatch {
            transaction_index,
            member_index,
            expected,
            actual,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*transaction_index)?;
            encoder.usize(*member_index)?;
            encoder.bool(*expected);
            encoder.bool(*actual);
            encoder
        }
        ExtraWithdrawalOutcomes {
            transaction_index,
            remaining,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*transaction_index)?;
            encoder.usize(*remaining)?;
            encoder
        }
    };
    encoder.finish()
}

pub(in super::super) fn zone_projection(
    error: &ZoneProjectionError,
) -> StoreResult<FindingSummary> {
    use ZoneProjectionError::*;
    let tag = StoredZoneProjectionError::from(error).wire_tag();
    let encoder = match error {
        MissingTempoBlockFinalized => Canonical::tagged(tag),
        ReorderedTempoBlockFinalized { actual, position } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u8(zone_event(*actual));
            encode_position!(&mut encoder, *position)?;
            encoder
        }
        MissingTokenEnabled { index } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*index)?;
            encoder
        }
        ReorderedTokenEnabled {
            index,
            actual,
            position,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*index)?;
            encoder.u8(zone_event(*actual));
            encode_position!(&mut encoder, *position)?;
            encoder
        }
        MissingDepositOutcome {
            index,
            deposit_kind,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*index)?;
            encoder.u8(deposit_input(*deposit_kind));
            encoder
        }
        ReorderedDepositOutcome {
            index,
            deposit_kind,
            actual,
            position,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*index)?;
            encoder.u8(deposit_input(*deposit_kind));
            encoder.u8(zone_event(*actual));
            encode_position!(&mut encoder, *position)?;
            encoder
        }
        MissingDepositFailed { index } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*index)?;
            encoder
        }
        ReorderedDepositFailed {
            index,
            actual,
            position,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*index)?;
            encoder.u8(zone_event(*actual));
            encode_position!(&mut encoder, *position)?;
            encoder
        }
        MissingTempoAdvanced => Canonical::tagged(tag),
        ReorderedTempoAdvanced { actual, position } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u8(zone_event(*actual));
            encode_position!(&mut encoder, *position)?;
            encoder
        }
        ExtraAdvanceEvent { actual, position } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u8(zone_event(*actual));
            encode_position!(&mut encoder, *position)?;
            encoder
        }
        AdvanceTransactionHashMismatch { expected, position } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.hash(*expected);
            encode_position!(&mut encoder, *position)?;
            encoder
        }
        InvalidDepositKeyParity { index, actual } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*index)?;
            encoder.u8(*actual);
            encoder
        }
        InvalidDepositCiphertextLength {
            index,
            actual,
            expected,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*index)?;
            encoder.usize(*actual)?;
            encoder.usize(*expected)?;
            encoder
        }
        InvalidBounceBackRecipient { index, recipient } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*index)?;
            encoder.address(*recipient);
            encoder
        }
        ZeroBounceBackNonce { index } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*index)?;
            encoder
        }
        ZeroBounceBackAmount { index } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*index)?;
            encoder
        }
        InvalidWithdrawalRequest {
            transaction_index,
            source,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*transaction_index)?;
            encode_withdrawal_data(&mut encoder, source)?;
            encoder
        }
        UnexpectedPostAdvanceEvent { actual, position } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u8(zone_event(*actual));
            encode_position!(&mut encoder, *position)?;
            encoder
        }
        BatchFinalizedWithoutEnvelope { position } => {
            let mut encoder = Canonical::tagged(tag);
            encode_position!(&mut encoder, *position)?;
            encoder
        }
        BatchFinalizedWrongTransaction { expected, position } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.hash(*expected);
            encode_position!(&mut encoder, *position)?;
            encoder
        }
        MissingBatchFinalized { transaction_hash } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.hash(*transaction_hash);
            encoder
        }
        ReorderedBatchFinalized { actual, position } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u8(zone_event(*actual));
            encode_position!(&mut encoder, *position)?;
            encoder
        }
        ExtraFinalizationEvent { actual, position } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u8(zone_event(*actual));
            encode_position!(&mut encoder, *position)?;
            encoder
        }
        UnsupportedDepositKind { index } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*index)?;
            encoder
        }
    };
    encoder.finish()
}

pub(super) fn encode_withdrawal_data(
    encoder: &mut Canonical,
    error: &WithdrawalDataError,
) -> StoreResult<()> {
    use WithdrawalDataError::*;
    match error {
        ZeroAmount => encoder.u8(1),
        ZeroTransactionHash => encoder.u8(2),
        GasLimitTooHigh { actual, maximum } => {
            encoder.u8(3);
            encoder.u64(*actual);
            encoder.u64(*maximum);
        }
        CallbackDataTooLong { actual, maximum } => {
            encoder.u8(4);
            encoder.usize(*actual)?;
            encoder.usize(*maximum)?;
        }
        InvalidRevealToLength { actual, expected } => {
            encoder.u8(5);
            encoder.usize(*actual)?;
            encoder.usize(*expected)?;
        }
        InvalidRevealToPrefix { actual } => {
            encoder.u8(6);
            encoder.u8(*actual);
        }
        InvalidEncryptedSenderLength { actual, expected } => {
            encoder.u8(7);
            encoder.usize(*actual)?;
            encoder.usize(*expected)?;
        }
        InvalidAuthenticatedEncryptedSenderLength {
            actual,
            nonempty_expected,
        } => {
            encoder.u8(8);
            encoder.usize(*actual)?;
            encoder.usize(*nonempty_expected)?;
        }
    }
    Ok(())
}

fn portal_family(family: PortalCallFamily) -> u8 {
    match family {
        PortalCallFamily::SubmitBatch => 1,
        PortalCallFamily::ProcessWithdrawals => 2,
    }
}

fn imported_event(event: ImportedEventKind) -> u8 {
    use ImportedEventKind::*;
    match event {
        DepositMade => 1,
        TokenEnabled => 2,
        BatchSubmitted => 3,
        WithdrawalProcessed => 4,
        WithdrawalBounceBack => 5,
        DepositBounceBack => 6,
        DepositBounceBackPending => 7,
        RefundClaimed => 8,
        BouncebackGasUpdated => 9,
        FactoryZoneCreated => 10,
        KnownNonModel => 11,
    }
}

fn deposit_input(kind: DepositInputKind) -> u8 {
    use DepositInputKind::*;
    match kind {
        Ordinary => 1,
        WithdrawalBounceBack => 2,
    }
}

fn zone_event(event: ZoneEventKind) -> u8 {
    use ZoneEventKind::*;
    match event {
        TempoBlockFinalized => 1,
        TokenEnabled => 2,
        DepositProcessed => 3,
        DepositFailed => 4,
        WithdrawalBounceBackProcessed => 5,
        WithdrawalBounceBackPending => 6,
        TempoAdvanced => 7,
        WithdrawalRequested => 8,
        RefundClaimed => 9,
        BatchFinalized => 10,
        TempoGasRateUpdated => 11,
        MaxWithdrawalsPerBlockUpdated => 12,
    }
}
