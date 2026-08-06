//! Cross-row invariants for one authoritative materialized model cut.
//!
//! Constructors keep individual owners well formed. This validator closes the
//! persistence boundary by checking relationships that only exist across rows
//! and monotonic counters.

use alloy_primitives::{Address, B256, U256};

use super::state::{ModelState, PortalLifecycle};

mod accounting;
mod batches;
mod counters;
mod origins;
mod owners;
mod refunds;

/// A closed label set used only to locate a malformed authoritative row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnerKind {
    PendingDeposit,
    Withdrawal,
    Fallback,
    Batch,
    PortalRefund,
    InboxRefund,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiabilityKind {
    Deposit,
    Withdrawal,
}

/// A closed label set for ordered deposit-queue cursors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorKind {
    PortalDeposit,
    ZoneProcessedDeposit,
    PortalLastSubmittedDeposit,
    ZoneBatchStartDeposit,
    BatchFirstProcessed { batch_index: u64 },
    BatchFinalProcessed { batch_index: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum AuthoritativeStateError {
    #[error("an awaiting-creation model contains created-lifecycle state")]
    AwaitingCreationHasLifecycleState,
    #[error("Portal queue counters are reversed: head {head}, tail {tail}")]
    PortalQueueCountersReversed { head: U256, tail: U256 },
    #[error("Portal queue length {length} exceeds capacity {capacity}")]
    PortalQueueCapacityExceeded { length: U256, capacity: U256 },
    #[error("{cursor:?} cursor zero carries nonzero commitment {hash}")]
    ZeroCursorHasCommitment { cursor: CursorKind, hash: B256 },
    #[error(
        "{earlier:?} cursor number {earlier_number} is beyond {later:?} cursor number {later_number}"
    )]
    CursorOrder {
        earlier: CursorKind,
        earlier_number: u64,
        later: CursorKind,
        later_number: u64,
    },
    #[error(
        "equal cursor number {number} has different commitments for {earlier:?} ({earlier_hash}) and {later:?} ({later_hash})"
    )]
    CursorCommitmentMismatch {
        earlier: CursorKind,
        later: CursorKind,
        number: u64,
        earlier_hash: B256,
        later_hash: B256,
    },
    #[error("an unsubmitted Portal has non-initial settlement state")]
    UnsubmittedPortalHasSettlementProgress,
    #[error(
        "Portal submitted batch counter {portal_batch_index} exceeds Zone batch counter {zone_batch_index}"
    )]
    PortalBatchCounterBeyondZone {
        portal_batch_index: u64,
        zone_batch_index: u64,
    },
    #[error("Portal submitted Zone height {height} does not fit the native u64 block domain")]
    PortalZoneHeightOverflow { height: U256 },
    #[error(
        "fallback counter {fallback_nonce} exceeds next withdrawal index {next_withdrawal_index}"
    )]
    FallbackCounterBeyondWithdrawals {
        fallback_nonce: u64,
        next_withdrawal_index: u64,
    },
    #[error("a Zone with no finalized batch has non-initial batch-accumulator state")]
    UnfinalizedZoneHasBatchProgress,
    #[error(
        "batch start withdrawal index {first_withdrawal_index} exceeds next index {next_withdrawal_index}"
    )]
    BatchStartBeyondNextWithdrawal {
        first_withdrawal_index: u64,
        next_withdrawal_index: u64,
    },
    #[error("{owner:?} row has Portal {actual}, expected {expected}")]
    OwnerPortalMismatch {
        owner: OwnerKind,
        expected: Address,
        actual: Address,
    },
    #[error("{owner:?} row has Zone ID {actual}, expected {expected}")]
    OwnerZoneMismatch {
        owner: OwnerKind,
        expected: u32,
        actual: u32,
    },
    #[error("{owner:?} row references missing token {token}")]
    MissingOwnerToken { owner: OwnerKind, token: Address },
    #[error("created Portal state is missing its initial token {token}")]
    MissingInitialToken { token: Address },
    #[error("{owner:?} row references token {token}, which is not Zone-enabled")]
    OwnerTokenNotZoneEnabled { owner: OwnerKind, token: Address },
    #[error(
        "Portal-only token {token} has nonzero Zone state: supply {supply}, withdrawal liability {withdrawal_liability}"
    )]
    PendingZoneTokenHasZoneState {
        token: Address,
        supply: U256,
        withdrawal_liability: U256,
    },
    #[error(
        "token {token} {kind:?} liability mismatch: open owners require {expected}, persisted accounting has {actual}"
    )]
    TokenLiabilityMismatch {
        token: Address,
        kind: LiabilityKind,
        expected: U256,
        actual: U256,
    },
    #[error("token {token} {kind:?} liability overflows U256 while summing open owners")]
    TokenLiabilityOverflow { token: Address, kind: LiabilityKind },
    #[error("token {token} collateral requirement overflows U256")]
    TokenCollateralOverflow { token: Address },
    #[error("ordinary pending deposit {deposit_number} has a zero Tempo refund recipient")]
    ZeroTempoRefundRecipient { deposit_number: u64 },
    #[error("pending failed-deposit withdrawal {withdrawal_index} has a zero recipient")]
    ZeroPendingFailedDepositRecipient { withdrawal_index: u64 },
    #[error("finalized failed-deposit withdrawal {withdrawal_index} has a zero recipient")]
    ZeroFinalizedFailedDepositRecipient { withdrawal_index: u64 },
    #[error(
        "pending deposit {deposit_number} is beyond Portal deposit cursor {portal_deposit_number}"
    )]
    DepositBeyondPortalCursor {
        deposit_number: u64,
        portal_deposit_number: u64,
    },
    #[error(
        "pending deposit {deposit_number} is not after Zone processed cursor {zone_processed_number}"
    )]
    PendingDepositAlreadyProcessed {
        deposit_number: u64,
        zone_processed_number: u64,
    },
    #[error("pending deposit suffix is missing deposit {deposit_number}")]
    PendingDepositMissing { deposit_number: u64 },
    #[error(
        "Portal deposit cursor {deposit_number} has commitment {actual}, but pending suffix commits to {expected}"
    )]
    PortalDepositCommitmentMismatch {
        deposit_number: u64,
        expected: B256,
        actual: B256,
    },
    #[error(
        "failed-deposit origin {deposit_number} is beyond Zone processed cursor {zone_processed_number}"
    )]
    DepositOriginBeyondProcessedCursor {
        deposit_number: u64,
        zone_processed_number: u64,
    },
    #[error(
        "withdrawal origin {withdrawal_index} is not below next withdrawal index {next_withdrawal_index}"
    )]
    WithdrawalOriginBeyondNext {
        withdrawal_index: u64,
        next_withdrawal_index: u64,
    },
    #[error("fallback nonce {fallback_nonce} exceeds last assigned nonce {last_fallback_nonce}")]
    FallbackBeyondLastNonce {
        fallback_nonce: u64,
        last_fallback_nonce: u64,
    },
    #[error("batch {batch_index} exceeds last finalized batch {last_batch_index}")]
    BatchBeyondLastIndex {
        batch_index: u64,
        last_batch_index: u64,
    },
    #[error(
        "pending withdrawal {withdrawal_index} predates current batch start {batch_start_index}"
    )]
    PendingWithdrawalBeforeBatchStart {
        withdrawal_index: u64,
        batch_start_index: u64,
    },
    #[error("current batch is missing pending withdrawal {withdrawal_index}")]
    PendingWithdrawalMissing { withdrawal_index: u64 },
    #[error("current batch withdrawal {withdrawal_index} is already finalized")]
    CurrentWithdrawalAlreadyFinalized { withdrawal_index: u64 },
    #[error(
        "finalized withdrawal {withdrawal_index} is not owned by an open finalized/submitted batch"
    )]
    OrphanFinalizedWithdrawal { withdrawal_index: u64 },
    #[error(
        "user withdrawal {withdrawal_index} has no held fallback owner for nonce {fallback_nonce}"
    )]
    UserFallbackMissing {
        withdrawal_index: u64,
        fallback_nonce: u64,
    },
    #[error("user withdrawal {withdrawal_index} disagrees with fallback owner {fallback_nonce}")]
    UserFallbackMismatch {
        withdrawal_index: u64,
        fallback_nonce: u64,
    },
    #[error("queued bounce-back deposit {deposit_number} has no matching fallback owner")]
    BounceBackFallbackMissing { deposit_number: u64 },
    #[error("queued bounce-back deposit {deposit_number} disagrees with its fallback owner")]
    BounceBackFallbackMismatch { deposit_number: u64 },
    #[error("queued fallback {fallback_nonce} has no matching pending deposit {deposit_number}")]
    QueuedFallbackDepositMissing {
        fallback_nonce: u64,
        deposit_number: u64,
    },
    #[error("queued fallback {fallback_nonce} disagrees with pending deposit {deposit_number}")]
    QueuedFallbackDepositMismatch {
        fallback_nonce: u64,
        deposit_number: u64,
    },
    #[error("held fallback {fallback_nonce} has no matching user withdrawal")]
    HeldFallbackWithdrawalMissing { fallback_nonce: u64 },
    #[error("held fallback {fallback_nonce} disagrees with its user withdrawal")]
    HeldFallbackWithdrawalMismatch { fallback_nonce: u64 },
    #[error("failed deposit {deposit_number} has more than one open lifecycle owner")]
    DuplicateDepositOriginOwner { deposit_number: u64 },
    #[error("withdrawal origin {withdrawal_index} has more than one open lifecycle phase")]
    DuplicateWithdrawalOriginOwner { withdrawal_index: u64 },
    #[error("batch {batch_index} is on the wrong side of Portal submitted counter {portal_index}")]
    BatchPhaseCounterMismatch { batch_index: u64, portal_index: u64 },
    #[error(
        "batch {batch_index} withdrawal range ends at {range_end}, beyond next index {next_withdrawal_index}"
    )]
    BatchRangeBeyondNext {
        batch_index: u64,
        range_end: u64,
        next_withdrawal_index: u64,
    },
    #[error("batch {batch_index} withdrawal range overflows u64")]
    BatchRangeOverflow { batch_index: u64 },
    #[error(
        "submitted batch {batch_index} processing ordinal {ordinal} exceeds member count {member_count}"
    )]
    BatchProcessingOrdinalBeyondMembers {
        batch_index: u64,
        ordinal: u64,
        member_count: u64,
    },
    #[error(
        "batch {batch_index} starts at withdrawal {actual}, but the open batch suffix requires {expected}"
    )]
    OpenBatchRangeDiscontinuity {
        batch_index: u64,
        expected: u64,
        actual: u64,
    },
    #[error(
        "last open batch {batch_index} ends at withdrawal {range_end}, but the Zone accumulator starts at {batch_start_index}"
    )]
    OpenBatchTerminalRangeDiscontinuity {
        batch_index: u64,
        range_end: u64,
        batch_start_index: u64,
    },
    #[error(
        "submitted batch {batch_index} does not continue submitted batch {previous_batch_index}'s Zone block boundary"
    )]
    SubmittedBatchBlockDiscontinuity {
        previous_batch_index: u64,
        batch_index: u64,
    },
    #[error(
        "submitted batch {batch_index} does not continue submitted batch {previous_batch_index}'s deposit boundary"
    )]
    SubmittedBatchDepositDiscontinuity {
        previous_batch_index: u64,
        batch_index: u64,
    },
    #[error(
        "submitted batch {batch_index} Zone height {current_height} does not advance by at least {minimum_advance} from submitted batch {previous_batch_index} at height {previous_height}"
    )]
    SubmittedBatchZoneHeightNotIncreasing {
        previous_batch_index: u64,
        batch_index: u64,
        previous_height: u64,
        current_height: u64,
        minimum_advance: u64,
    },
    #[error(
        "submitted batch {batch_index} imported Tempo height {current_height} does not advance by at least {minimum_advance} from submitted batch {previous_batch_index} at height {previous_height}"
    )]
    SubmittedBatchTempoHeightNotIncreasing {
        previous_batch_index: u64,
        batch_index: u64,
        previous_height: u64,
        current_height: u64,
        minimum_advance: u64,
    },
    #[error(
        "batch {batch_index} advances Zone height by {zone_advance}, but imported Tempo height by {tempo_advance}, from batch boundary {previous_batch_index}"
    )]
    BatchTipAdvanceMismatch {
        previous_batch_index: u64,
        batch_index: u64,
        zone_advance: u64,
        tempo_advance: u64,
    },
    #[error("unsubmitted batch suffix is missing batch {batch_index}")]
    UnsubmittedBatchMissing { batch_index: u64 },
    #[error("unsubmitted batch {batch_index} is already marked submitted")]
    UnsubmittedBatchAlreadySubmitted { batch_index: u64 },
    #[error("unsubmitted batch {batch_index} does not continue the prior Zone block boundary")]
    UnsubmittedBatchBlockDiscontinuity { batch_index: u64 },
    #[error("unsubmitted batch {batch_index} does not continue the prior deposit boundary")]
    UnsubmittedBatchDepositDiscontinuity { batch_index: u64 },
    #[error("unsubmitted batch {batch_index} does not advance Zone height")]
    UnsubmittedBatchZoneHeightNotIncreasing { batch_index: u64 },
    #[error("unsubmitted batch {batch_index} does not advance imported Tempo height")]
    UnsubmittedBatchTempoHeightNotIncreasing { batch_index: u64 },
    #[error("batch {batch_index} requires missing withdrawal {withdrawal_index}")]
    BatchWithdrawalMissing {
        batch_index: u64,
        withdrawal_index: u64,
    },
    #[error("batch {batch_index} withdrawal {withdrawal_index} is not finalized")]
    BatchWithdrawalNotFinalized {
        batch_index: u64,
        withdrawal_index: u64,
    },
    #[error("batch {batch_index} withdrawal queue commitment is inconsistent with its owners")]
    BatchQueueCommitmentMismatch { batch_index: u64 },
    #[error("withdrawal {withdrawal_index} belongs to more than one open batch")]
    DuplicateBatchWithdrawal { withdrawal_index: u64 },
    #[error("submitted batch {batch_index} has queue Portal {actual}, expected {expected}")]
    SubmittedQueuePortalMismatch {
        batch_index: u64,
        expected: Address,
        actual: Address,
    },
    #[error("submitted batch {batch_index} queue index {queue_index} is outside [{head}, {tail})")]
    SubmittedQueueIndexOutOfRange {
        batch_index: u64,
        queue_index: U256,
        head: U256,
        tail: U256,
    },
    #[error(
        "submitted batch {batch_index} at queue index {queue_index} has processing ordinal {ordinal} ahead of queue head {head}"
    )]
    SubmittedBatchProcessedAheadOfHead {
        batch_index: u64,
        queue_index: U256,
        head: U256,
        ordinal: u64,
    },
    #[error("submitted batches reuse Portal queue index {queue_index}")]
    DuplicateSubmittedQueueIndex { queue_index: U256 },
    #[error("Portal queue owner count {actual} does not match counter-derived length {expected}")]
    SubmittedQueueOwnerCountMismatch { expected: U256, actual: usize },
    #[error("Portal queue owner index gap: expected {expected}, got {actual}")]
    SubmittedQueueIndexGap { expected: U256, actual: U256 },
    #[error("Portal queue batch order is not increasing at batch {batch_index}")]
    SubmittedQueueBatchOrder { batch_index: u64 },
    #[error("latest open batch {batch_index} disagrees with the Zone batch accumulator")]
    LastBatchAccumulatorMismatch { batch_index: u64 },
    #[error("latest submitted batch {batch_index} disagrees with Portal settlement state")]
    SubmittedBatchSettlementMismatch { batch_index: u64 },
    #[error("Portal settlement boundary disagrees with the Zone batch accumulator")]
    PortalZoneAccumulatorMismatch,
    #[error("{ledger} refund aggregate overflow for token {token}, recipient {recipient}")]
    RefundAggregateOverflow {
        ledger: RefundLedger,
        token: Address,
        recipient: Address,
    },
    #[error("Portal refund credit for failed deposit {deposit_number} has a zero recipient")]
    ZeroPortalRefundRecipient { deposit_number: u64 },
    #[error("Inbox refund credit for withdrawal {withdrawal_index} has a zero recipient")]
    ZeroInboxRefundRecipient { withdrawal_index: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefundLedger {
    Portal,
    Inbox,
}

impl std::fmt::Display for RefundLedger {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Portal => formatter.write_str("Portal"),
            Self::Inbox => formatter.write_str("Inbox"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Cursor {
    kind: CursorKind,
    hash: B256,
    number: u64,
}

/// Validate all cross-row invariants required before a model becomes the
/// authoritative restart state.
pub(crate) fn validate_authoritative(state: &ModelState) -> Result<(), AuthoritativeStateError> {
    let PortalLifecycle::Created(portal) = &state.portal else {
        return validate_awaiting_creation(state);
    };

    counters::validate(state, portal)?;
    accounting::validate_token_basics(state, portal.identity().initial_token())?;
    owners::validate_deposits(
        state,
        portal.identity().portal(),
        portal.deposit_cursor().number(),
    )?;
    owners::validate_withdrawals(
        state,
        portal.identity().zone_id(),
        state.zone().processed_deposit_cursor().number(),
    )?;
    owners::validate_fallbacks(state, portal.identity().zone_id())?;
    batches::validate(state, portal)?;
    origins::validate(state)?;
    refunds::validate(
        state,
        portal.identity().portal(),
        portal.identity().zone_id(),
        state.zone().processed_deposit_cursor().number(),
    )?;
    accounting::validate_liabilities(state)
}

fn validate_awaiting_creation(state: &ModelState) -> Result<(), AuthoritativeStateError> {
    if state.zone != super::state::ZoneState::INITIAL
        || !state.tokens.is_empty()
        || !state.pending_deposits.is_empty()
        || !state.withdrawals.is_empty()
        || !state.batches.is_empty()
        || !state.fallback_owners.is_empty()
        || !state.portal_refunds.is_empty()
        || !state.inbox_refunds.is_empty()
    {
        return Err(AuthoritativeStateError::AwaitingCreationHasLifecycleState);
    }
    Ok(())
}

fn cursor(kind: CursorKind, hash: B256, number: u64) -> Result<Cursor, AuthoritativeStateError> {
    if number == 0 && !hash.is_zero() {
        return Err(AuthoritativeStateError::ZeroCursorHasCommitment { cursor: kind, hash });
    }
    Ok(Cursor { kind, hash, number })
}

fn require_cursor_prefix(earlier: Cursor, later: Cursor) -> Result<(), AuthoritativeStateError> {
    if earlier.number > later.number {
        return Err(AuthoritativeStateError::CursorOrder {
            earlier: earlier.kind,
            earlier_number: earlier.number,
            later: later.kind,
            later_number: later.number,
        });
    }
    if earlier.number == later.number && earlier.hash != later.hash {
        return Err(AuthoritativeStateError::CursorCommitmentMismatch {
            earlier: earlier.kind,
            later: later.kind,
            number: earlier.number,
            earlier_hash: earlier.hash,
            later_hash: later.hash,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
