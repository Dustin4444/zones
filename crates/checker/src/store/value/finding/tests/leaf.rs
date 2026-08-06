use std::fmt::Debug;

use alloy_primitives::B256;

use crate::{
    model::{
        adapter::{ImportedProjectionError, ZoneProjectionError},
        transition::ModelError,
    },
    observe::{DataSource, EnvelopeRule, PortalCallError, ProtocolChain},
    store::value::finding::{
        StoredDataSource, StoredEnvelopeRule, StoredImportedProjectionError, StoredModelError,
        StoredPortalCallError, StoredProtocolChain, StoredZoneProjectionError,
    },
};

fn assert_stable_tags<T: Copy + Debug + Eq>(
    expected: &[(T, u8)],
    encode: impl Fn(T) -> u8,
    decode: impl Fn(u8) -> Option<T>,
) {
    for &(value, tag) in expected {
        assert_eq!(encode(value), tag);
        assert_eq!(decode(tag), Some(value));
    }
    assert_eq!(decode(0), None);
    assert_eq!(decode(0xff), None);
}

#[test]
fn observation_leaf_tags_and_runtime_conversions_are_exhaustive() {
    assert_stable_tags(
        &[
            (StoredProtocolChain::TempoL1, 0x01),
            (StoredProtocolChain::ZoneL2, 0x02),
        ],
        StoredProtocolChain::wire_tag,
        StoredProtocolChain::from_wire_tag,
    );
    assert_eq!(
        StoredProtocolChain::from(ProtocolChain::TempoL1),
        StoredProtocolChain::TempoL1
    );
    assert_eq!(
        ProtocolChain::from(StoredProtocolChain::ZoneL2),
        ProtocolChain::ZoneL2
    );

    assert_stable_tags(
        &[
            (StoredEnvelopeRule::NonGenesis, 0x01),
            (StoredEnvelopeRule::AdvancePresent, 0x02),
            (StoredEnvelopeRule::AdvanceSystemCaller, 0x03),
            (StoredEnvelopeRule::AdvanceDestination, 0x04),
            (StoredEnvelopeRule::AdvanceSuccess, 0x05),
            (StoredEnvelopeRule::SystemIdentity, 0x06),
            (StoredEnvelopeRule::FinalizationPosition, 0x07),
            (StoredEnvelopeRule::FinalizationDestination, 0x08),
            (StoredEnvelopeRule::FinalizationSuccess, 0x09),
            (StoredEnvelopeRule::FinalizationBlockNumber, 0x0a),
        ],
        StoredEnvelopeRule::wire_tag,
        StoredEnvelopeRule::from_wire_tag,
    );
    assert_eq!(
        StoredEnvelopeRule::from(EnvelopeRule::FinalizationBlockNumber),
        StoredEnvelopeRule::FinalizationBlockNumber
    );
    assert_eq!(
        EnvelopeRule::from(StoredEnvelopeRule::AdvancePresent),
        EnvelopeRule::AdvancePresent
    );

    assert_stable_tags(
        &[
            (StoredDataSource::AdvanceTempoCalldata, 0x01),
            (StoredDataSource::AdvanceHeaderRlp, 0x02),
            (StoredDataSource::OrdinaryDepositData, 0x03),
            (StoredDataSource::WithdrawalBounceBackData, 0x04),
            (StoredDataSource::FinalizationCalldata, 0x05),
            (StoredDataSource::ProcessWithdrawalsCalldata, 0x06),
            (StoredDataSource::SubmitBatchCalldata, 0x07),
            (StoredDataSource::PortalTransactionCalldata, 0x08),
        ],
        StoredDataSource::wire_tag,
        StoredDataSource::from_wire_tag,
    );
    assert_eq!(
        StoredDataSource::from(DataSource::SubmitBatchCalldata),
        StoredDataSource::SubmitBatchCalldata
    );
    assert_eq!(
        DataSource::from(StoredDataSource::OrdinaryDepositData),
        DataSource::OrdinaryDepositData
    );

    assert_stable_tags(
        &[
            (StoredPortalCallError::UnsupportedNestedPortalCall, 0x01),
            (StoredPortalCallError::ConflictingFamilies, 0x02),
            (StoredPortalCallError::FamilyMismatch, 0x03),
            (StoredPortalCallError::EmptyProcessWithOutcomes, 0x04),
        ],
        StoredPortalCallError::wire_tag,
        StoredPortalCallError::from_wire_tag,
    );
    assert_eq!(
        StoredPortalCallError::from(&PortalCallError::ConflictingFamilies {
            transaction_hash: B256::ZERO,
        }),
        StoredPortalCallError::ConflictingFamilies
    );
}

