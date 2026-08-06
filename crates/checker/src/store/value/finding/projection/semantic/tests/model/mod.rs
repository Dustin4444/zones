//! Exhaustive golden vectors for logical-model summaries.

use std::collections::BTreeSet;

use crate::model::transition::ModelError;

use super::super::model;
use super::{Golden, assert_golden};

mod core;
mod nested;
mod settlement;

type Case = (ModelError, Vec<u8>);

fn expected(tag: u8, encode: impl FnOnce(&mut Golden)) -> Vec<u8> {
    let mut bytes = Golden::tagged(tag);
    encode(&mut bytes);
    bytes.finish()
}

fn model_tag(error: &ModelError) -> u8 {
    use ModelError::*;
    match error {
        PortalNotCreated => 0x01,
        PortalAlreadyCreated => 0x02,
        PortalIdentityMismatch { .. } => 0x03,
        PortalAddressMismatch { .. } => 0x04,
        InitialTokenMismatch { .. } => 0x05,
        TokenAlreadyEnabled { .. } => 0x06,
        TokenNotPortalEnabled { .. } => 0x07,
        TokenNotZoneEnabled { .. } => 0x08,
        ZeroTempoRefundRecipient => 0x09,
        ZoneTokenEnableCountMismatch { .. } => 0x0a,
        ZoneTokenEnableMismatch { .. } => 0x0b,
        PortalDepositNumberOverflow => 0x0c,
        DepositOwnerCollision { .. } => 0x0d,
        FallbackOwnerMissing { .. } => 0x0e,
        FallbackOwnerMismatch { .. } => 0x0f,
        WithdrawalBounceBackAlreadyPending { .. } => 0x10,
        DepositOutcomeCountMismatch { .. } => 0x11,
        ProcessedDepositNumberOverflow => 0x12,
        PendingDepositMissing { .. } => 0x13,
        DepositPrefixMismatch { .. } => 0x14,
        DepositOutcomeKindMismatch { .. } => 0x15,
        WithdrawalIndexOverflow => 0x16,
        WithdrawalOwnerCollision { .. } => 0x17,
        WithdrawalBlockCapExceeded { .. } => 0x18,
        FallbackNonceOverflow => 0x19,
        FallbackOwnerCollision { .. } => 0x1a,
        FinalizationBlockNumberMismatch { .. } => 0x1b,
        FinalizationCountMismatch { .. } => 0x1c,
        FinalizationSenderCountMismatch { .. } => 0x1d,
        InvalidBatchWithdrawalRange { .. } => 0x1e,
        WithdrawalOwnerMissing { .. } => 0x1f,
        WithdrawalAlreadyFinalized { .. } => 0x20,
        WithdrawalBatchIndexOverflow => 0x21,
        BatchOwnerCollision { .. } => 0x22,
        PortalBatchIndexOverflow => 0x23,
        BatchOwnerMissing { .. } => 0x24,
        BatchAlreadySubmitted { .. } => 0x25,
        BatchTempoBlockMismatch { .. } => 0x26,
        BatchZoneHeightMismatch { .. } => 0x27,
        BatchBlockTransitionMismatch { .. } => 0x28,
        BatchDepositTransitionMismatch { .. } => 0x29,
        BatchWithdrawalQueueHashMismatch { .. } => 0x2a,
        PortalBlockContinuityMismatch { .. } => 0x2b,
        PortalDepositContinuityMismatch { .. } => 0x2c,
        PortalZoneHeightNotIncreasing { .. } => 0x2d,
        PortalDepositCursorBeyondQueue { .. } => 0x2e,
        InvalidPortalWithdrawalQueueProgress { .. } => 0x2f,
        PortalWithdrawalQueueFull => 0x30,
        PortalWithdrawalQueueCounterOverflow => 0x31,
        WithdrawalProcessingOutcomeCountMismatch { .. } => 0x32,
        PortalWithdrawalQueueEmpty => 0x33,
        PortalWithdrawalQueueHeadMissing => 0x34,
        PortalWithdrawalQueueHeadNotSubmitted => 0x35,
        PortalWithdrawalQueuePortalMismatch { .. } => 0x36,
        PortalWithdrawalQueueHeadMismatch { .. } => 0x37,
        WithdrawalProcessingLengthOverflow { .. } => 0x38,
        WithdrawalProcessingBeyondBatch { .. } => 0x39,
        WithdrawalProcessingExhaustedEarly => 0x3a,
        WithdrawalProcessingLeftSuffixAfterBatch => 0x3b,
        WithdrawalNotFinalizedForProcessing { .. } => 0x3c,
        WithdrawalProcessingPreimageMismatch { .. } => 0x3d,
        WithdrawalProcessingOutcomeMismatch { .. } => 0x3e,
        CallbackDepositsWithoutCallback { .. } => 0x3f,
        PortalRefundCollision { .. } => 0x40,
        RefundAggregateOverflow { .. } => 0x41,
        RefundClaimAmountMismatch { .. } => 0x44,
        InboxRefundCollision { .. } => 0x45,
        ZeroBounceBackRecipient { .. } => 0x46,
        Accounting(_) => 0x47,
        Fee(_) => 0x48,
        WithdrawalData(_) => 0x49,
        BatchState(_) => 0x4a,
        PortalQueueId(_) => 0x4b,
        WithdrawalQueue(_) => 0x4c,
    }
}

#[test]
fn every_model_error_tag_and_payload_is_golden() {
    let cases = core::cases()
        .into_iter()
        .chain(settlement::cases())
        .chain(nested::top_level_cases())
        .collect::<Vec<_>>();
    let expected_tags = (0x01..=0x41).chain(0x44..=0x4c).collect::<BTreeSet<_>>();
    let actual_tags = cases
        .iter()
        .map(|(error, _)| model_tag(error))
        .collect::<BTreeSet<_>>();

    assert_eq!(cases.len(), expected_tags.len());
    assert_eq!(actual_tags, expected_tags);
    for (error, bytes) in cases {
        assert_eq!(bytes.first(), Some(&model_tag(&error)));
        assert_golden(model(&error).unwrap(), &bytes);
    }
}
