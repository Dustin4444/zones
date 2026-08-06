use crate::{
    model::transition::ModelError,
    store::{error::StoreResult, value::finding::leaf::StoredModelError},
};

use super::{Canonical, FindingSummary, nested};

pub(in super::super) fn model(error: &ModelError) -> StoreResult<FindingSummary> {
    use ModelError::*;

    let tag = StoredModelError::from(error).wire_tag();
    let encoder = match error {
        PortalNotCreated => Canonical::tagged(tag),
        PortalAlreadyCreated => Canonical::tagged(tag),
        PortalIdentityMismatch { expected, actual } => {
            let mut encoder = Canonical::tagged(tag);
            portal_identity(&mut encoder, expected);
            portal_identity(&mut encoder, actual);
            encoder
        }
        PortalAddressMismatch { expected, actual } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.address(*expected);
            encoder.address(*actual);
            encoder
        }
        InitialTokenMismatch { expected, actual } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.address(*expected);
            encoder.address(*actual);
            encoder
        }
        TokenAlreadyEnabled { token } => one_address(tag, *token),
        TokenNotPortalEnabled { token } => one_address(tag, *token),
        TokenNotZoneEnabled { token } => one_address(tag, *token),
        ZeroTempoRefundRecipient => Canonical::tagged(tag),
        ZoneTokenEnableCountMismatch { expected, actual } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*expected)?;
            encoder.usize(*actual)?;
            encoder
        }
        ZoneTokenEnableMismatch {
            index,
            expected,
            actual,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*index)?;
            token_enable(
                &mut encoder,
                expected.token(),
                expected.name(),
                expected.symbol(),
                expected.currency(),
            )?;
            token_enable(
                &mut encoder,
                actual.token(),
                actual.name(),
                actual.symbol(),
                actual.currency(),
            )?;
            encoder
        }
        PortalDepositNumberOverflow => Canonical::tagged(tag),
        DepositOwnerCollision { number } => one_u64(tag, *number),
        FallbackOwnerMissing { fallback_nonce } => one_u64(tag, *fallback_nonce),
        FallbackOwnerMismatch { fallback_nonce } => one_u64(tag, *fallback_nonce),
        WithdrawalBounceBackAlreadyPending { withdrawal_index } => one_u64(tag, *withdrawal_index),
        DepositOutcomeCountMismatch { deposits, outcomes } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*deposits)?;
            encoder.usize(*outcomes)?;
            encoder
        }
        ProcessedDepositNumberOverflow => Canonical::tagged(tag),
        PendingDepositMissing { number } => one_u64(tag, *number),
        DepositPrefixMismatch { number } => one_u64(tag, *number),
        DepositOutcomeKindMismatch {
            number,
            expected,
            actual,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u64(*number);
            encoder.u8(nested::deposit_kind(*expected));
            encoder.u8(nested::deposit_outcome_kind(*actual));
            encoder
        }
        WithdrawalIndexOverflow => Canonical::tagged(tag),
        WithdrawalOwnerCollision { withdrawal_index } => one_u64(tag, *withdrawal_index),
        WithdrawalBlockCapExceeded { limit } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u32(*limit);
            encoder
        }
        FallbackNonceOverflow => Canonical::tagged(tag),
        FallbackOwnerCollision { fallback_nonce } => one_u64(tag, *fallback_nonce),
        FinalizationBlockNumberMismatch { expected, actual } => two_u64(tag, *expected, *actual),
        FinalizationCountMismatch { expected, actual } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u64(*expected);
            encoder.usize(*actual)?;
            encoder
        }
        FinalizationSenderCountMismatch { declared, actual } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*declared)?;
            encoder.usize(*actual)?;
            encoder
        }
        InvalidBatchWithdrawalRange { first, next } => two_u64(tag, *first, *next),
        WithdrawalOwnerMissing { withdrawal_index } => one_u64(tag, *withdrawal_index),
        WithdrawalAlreadyFinalized { withdrawal_index } => one_u64(tag, *withdrawal_index),
        WithdrawalBatchIndexOverflow => Canonical::tagged(tag),
        BatchOwnerCollision {
            withdrawal_batch_index,
        } => one_u64(tag, *withdrawal_batch_index),
        PortalBatchIndexOverflow => Canonical::tagged(tag),
        BatchOwnerMissing {
            withdrawal_batch_index,
        } => one_u64(tag, *withdrawal_batch_index),
        BatchAlreadySubmitted {
            withdrawal_batch_index,
        } => one_u64(tag, *withdrawal_batch_index),
        BatchTempoBlockMismatch {
            withdrawal_batch_index,
            expected,
            actual,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u64(*withdrawal_batch_index);
            encoder.u64(*expected);
            encoder.u64(*actual);
            encoder
        }
        BatchZoneHeightMismatch {
            withdrawal_batch_index,
            expected,
            actual,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u64(*withdrawal_batch_index);
            encoder.u256(*expected);
            encoder.u256(*actual);
            encoder
        }
        BatchBlockTransitionMismatch {
            withdrawal_batch_index,
            details,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u64(*withdrawal_batch_index);
            encoder.hash(details.expected_previous);
            encoder.hash(details.actual_previous);
            encoder.hash(details.expected_next);
            encoder.hash(details.actual_next);
            encoder
        }
        BatchDepositTransitionMismatch {
            withdrawal_batch_index,
            details,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u64(*withdrawal_batch_index);
            deposit_cursor(&mut encoder, details.expected_previous);
            deposit_cursor(&mut encoder, details.actual_previous);
            deposit_cursor(&mut encoder, details.expected_next);
            deposit_cursor(&mut encoder, details.actual_next);
            encoder
        }
        BatchWithdrawalQueueHashMismatch {
            withdrawal_batch_index,
            expected,
            actual,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u64(*withdrawal_batch_index);
            encoder.hash(*expected);
            encoder.hash(*actual);
            encoder
        }
        PortalBlockContinuityMismatch {
            withdrawal_batch_index,
            expected,
            actual,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u64(*withdrawal_batch_index);
            encoder.hash(*expected);
            encoder.hash(*actual);
            encoder
        }
        PortalDepositContinuityMismatch {
            withdrawal_batch_index,
            expected,
            actual,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u64(*withdrawal_batch_index);
            deposit_cursor(&mut encoder, *expected);
            deposit_cursor(&mut encoder, *actual);
            encoder
        }
        PortalZoneHeightNotIncreasing {
            withdrawal_batch_index,
            previous,
            next,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u64(*withdrawal_batch_index);
            encoder.u256(*previous);
            encoder.u256(*next);
            encoder
        }
        PortalDepositCursorBeyondQueue {
            withdrawal_batch_index,
            submitted,
            deposited,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u64(*withdrawal_batch_index);
            encoder.u64(*submitted);
            encoder.u64(*deposited);
            encoder
        }
        InvalidPortalWithdrawalQueueProgress { head, tail } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u256(*head);
            encoder.u256(*tail);
            encoder
        }
        PortalWithdrawalQueueFull => Canonical::tagged(tag),
        PortalWithdrawalQueueCounterOverflow => Canonical::tagged(tag),
        WithdrawalProcessingOutcomeCountMismatch {
            withdrawals,
            outcomes,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*withdrawals)?;
            encoder.usize(*outcomes)?;
            encoder
        }
        PortalWithdrawalQueueEmpty => Canonical::tagged(tag),
        PortalWithdrawalQueueHeadMissing => Canonical::tagged(tag),
        PortalWithdrawalQueueHeadNotSubmitted => Canonical::tagged(tag),
        PortalWithdrawalQueuePortalMismatch { expected, actual } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.address(*expected);
            encoder.address(*actual);
            encoder
        }
        PortalWithdrawalQueueHeadMismatch { expected, actual } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u256(*expected);
            encoder.u256(*actual);
            encoder
        }
        WithdrawalProcessingLengthOverflow { actual } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.usize(*actual)?;
            encoder
        }
        WithdrawalProcessingBeyondBatch { remaining, actual } => two_u64(tag, *remaining, *actual),
        WithdrawalProcessingExhaustedEarly => Canonical::tagged(tag),
        WithdrawalProcessingLeftSuffixAfterBatch => Canonical::tagged(tag),
        WithdrawalNotFinalizedForProcessing { withdrawal_index } => one_u64(tag, *withdrawal_index),
        WithdrawalProcessingPreimageMismatch { withdrawal_index } => {
            one_u64(tag, *withdrawal_index)
        }
        WithdrawalProcessingOutcomeMismatch {
            withdrawal_index,
            expected,
            actual,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.u64(*withdrawal_index);
            encoder.u8(nested::withdrawal_origin(*expected));
            encoder.u8(nested::processing_outcome(*actual));
            encoder
        }
        CallbackDepositsWithoutCallback { withdrawal_index } => one_u64(tag, *withdrawal_index),
        PortalRefundCollision { deposit_number } => one_u64(tag, *deposit_number),
        RefundAggregateOverflow { token, recipient } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.address(*token);
            encoder.address(*recipient);
            encoder
        }
        RefundClaimAmountMismatch {
            token,
            recipient,
            expected,
            actual,
        } => {
            let mut encoder = Canonical::tagged(tag);
            encoder.address(*token);
            encoder.address(*recipient);
            encoder.u128(*expected);
            encoder.u128(*actual);
            encoder
        }
        InboxRefundCollision { withdrawal_index } => one_u64(tag, *withdrawal_index),
        ZeroBounceBackRecipient { withdrawal_index } => one_u64(tag, *withdrawal_index),
        Accounting(error) => {
            let mut encoder = Canonical::tagged(tag);
            nested::accounting(&mut encoder, *error);
            encoder
        }
        Fee(error) => {
            let mut encoder = Canonical::tagged(tag);
            nested::fee(&mut encoder, *error);
            encoder
        }
        WithdrawalData(error) => {
            let mut encoder = Canonical::tagged(tag);
            nested::withdrawal_data(&mut encoder, *error)?;
            encoder
        }
        BatchState(error) => {
            let mut encoder = Canonical::tagged(tag);
            nested::batch_state(&mut encoder, *error)?;
            encoder
        }
        PortalQueueId(error) => {
            let mut encoder = Canonical::tagged(tag);
            nested::portal_queue_id(&mut encoder, *error);
            encoder
        }
        WithdrawalQueue(error) => {
            let mut encoder = Canonical::tagged(tag);
            nested::withdrawal_queue(&mut encoder, *error);
            encoder
        }
    };
    encoder.finish()
}

fn one_u64(tag: u8, value: u64) -> Canonical {
    let mut encoder = Canonical::tagged(tag);
    encoder.u64(value);
    encoder
}

fn two_u64(tag: u8, first: u64, second: u64) -> Canonical {
    let mut encoder = one_u64(tag, first);
    encoder.u64(second);
    encoder
}

fn one_address(tag: u8, value: alloy_primitives::Address) -> Canonical {
    let mut encoder = Canonical::tagged(tag);
    encoder.address(value);
    encoder
}

fn portal_identity(encoder: &mut Canonical, identity: &crate::model::state::PortalIdentity) {
    encoder.address(identity.portal());
    encoder.u32(identity.zone_id());
    encoder.address(identity.initial_token());
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

fn deposit_cursor(encoder: &mut Canonical, cursor: crate::model::ownership::DepositCursor) {
    encoder.hash(cursor.hash);
    encoder.u64(cursor.number);
}
