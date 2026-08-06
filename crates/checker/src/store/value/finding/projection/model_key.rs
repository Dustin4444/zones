use crate::{model::transition::ModelError, store::schema::ModelKey};

pub(super) fn model_key(error: &ModelError) -> Option<ModelKey> {
    use ModelError::*;

    match error {
        PortalNotCreated
        | PortalAlreadyCreated
        | PortalIdentityMismatch { .. }
        | PortalAddressMismatch { .. } => Some(ModelKey::PortalConfig),
        InitialTokenMismatch { expected, .. } => Some(ModelKey::Token(*expected)),
        TokenAlreadyEnabled { token }
        | TokenNotPortalEnabled { token }
        | TokenNotZoneEnabled { token } => Some(ModelKey::Token(*token)),
        ZoneTokenEnableMismatch { expected, .. } => Some(ModelKey::Token(expected.token())),
        PortalDepositNumberOverflow => Some(ModelKey::PortalDepositCursor),
        DepositOwnerCollision { number }
        | PendingDepositMissing { number }
        | DepositPrefixMismatch { number }
        | DepositOutcomeKindMismatch { number, .. } => Some(ModelKey::PendingDeposit(*number)),
        FallbackOwnerMissing { fallback_nonce }
        | FallbackOwnerMismatch { fallback_nonce }
        | FallbackOwnerCollision { fallback_nonce } => {
            Some(ModelKey::FallbackOwner(*fallback_nonce))
        }
        WithdrawalBounceBackAlreadyPending { withdrawal_index }
        | WithdrawalOwnerCollision { withdrawal_index }
        | WithdrawalOwnerMissing { withdrawal_index }
        | WithdrawalAlreadyFinalized { withdrawal_index }
        | WithdrawalNotFinalizedForProcessing { withdrawal_index }
        | WithdrawalProcessingPreimageMismatch { withdrawal_index }
        | WithdrawalProcessingOutcomeMismatch {
            withdrawal_index, ..
        }
        | CallbackDepositsWithoutCallback { withdrawal_index }
        | InboxRefundCollision { withdrawal_index }
        | ZeroBounceBackRecipient { withdrawal_index } => {
            Some(ModelKey::Withdrawal(*withdrawal_index))
        }
        ProcessedDepositNumberOverflow => Some(ModelKey::ZoneProcessedDepositCursor),
        WithdrawalIndexOverflow => Some(ModelKey::ZoneNextWithdrawalIndex),
        WithdrawalBlockCapExceeded { .. } => Some(ModelKey::ZoneConfig),
        FallbackNonceOverflow => Some(ModelKey::ZoneLastFallbackNonce),
        FinalizationBlockNumberMismatch { .. }
        | FinalizationSenderCountMismatch { .. }
        | InvalidBatchWithdrawalRange { .. }
        | WithdrawalBatchIndexOverflow => Some(ModelKey::ZoneBatchAccumulator),
        BatchOwnerCollision {
            withdrawal_batch_index,
        }
        | BatchOwnerMissing {
            withdrawal_batch_index,
        }
        | BatchAlreadySubmitted {
            withdrawal_batch_index,
        }
        | BatchTempoBlockMismatch {
            withdrawal_batch_index,
            ..
        }
        | BatchZoneHeightMismatch {
            withdrawal_batch_index,
            ..
        }
        | BatchBlockTransitionMismatch {
            withdrawal_batch_index,
            ..
        }
        | BatchDepositTransitionMismatch {
            withdrawal_batch_index,
            ..
        }
        | BatchWithdrawalQueueHashMismatch {
            withdrawal_batch_index,
            ..
        }
        | PortalBlockContinuityMismatch {
            withdrawal_batch_index,
            ..
        }
        | PortalDepositContinuityMismatch {
            withdrawal_batch_index,
            ..
        }
        | PortalZoneHeightNotIncreasing {
            withdrawal_batch_index,
            ..
        }
        | PortalDepositCursorBeyondQueue {
            withdrawal_batch_index,
            ..
        } => Some(ModelKey::Batch(*withdrawal_batch_index)),
        PortalBatchIndexOverflow
        | InvalidPortalWithdrawalQueueProgress { .. }
        | PortalWithdrawalQueueFull
        | PortalWithdrawalQueueCounterOverflow
        | PortalWithdrawalQueueEmpty
        | PortalWithdrawalQueueHeadMissing
        | PortalWithdrawalQueueHeadNotSubmitted
        | PortalWithdrawalQueuePortalMismatch { .. }
        | PortalWithdrawalQueueHeadMismatch { .. }
        | WithdrawalProcessingBeyondBatch { .. }
        | WithdrawalProcessingExhaustedEarly
        | WithdrawalProcessingLeftSuffixAfterBatch => Some(ModelKey::PortalSettlement),
        ZeroTempoRefundRecipient
        | ZoneTokenEnableCountMismatch { .. }
        | DepositOutcomeCountMismatch { .. }
        | FinalizationCountMismatch { .. }
        | WithdrawalProcessingOutcomeCountMismatch { .. }
        | WithdrawalProcessingLengthOverflow { .. }
        | PortalRefundCollision { .. }
        | RefundAggregateOverflow { .. }
        | RefundClaimAmountMismatch { .. }
        | Accounting(_)
        | Fee(_)
        | WithdrawalData(_)
        | BatchState(_)
        | PortalQueueId(_)
        | WithdrawalQueue(_) => None,
    }
}
