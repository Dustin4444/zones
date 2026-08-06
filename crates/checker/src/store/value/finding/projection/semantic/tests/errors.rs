//! Golden vectors for observation and projection error summaries.

use alloy_primitives::{Address, B256};

use crate::{
    model::{
        adapter::{
            DepositInputKind, ImportedEventKind, ImportedProjectionError, ObservedZoneOutputs,
            ZoneEventKind, ZoneProjectionError,
        },
        encoding::WithdrawalDataError,
    },
    observe::{AuthenticatedDataEvidence, PortalCallError, PortalCallFamily},
};

use super::super::{
    imported_projection, malformed_authenticated_data, malformed_event, portal_call,
    zone_projection,
};
use super::{Golden, assert_golden};

fn expected(tag: u8, encode: impl FnOnce(&mut Golden)) -> Vec<u8> {
    let mut bytes = Golden::tagged(tag);
    encode(&mut bytes);
    bytes.finish()
}

#[test]
fn formatting_independent_observation_summaries_are_golden() {
    let evidence = b"authenticated bytes";
    assert_golden(
        malformed_authenticated_data(AuthenticatedDataEvidence::from_bytes(evidence)),
        evidence,
    );

    let event = malformed_event("TokenEnabled").unwrap();
    assert_golden(event, &expected(0x01, |bytes| bytes.bytes(b"TokenEnabled")));
}

#[test]
fn every_portal_call_branch_has_an_independent_golden_vector() {
    let transaction_hash = B256::repeat_byte(0x11);
    let target = Address::repeat_byte(0x22);
    let cases = [
        (
            PortalCallError::UnsupportedNestedPortalCall {
                transaction_hash,
                target: Some(target),
            },
            expected(0x01, |bytes| {
                bytes.hash(transaction_hash);
                bytes.u8(1);
                bytes.address(target);
            }),
        ),
        (
            PortalCallError::UnsupportedNestedPortalCall {
                transaction_hash,
                target: None,
            },
            expected(0x01, |bytes| {
                bytes.hash(transaction_hash);
                bytes.u8(0);
            }),
        ),
        (
            PortalCallError::ConflictingFamilies { transaction_hash },
            expected(0x02, |bytes| bytes.hash(transaction_hash)),
        ),
        (
            PortalCallError::FamilyMismatch {
                transaction_hash,
                expected: PortalCallFamily::SubmitBatch,
                actual: PortalCallFamily::ProcessWithdrawals,
            },
            expected(0x03, |bytes| {
                bytes.hash(transaction_hash);
                bytes.u8(0x01);
                bytes.u8(0x02);
            }),
        ),
        (
            PortalCallError::EmptyProcessWithOutcomes { transaction_hash },
            expected(0x04, |bytes| bytes.hash(transaction_hash)),
        ),
    ];

    for (error, bytes) in cases {
        assert_golden(portal_call(&error).unwrap(), &bytes);
    }
}