#[test]
fn projection_leaf_tags_are_complete_and_stable() {
    assert_stable_tags(
        &[
            (StoredImportedProjectionError::MissingBaseFee, 0x01),
            (StoredImportedProjectionError::BlockHashMismatch, 0x02),
            (StoredImportedProjectionError::BlockNumberMismatch, 0x03),
            (
                StoredImportedProjectionError::TransactionOrderMismatch,
                0x04,
            ),
            (
                StoredImportedProjectionError::OutcomeCoordinateMismatch,
                0x05,
            ),
            (StoredImportedProjectionError::InvalidCreationGrammar, 0x06),
            (
                StoredImportedProjectionError::InvalidSubmitBatchGrammar,
                0x07,
            ),
            (StoredImportedProjectionError::DirectCallRequired, 0x08),
            (StoredImportedProjectionError::UnexpectedEvent, 0x09),
            (
                StoredImportedProjectionError::InvalidDepositCiphertextLength,
                0x0a,
            ),
            (StoredImportedProjectionError::InvalidDepositKeyParity, 0x0b),
            (
                StoredImportedProjectionError::InvalidWithdrawalPreimage,
                0x0c,
            ),
            (
                StoredImportedProjectionError::MissingWithdrawalOutcome,
                0x0d,
            ),
            (
                StoredImportedProjectionError::UnexpectedWithdrawalOutcome,
                0x0e,
            ),
            (
                StoredImportedProjectionError::WithdrawalCallbackSuccessMismatch,
                0x0f,
            ),
            (StoredImportedProjectionError::ExtraWithdrawalOutcomes, 0x10),
        ],
        StoredImportedProjectionError::wire_tag,
        StoredImportedProjectionError::from_wire_tag,
    );
    assert_eq!(
        StoredImportedProjectionError::from(&ImportedProjectionError::MissingBaseFee),
        StoredImportedProjectionError::MissingBaseFee
    );

    assert_stable_tags(
        &[
            (StoredZoneProjectionError::MissingTempoBlockFinalized, 0x01),
            (
                StoredZoneProjectionError::ReorderedTempoBlockFinalized,
                0x02,
            ),
            (StoredZoneProjectionError::MissingTokenEnabled, 0x03),
            (StoredZoneProjectionError::ReorderedTokenEnabled, 0x04),
            (StoredZoneProjectionError::MissingDepositOutcome, 0x05),
            (StoredZoneProjectionError::ReorderedDepositOutcome, 0x06),
            (StoredZoneProjectionError::MissingDepositFailed, 0x07),
            (StoredZoneProjectionError::ReorderedDepositFailed, 0x08),
            (StoredZoneProjectionError::MissingTempoAdvanced, 0x09),
            (StoredZoneProjectionError::ReorderedTempoAdvanced, 0x0a),
            (StoredZoneProjectionError::ExtraAdvanceEvent, 0x0b),
            (
                StoredZoneProjectionError::AdvanceTransactionHashMismatch,
                0x0c,
            ),
            (StoredZoneProjectionError::InvalidDepositKeyParity, 0x0d),
            (
                StoredZoneProjectionError::InvalidDepositCiphertextLength,
                0x0e,
            ),
            (StoredZoneProjectionError::InvalidBounceBackRecipient, 0x0f),
            (StoredZoneProjectionError::ZeroBounceBackNonce, 0x10),
            (StoredZoneProjectionError::ZeroBounceBackAmount, 0x11),
            (StoredZoneProjectionError::InvalidWithdrawalRequest, 0x12),
            (StoredZoneProjectionError::UnexpectedPostAdvanceEvent, 0x13),
            (
                StoredZoneProjectionError::BatchFinalizedWithoutEnvelope,
                0x14,
            ),
            (
                StoredZoneProjectionError::BatchFinalizedWrongTransaction,
                0x15,
            ),
            (StoredZoneProjectionError::MissingBatchFinalized, 0x16),
            (StoredZoneProjectionError::ReorderedBatchFinalized, 0x17),
            (StoredZoneProjectionError::ExtraFinalizationEvent, 0x18),
            (StoredZoneProjectionError::UnsupportedDepositKind, 0x19),
        ],
        StoredZoneProjectionError::wire_tag,
        StoredZoneProjectionError::from_wire_tag,
    );
    assert_eq!(
        StoredZoneProjectionError::from(&ZoneProjectionError::MissingTempoBlockFinalized),
        StoredZoneProjectionError::MissingTempoBlockFinalized
    );
}

