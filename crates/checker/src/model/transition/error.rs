//! Typed transition diagnostics.

use alloy_primitives::{Address, B256, U256};

use crate::model::{
    accounting::AccountingError,
    encoding::{WithdrawalDataError, WithdrawalQueueError},
    fees::FeeError,
    input::TokenEnable,
    ownership::{BatchStateError, DepositCursor, PortalQueueIdError},
    state::PortalIdentity,
};

/// Deposit origin expected by the oldest pending owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DepositKind {
    Ordinary,
    WithdrawalBounceBack,
}

/// Authenticated implementation branch selected for one consumed deposit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DepositOutcomeKind {
    OrdinaryMinted,
    OrdinaryFailed,
    WithdrawalBounceBackMinted,
    WithdrawalBounceBackPending,
}

/// Retained origin required by one Portal-processing branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WithdrawalOriginKind {
    User,
    FailedDeposit,
}

/// Authenticated branch selected for one direct processing item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WithdrawalProcessingOutcomeKind {
    UserDelivered,
    UserBounced,
    FailedDepositPaid,
    FailedDepositPending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BlockTransitionMismatch {
    pub(crate) expected_previous: B256,
    pub(crate) actual_previous: B256,
    pub(crate) expected_next: B256,
    pub(crate) actual_next: B256,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DepositTransitionMismatch {
    pub(crate) expected_previous: DepositCursor,
    pub(crate) actual_previous: DepositCursor,
    pub(crate) expected_next: DepositCursor,
    pub(crate) actual_next: DepositCursor,
}

/// Fail-closed logical transition errors. Acquisition and event decoding
/// failures remain in the observation adapter and never enter this enum.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ModelError {
    #[error("the Portal has not been authenticated as created")]
    PortalNotCreated,
    #[error("the Portal creation transition was already applied")]
    PortalAlreadyCreated,
    #[error("Portal creation identity mismatch: expected {expected:?}, got {actual:?}")]
    PortalIdentityMismatch {
        expected: PortalIdentity,
        actual: PortalIdentity,
    },
    #[error("configured Portal address {actual} does not match derived address {expected}")]
    PortalAddressMismatch { expected: Address, actual: Address },
    #[error("constructor TokenEnabled token mismatch: expected {expected}, got {actual}")]
    InitialTokenMismatch { expected: Address, actual: Address },
    #[error("token {token} is already enabled on the Portal")]
    TokenAlreadyEnabled { token: Address },
    #[error("token {token} is not enabled on the Portal")]
    TokenNotPortalEnabled { token: Address },
    #[error("token {token} is not enabled on the Zone")]
    TokenNotZoneEnabled { token: Address },
    #[error("ordinary deposit refund recipient is zero")]
    ZeroTempoRefundRecipient,
    #[error("Zone token enable count mismatch: expected {expected}, got {actual}")]
    ZoneTokenEnableCountMismatch { expected: usize, actual: usize },
    #[error(
        "Zone token enable at position {index} does not match the imported Portal event: expected {expected:?}, got {actual:?}"
    )]
    ZoneTokenEnableMismatch {
        index: usize,
        expected: Box<TokenEnable>,
        actual: Box<TokenEnable>,
    },
    #[error("Portal deposit number overflow")]
    PortalDepositNumberOverflow,
    #[error("Portal deposit {number} already has an open owner")]
    DepositOwnerCollision { number: u64 },
    #[error("withdrawal bounce-back nonce {fallback_nonce} has no matching fallback owner")]
    FallbackOwnerMissing { fallback_nonce: u64 },
    #[error("withdrawal bounce-back nonce {fallback_nonce} does not match its fallback owner")]
    FallbackOwnerMismatch { fallback_nonce: u64 },
    #[error("withdrawal {withdrawal_index} already has an open bounce-back deposit")]
    WithdrawalBounceBackAlreadyPending { withdrawal_index: u64 },
    #[error("deposit prefix/outcome count mismatch: {deposits} deposits, {outcomes} outcomes")]
    DepositOutcomeCountMismatch { deposits: usize, outcomes: usize },
    #[error("processed deposit number overflow")]
    ProcessedDepositNumberOverflow,
    #[error("pending deposit {number} does not exist")]
    PendingDepositMissing { number: u64 },
    #[error("deposit prefix member {number} does not match the oldest pending owner")]
    DepositPrefixMismatch { number: u64 },
    #[error("deposit {number} expected {expected:?} outcome, got authenticated {actual:?} outcome")]
    DepositOutcomeKindMismatch {
        number: u64,
        expected: DepositKind,
        actual: DepositOutcomeKind,
    },
    #[error("withdrawal index overflow")]
    WithdrawalIndexOverflow,
    #[error("withdrawal index {withdrawal_index} already has an open owner")]
    WithdrawalOwnerCollision { withdrawal_index: u64 },
    #[error("withdrawal cap {limit} was exceeded in this Zone block")]
    WithdrawalBlockCapExceeded { limit: u32 },
    #[error("fallback nonce overflow")]
    FallbackNonceOverflow,
    #[error("fallback nonce {fallback_nonce} already has an open owner")]
    FallbackOwnerCollision { fallback_nonce: u64 },
    #[error("finalization block number mismatch: expected {expected}, got {actual}")]
    FinalizationBlockNumberMismatch { expected: u64, actual: u64 },
    #[error("finalization count mismatch: expected {expected}, got {actual}")]
    FinalizationCountMismatch { expected: u64, actual: usize },
    #[error(
        "finalization encrypted-sender count mismatch: declared {declared}, got {actual} entries"
    )]
    FinalizationSenderCountMismatch { declared: usize, actual: usize },
    #[error("current batch withdrawal range is invalid: first {first}, next {next}")]
    InvalidBatchWithdrawalRange { first: u64, next: u64 },
    #[error("withdrawal {withdrawal_index} is missing from the current batch range")]
    WithdrawalOwnerMissing { withdrawal_index: u64 },
    #[error("withdrawal {withdrawal_index} was already finalized")]
    WithdrawalAlreadyFinalized { withdrawal_index: u64 },
    #[error("withdrawal batch index overflow")]
    WithdrawalBatchIndexOverflow,
    #[error("withdrawal batch index {withdrawal_batch_index} already has an open owner")]
    BatchOwnerCollision { withdrawal_batch_index: u64 },
    #[error("Portal withdrawal batch index overflow")]
    PortalBatchIndexOverflow,
    #[error("finalized batch {withdrawal_batch_index} does not exist for Portal submission")]
    BatchOwnerMissing { withdrawal_batch_index: u64 },
    #[error("batch {withdrawal_batch_index} was already submitted")]
    BatchAlreadySubmitted { withdrawal_batch_index: u64 },
    #[error(
        "batch {withdrawal_batch_index} Tempo block mismatch: expected {expected}, got {actual}"
    )]
    BatchTempoBlockMismatch {
        withdrawal_batch_index: u64,
        expected: u64,
        actual: u64,
    },
    #[error(
        "batch {withdrawal_batch_index} Zone height mismatch: expected {expected}, got {actual}"
    )]
    BatchZoneHeightMismatch {
        withdrawal_batch_index: u64,
        expected: U256,
        actual: U256,
    },
    #[error("batch {withdrawal_batch_index} block transition mismatch: {details:?}")]
    BatchBlockTransitionMismatch {
        withdrawal_batch_index: u64,
        details: Box<BlockTransitionMismatch>,
    },
    #[error("batch {withdrawal_batch_index} deposit transition mismatch: {details:?}")]
    BatchDepositTransitionMismatch {
        withdrawal_batch_index: u64,
        details: Box<DepositTransitionMismatch>,
    },
    #[error(
        "batch {withdrawal_batch_index} withdrawal commitment mismatch: expected {expected}, got {actual}"
    )]
    BatchWithdrawalQueueHashMismatch {
        withdrawal_batch_index: u64,
        expected: B256,
        actual: B256,
    },
    #[error(
        "batch {withdrawal_batch_index} starts from Portal block {actual}, expected {expected}"
    )]
    PortalBlockContinuityMismatch {
        withdrawal_batch_index: u64,
        expected: B256,
        actual: B256,
    },
    #[error(
        "batch {withdrawal_batch_index} starts from Portal deposit cursor {actual:?}, expected {expected:?}"
    )]
    PortalDepositContinuityMismatch {
        withdrawal_batch_index: u64,
        expected: DepositCursor,
        actual: DepositCursor,
    },
    #[error(
        "batch {withdrawal_batch_index} Zone height {next} does not exceed Portal height {previous}"
    )]
    PortalZoneHeightNotIncreasing {
        withdrawal_batch_index: u64,
        previous: U256,
        next: U256,
    },
    #[error(
        "batch {withdrawal_batch_index} submits deposit {submitted} beyond Portal deposit {deposited}"
    )]
    PortalDepositCursorBeyondQueue {
        withdrawal_batch_index: u64,
        submitted: u64,
        deposited: u64,
    },
    #[error("Portal withdrawal queue progress is invalid: head {head}, tail {tail}")]
    InvalidPortalWithdrawalQueueProgress { head: U256, tail: U256 },
    #[error("Portal withdrawal queue is full")]
    PortalWithdrawalQueueFull,
    #[error("Portal withdrawal queue counter overflow")]
    PortalWithdrawalQueueCounterOverflow,
    #[error(
        "processWithdrawals outcome count mismatch: {withdrawals} withdrawals, {outcomes} outcomes"
    )]
    WithdrawalProcessingOutcomeCountMismatch { withdrawals: usize, outcomes: usize },
    #[error("the Portal withdrawal queue is empty")]
    PortalWithdrawalQueueEmpty,
    #[error("the Portal withdrawal queue head has no open submitted batch")]
    PortalWithdrawalQueueHeadMissing,
    #[error("the first open batch is finalized while the Portal queue is non-empty")]
    PortalWithdrawalQueueHeadNotSubmitted,
    #[error("submitted batch belongs to Portal {actual}, expected {expected}")]
    PortalWithdrawalQueuePortalMismatch { expected: Address, actual: Address },
    #[error("submitted batch queue index mismatch: expected head {expected}, got {actual}")]
    PortalWithdrawalQueueHeadMismatch { expected: U256, actual: U256 },
    #[error("withdrawal processing prefix length does not fit u64: {actual}")]
    WithdrawalProcessingLengthOverflow { actual: usize },
    #[error(
        "withdrawal processing prefix exceeds batch remainder: remaining {remaining}, got {actual}"
    )]
    WithdrawalProcessingBeyondBatch { remaining: u64, actual: u64 },
    #[error("withdrawal processing exhausted the queue before the batch's last member")]
    WithdrawalProcessingExhaustedEarly,
    #[error("withdrawal processing left a suffix after consuming the batch's last member")]
    WithdrawalProcessingLeftSuffixAfterBatch,
    #[error("withdrawal {withdrawal_index} is not finalized for Portal processing")]
    WithdrawalNotFinalizedForProcessing { withdrawal_index: u64 },
    #[error("withdrawal {withdrawal_index} preimage does not match direct calldata")]
    WithdrawalProcessingPreimageMismatch { withdrawal_index: u64 },
    #[error(
        "withdrawal {withdrawal_index} expected {expected:?} processing outcome, got {actual:?}"
    )]
    WithdrawalProcessingOutcomeMismatch {
        withdrawal_index: u64,
        expected: WithdrawalOriginKind,
        actual: WithdrawalProcessingOutcomeKind,
    },
    #[error("non-callback withdrawal {withdrawal_index} cannot produce callback deposits")]
    CallbackDepositsWithoutCallback { withdrawal_index: u64 },
    #[error("Portal refund credit already exists for failed deposit {deposit_number}")]
    PortalRefundCollision { deposit_number: u64 },
    #[error("refund aggregate overflow for token {token} recipient {recipient}")]
    RefundAggregateOverflow { token: Address, recipient: Address },
    #[error(
        "Portal refund aggregate state mismatch for token {token} recipient {recipient}: expected {expected}, got {actual} from origins"
    )]
    PortalRefundAggregateStateMismatch {
        token: Address,
        recipient: Address,
        expected: u128,
        actual: u128,
    },
    #[error(
        "Inbox refund aggregate state mismatch for token {token} recipient {recipient}: expected {expected}, got {actual} from origins"
    )]
    InboxRefundAggregateStateMismatch {
        token: Address,
        recipient: Address,
        expected: u128,
        actual: u128,
    },
    #[error(
        "refund claim mismatch for token {token} recipient {recipient}: expected {expected}, got {actual}"
    )]
    RefundClaimAmountMismatch {
        token: Address,
        recipient: Address,
        expected: u128,
        actual: u128,
    },
    #[error("Inbox refund credit already exists for withdrawal {withdrawal_index}")]
    InboxRefundCollision { withdrawal_index: u64 },
    #[error("withdrawal {withdrawal_index} bounce-back outcome recipient is zero")]
    ZeroBounceBackRecipient { withdrawal_index: u64 },
    #[error(transparent)]
    Accounting(#[from] AccountingError),
    #[error(transparent)]
    Fee(#[from] FeeError),
    #[error(transparent)]
    WithdrawalData(#[from] WithdrawalDataError),
    #[error(transparent)]
    BatchState(#[from] BatchStateError),
    #[error(transparent)]
    PortalQueueId(#[from] PortalQueueIdError),
    #[error(transparent)]
    WithdrawalQueue(#[from] WithdrawalQueueError),
}