#[test]
fn every_imported_projection_branch_has_an_independent_golden_vector() {
    let first_hash = B256::repeat_byte(0x31);
    let second_hash = B256::repeat_byte(0x32);
    let cases = vec![
        (
            ImportedProjectionError::MissingBaseFee,
            expected(0x01, |_| {}),
        ),
        (
            ImportedProjectionError::BlockHashMismatch {
                expected: first_hash,
                actual: second_hash,
            },
            expected(0x02, |bytes| {
                bytes.hash(first_hash);
                bytes.hash(second_hash);
            }),
        ),
        (
            ImportedProjectionError::BlockNumberMismatch {
                expected: 0x0102,
                actual: 0x0304,
            },
            expected(0x03, |bytes| {
                bytes.u64(0x0102);
                bytes.u64(0x0304);
            }),
        ),
        (
            ImportedProjectionError::TransactionOrderMismatch {
                previous: 5,
                next: 8,
            },
            expected(0x04, |bytes| {
                bytes.usize(5);
                bytes.usize(8);
            }),
        ),
        (
            ImportedProjectionError::OutcomeCoordinateMismatch {
                transaction_index: 3,
                transaction_hash: first_hash,
                event_transaction_index: 4,
                event_transaction_hash: second_hash,
            },
            expected(0x05, |bytes| {
                bytes.usize(3);
                bytes.hash(first_hash);
                bytes.usize(4);
                bytes.hash(second_hash);
            }),
        ),
        (
            ImportedProjectionError::InvalidCreationGrammar {
                transaction_index: 6,
            },
            expected(0x06, |bytes| bytes.usize(6)),
        ),
        (
            ImportedProjectionError::InvalidSubmitBatchGrammar {
                transaction_index: 7,
            },
            expected(0x07, |bytes| bytes.usize(7)),
        ),
        (
            ImportedProjectionError::DirectCallRequired {
                transaction_index: 8,
                event: ImportedEventKind::FactoryZoneCreated,
            },
            expected(0x08, |bytes| {
                bytes.usize(8);
                bytes.u8(0x0a);
            }),
        ),
        (
            ImportedProjectionError::UnexpectedEvent {
                transaction_index: 9,
                event: ImportedEventKind::KnownNonModel,
            },
            expected(0x09, |bytes| {
                bytes.usize(9);
                bytes.u8(0x0b);
            }),
        ),
        (
            ImportedProjectionError::InvalidDepositCiphertextLength {
                block_log_index: 10,
                actual: 11,
                expected: 12,
            },
            expected(0x0a, |bytes| {
                bytes.usize(10);
                bytes.usize(11);
                bytes.usize(12);
            }),
        ),
        (
            ImportedProjectionError::InvalidDepositKeyParity {
                block_log_index: 13,
                actual: 0xff,
            },
            expected(0x0b, |bytes| {
                bytes.usize(13);
                bytes.u8(0xff);
            }),
        ),
        (
            ImportedProjectionError::InvalidWithdrawalPreimage {
                transaction_index: 14,
                member_index: 15,
                source: WithdrawalDataError::GasLimitTooHigh {
                    actual: 16,
                    maximum: 17,
                },
            },
            expected(0x0c, |bytes| {
                bytes.usize(14);
                bytes.usize(15);
                bytes.u8(0x03);
                bytes.u64(16);
                bytes.u64(17);
            }),
        ),
        (
            ImportedProjectionError::MissingWithdrawalOutcome {
                transaction_index: 18,
                member_index: 19,
            },
            expected(0x0d, |bytes| {
                bytes.usize(18);
                bytes.usize(19);
            }),
        ),
        (
            ImportedProjectionError::UnexpectedWithdrawalOutcome {
                transaction_index: 20,
                member_index: 21,
                event: ImportedEventKind::WithdrawalBounceBack,
            },
            expected(0x0e, |bytes| {
                bytes.usize(20);
                bytes.usize(21);
                bytes.u8(0x05);
            }),
        ),
        (
            ImportedProjectionError::WithdrawalCallbackSuccessMismatch {
                transaction_index: 22,
                member_index: 23,
                expected: true,
                actual: false,
            },
            expected(0x0f, |bytes| {
                bytes.usize(22);
                bytes.usize(23);
                bytes.bool(true);
                bytes.bool(false);
            }),
        ),
        (
            ImportedProjectionError::ExtraWithdrawalOutcomes {
                transaction_index: 24,
                remaining: 25,
            },
            expected(0x10, |bytes| {
                bytes.usize(24);
                bytes.usize(25);
            }),
        ),
    ];

    for (error, bytes) in cases {
        assert_golden(imported_projection(&error).unwrap(), &bytes);
    }
}