#[test]
fn model_leaf_tags_preserve_retired_slots_and_runtime_mapping() {
    assert_stable_tags(
        &[
            (StoredModelError::PortalNotCreated, 0x01),
            (StoredModelError::PortalAlreadyCreated, 0x02),
            (StoredModelError::PortalIdentityMismatch, 0x03),
            (StoredModelError::PortalAddressMismatch, 0x04),
            (StoredModelError::InitialTokenMismatch, 0x05),
            (StoredModelError::TokenAlreadyEnabled, 0x06),
            (StoredModelError::TokenNotPortalEnabled, 0x07),
            (StoredModelError::TokenNotZoneEnabled, 0x08),
            (StoredModelError::ZeroTempoRefundRecipient, 0x09),
            (StoredModelError::ZoneTokenEnableCountMismatch, 0x0a),
            (StoredModelError::ZoneTokenEnableMismatch, 0x0b),
            (StoredModelError::PortalDepositNumberOverflow, 0x0c),
            (StoredModelError::DepositOwnerCollision, 0x0d),
            (StoredModelError::FallbackOwnerMissing, 0x0e),
            (StoredModelError::FallbackOwnerMismatch, 0x0f),
            (StoredModelError::WithdrawalBounceBackAlreadyPending, 0x10),
            (StoredModelError::DepositOutcomeCountMismatch, 0x11),
            (StoredModelError::ProcessedDepositNumberOverflow, 0x12),
            (StoredModelError::PendingDepositMissing, 0x13),
            (StoredModelError::DepositPrefixMismatch, 0x14),
            (StoredModelError::DepositOutcomeKindMismatch, 0x15),
            (StoredModelError::WithdrawalIndexOverflow, 0x16),
            (StoredModelError::WithdrawalOwnerCollision, 0x17),
            (StoredModelError::WithdrawalBlockCapExceeded, 0x18),
            (StoredModelError::FallbackNonceOverflow, 0x19),
            (StoredModelError::FallbackOwnerCollision, 0x1a),
            (StoredModelError::FinalizationBlockNumberMismatch, 0x1b),
            (StoredModelError::FinalizationCountMismatch, 0x1c),
            (StoredModelError::FinalizationSenderCountMismatch, 0x1d),
            (StoredModelError::InvalidBatchWithdrawalRange, 0x1e),
            (StoredModelError::WithdrawalOwnerMissing, 0x1f),
            (StoredModelError::WithdrawalAlreadyFinalized, 0x20),
            (StoredModelError::WithdrawalBatchIndexOverflow, 0x21),
            (StoredModelError::BatchOwnerCollision, 0x22),
            (StoredModelError::PortalBatchIndexOverflow, 0x23),
            (StoredModelError::BatchOwnerMissing, 0x24),
            (StoredModelError::BatchAlreadySubmitted, 0x25),
            (StoredModelError::BatchTempoBlockMismatch, 0x26),
            (StoredModelError::BatchZoneHeightMismatch, 0x27),
            (StoredModelError::BatchBlockTransitionMismatch, 0x28),
            (StoredModelError::BatchDepositTransitionMismatch, 0x29),
            (StoredModelError::BatchWithdrawalQueueHashMismatch, 0x2a),
            (StoredModelError::PortalBlockContinuityMismatch, 0x2b),
            (StoredModelError::PortalDepositContinuityMismatch, 0x2c),
            (StoredModelError::PortalZoneHeightNotIncreasing, 0x2d),
            (StoredModelError::PortalDepositCursorBeyondQueue, 0x2e),
            (StoredModelError::InvalidPortalWithdrawalQueueProgress, 0x2f),
            (StoredModelError::PortalWithdrawalQueueFull, 0x30),
            (StoredModelError::PortalWithdrawalQueueCounterOverflow, 0x31),
            (
                StoredModelError::WithdrawalProcessingOutcomeCountMismatch,
                0x32,
            ),
            (StoredModelError::PortalWithdrawalQueueEmpty, 0x33),
            (StoredModelError::PortalWithdrawalQueueHeadMissing, 0x34),
            (
                StoredModelError::PortalWithdrawalQueueHeadNotSubmitted,
                0x35,
            ),
            (StoredModelError::PortalWithdrawalQueuePortalMismatch, 0x36),
            (StoredModelError::PortalWithdrawalQueueHeadMismatch, 0x37),
            (StoredModelError::WithdrawalProcessingLengthOverflow, 0x38),
            (StoredModelError::WithdrawalProcessingBeyondBatch, 0x39),
            (StoredModelError::WithdrawalProcessingExhaustedEarly, 0x3a),
            (
                StoredModelError::WithdrawalProcessingLeftSuffixAfterBatch,
                0x3b,
            ),
            (StoredModelError::WithdrawalNotFinalizedForProcessing, 0x3c),
            (StoredModelError::WithdrawalProcessingPreimageMismatch, 0x3d),
            (StoredModelError::WithdrawalProcessingOutcomeMismatch, 0x3e),
            (StoredModelError::CallbackDepositsWithoutCallback, 0x3f),
            (StoredModelError::PortalRefundCollision, 0x40),
            (StoredModelError::RefundAggregateOverflow, 0x41),
            (
                StoredModelError::RetiredPortalRefundAggregateStateMismatch,
                0x42,
            ),
            (
                StoredModelError::RetiredInboxRefundAggregateStateMismatch,
                0x43,
            ),
            (StoredModelError::RefundClaimAmountMismatch, 0x44),
            (StoredModelError::InboxRefundCollision, 0x45),
            (StoredModelError::ZeroBounceBackRecipient, 0x46),
            (StoredModelError::Accounting, 0x47),
            (StoredModelError::Fee, 0x48),
            (StoredModelError::WithdrawalData, 0x49),
            (StoredModelError::BatchState, 0x4a),
            (StoredModelError::PortalQueueId, 0x4b),
            (StoredModelError::WithdrawalQueue, 0x4c),
        ],
        StoredModelError::wire_tag,
        StoredModelError::from_wire_tag,
    );
    assert_eq!(
        StoredModelError::from(&ModelError::PortalNotCreated),
        StoredModelError::PortalNotCreated
    );
}