#[test]
fn every_zone_projection_branch_has_an_independent_golden_vector() {
    let outputs =
        ObservedZoneOutputs::empty_for_test(B256::repeat_byte(0x41), 42, B256::repeat_byte(0x43));
    let position = outputs.tempo_block_finalized().position();
    let expected_hash = B256::repeat_byte(0x44);
    let recipient = Address::repeat_byte(0x45);
    let encode_position = |bytes: &mut Golden| {
        bytes.usize(position.transaction_index());
        bytes.hash(position.transaction_hash());
        bytes.usize(position.receipt_log_index());
        bytes.usize(position.block_log_index());
    };
    let with_position = |tag, event: Option<u8>| {
        expected(tag, |bytes| {
            if let Some(event) = event {
                bytes.u8(event);
            }
            encode_position(bytes);
        })
    };
    let cases = vec![
        (
            ZoneProjectionError::MissingTempoBlockFinalized,
            expected(0x01, |_| {}),
        ),
        (
            ZoneProjectionError::ReorderedTempoBlockFinalized {
                actual: ZoneEventKind::TokenEnabled,
                position,
            },
            with_position(0x02, Some(0x02)),
        ),
        (
            ZoneProjectionError::MissingTokenEnabled { index: 3 },
            expected(0x03, |bytes| bytes.usize(3)),
        ),
        (
            ZoneProjectionError::ReorderedTokenEnabled {
                index: 4,
                actual: ZoneEventKind::DepositProcessed,
                position,
            },
            expected(0x04, |bytes| {
                bytes.usize(4);
                bytes.u8(0x03);
                encode_position(bytes);
            }),
        ),
        (
            ZoneProjectionError::MissingDepositOutcome {
                index: 5,
                deposit_kind: DepositInputKind::WithdrawalBounceBack,
            },
            expected(0x05, |bytes| {
                bytes.usize(5);
                bytes.u8(0x02);
            }),
        ),
        (
            ZoneProjectionError::ReorderedDepositOutcome {
                index: 6,
                deposit_kind: DepositInputKind::Ordinary,
                actual: ZoneEventKind::DepositFailed,
                position,
            },
            expected(0x06, |bytes| {
                bytes.usize(6);
                bytes.u8(0x01);
                bytes.u8(0x04);
                encode_position(bytes);
            }),
        ),
        (
            ZoneProjectionError::MissingDepositFailed { index: 7 },
            expected(0x07, |bytes| bytes.usize(7)),
        ),
        (
            ZoneProjectionError::ReorderedDepositFailed {
                index: 8,
                actual: ZoneEventKind::WithdrawalBounceBackProcessed,
                position,
            },
            expected(0x08, |bytes| {
                bytes.usize(8);
                bytes.u8(0x05);
                encode_position(bytes);
            }),
        ),
        (
            ZoneProjectionError::MissingTempoAdvanced,
            expected(0x09, |_| {}),
        ),
        (
            ZoneProjectionError::ReorderedTempoAdvanced {
                actual: ZoneEventKind::WithdrawalBounceBackPending,
                position,
            },
            with_position(0x0a, Some(0x06)),
        ),
        (
            ZoneProjectionError::ExtraAdvanceEvent {
                actual: ZoneEventKind::TempoAdvanced,
                position,
            },
            with_position(0x0b, Some(0x07)),
        ),
        (
            ZoneProjectionError::AdvanceTransactionHashMismatch {
                expected: expected_hash,
                position,
            },
            expected(0x0c, |bytes| {
                bytes.hash(expected_hash);
                encode_position(bytes);
            }),
        ),
        (
            ZoneProjectionError::InvalidDepositKeyParity {
                index: 13,
                actual: 0xfe,
            },
            expected(0x0d, |bytes| {
                bytes.usize(13);
                bytes.u8(0xfe);
            }),
        ),
        (
            ZoneProjectionError::InvalidDepositCiphertextLength {
                index: 14,
                actual: 15,
                expected: 16,
            },
            expected(0x0e, |bytes| {
                bytes.usize(14);
                bytes.usize(15);
                bytes.usize(16);
            }),
        ),
        (
            ZoneProjectionError::InvalidBounceBackRecipient {
                index: 17,
                recipient,
            },
            expected(0x0f, |bytes| {
                bytes.usize(17);
                bytes.address(recipient);
            }),
        ),
        (
            ZoneProjectionError::ZeroBounceBackNonce { index: 18 },
            expected(0x10, |bytes| bytes.usize(18)),
        ),
        (
            ZoneProjectionError::ZeroBounceBackAmount { index: 19 },
            expected(0x11, |bytes| bytes.usize(19)),
        ),
        (
            ZoneProjectionError::InvalidWithdrawalRequest {
                transaction_index: 20,
                source: WithdrawalDataError::InvalidRevealToPrefix { actual: 0xfd },
            },
            expected(0x12, |bytes| {
                bytes.usize(20);
                bytes.u8(0x06);
                bytes.u8(0xfd);
            }),
        ),
        (
            ZoneProjectionError::UnexpectedPostAdvanceEvent {
                actual: ZoneEventKind::WithdrawalRequested,
                position,
            },
            with_position(0x13, Some(0x08)),
        ),
        (
            ZoneProjectionError::BatchFinalizedWithoutEnvelope { position },
            with_position(0x14, None),
        ),
        (
            ZoneProjectionError::BatchFinalizedWrongTransaction {
                expected: expected_hash,
                position,
            },
            expected(0x15, |bytes| {
                bytes.hash(expected_hash);
                encode_position(bytes);
            }),
        ),
        (
            ZoneProjectionError::MissingBatchFinalized {
                transaction_hash: expected_hash,
            },
            expected(0x16, |bytes| bytes.hash(expected_hash)),
        ),
        (
            ZoneProjectionError::ReorderedBatchFinalized {
                actual: ZoneEventKind::BatchFinalized,
                position,
            },
            with_position(0x17, Some(0x0a)),
        ),
        (
            ZoneProjectionError::ExtraFinalizationEvent {
                actual: ZoneEventKind::TempoGasRateUpdated,
                position,
            },
            with_position(0x18, Some(0x0b)),
        ),
        (
            ZoneProjectionError::UnsupportedDepositKind { index: 25 },
            expected(0x19, |bytes| bytes.usize(25)),
        ),
    ];

    for (error, bytes) in cases {
        assert_golden(zone_projection(&error).unwrap(), &bytes);
    }
}
