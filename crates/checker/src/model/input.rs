//! Checker-owned semantic inputs and authenticated branch outcomes.

// Construction stays inside the pure-model boundary. Goal 5 will add the
// narrow projection from Goal 1's adapter-owned observation types; do not make
// these constructors a crate-wide authenticated-value relabeling API.

use alloy_primitives::{Address, B256, Bytes, U256};

use super::{
    encoding::{DepositQueueMember, OrdinaryDeposit, UserWithdrawalRequest, Withdrawal},
    ownership::DepositCursor,
    state::PortalIdentity,
};

/// Exact token metadata carried from one authenticated Portal enablement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TokenEnable {
    token: Address,
    name: String,
    symbol: String,
    currency: String,
}

impl TokenEnable {
    pub(super) fn new(
        token: Address,
        name: impl Into<String>,
        symbol: impl Into<String>,
        currency: impl Into<String>,
    ) -> Self {
        Self {
            token,
            name: name.into(),
            symbol: symbol.into(),
            currency: currency.into(),
        }
    }

    pub(crate) const fn token(&self) -> Address {
        self.token
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn symbol(&self) -> &str {
        &self.symbol
    }

    pub(crate) fn currency(&self) -> &str {
        &self.currency
    }
}

/// Atomic semantic view of the constructor `TokenEnabled` followed by the
/// factory `ZoneCreated` outcome in the same successful transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PortalCreationInput {
    identity: PortalIdentity,
    initial_token_enable: TokenEnable,
}

impl PortalCreationInput {
    pub(super) const fn new(identity: PortalIdentity, initial_token_enable: TokenEnable) -> Self {
        Self {
            identity,
            initial_token_enable,
        }
    }

    pub(super) const fn identity(&self) -> PortalIdentity {
        self.identity
    }

    pub(super) const fn initial_token_enable(&self) -> &TokenEnable {
        &self.initial_token_enable
    }
}

/// Block-hash pair supplied by one authenticated `submitBatch` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BatchBlockTransitionInput {
    previous: B256,
    next: B256,
}

impl BatchBlockTransitionInput {
    pub(super) const fn new(previous: B256, next: B256) -> Self {
        Self { previous, next }
    }

    pub(super) const fn previous(&self) -> B256 {
        self.previous
    }

    pub(super) const fn next(&self) -> B256 {
        self.next
    }
}

/// Processed-deposit cursor pair supplied by one authenticated `submitBatch` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BatchDepositTransitionInput {
    previous: DepositCursor,
    next: DepositCursor,
}

impl BatchDepositTransitionInput {
    pub(super) const fn new(previous: DepositCursor, next: DepositCursor) -> Self {
        Self { previous, next }
    }

    pub(super) const fn previous(&self) -> DepositCursor {
        self.previous
    }

    pub(super) const fn next(&self) -> DepositCursor {
        self.next
    }
}

/// Settlement fields from one authenticated direct `submitBatch` call.
///
/// Proof, quorum, verifier, and anchoring data are intentionally absent: the
/// release-one model trusts their successful execution and independently
/// checks only the fields that advance logical settlement state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BatchSubmissionInput {
    tempo_block_number: u64,
    block_transition: BatchBlockTransitionInput,
    deposit_transition: BatchDepositTransitionInput,
    withdrawal_queue_hash: B256,
    next_zone_height: U256,
}

impl BatchSubmissionInput {
    pub(super) const fn new(
        tempo_block_number: u64,
        block_transition: BatchBlockTransitionInput,
        deposit_transition: BatchDepositTransitionInput,
        withdrawal_queue_hash: B256,
        next_zone_height: U256,
    ) -> Self {
        Self {
            tempo_block_number,
            block_transition,
            deposit_transition,
            withdrawal_queue_hash,
            next_zone_height,
        }
    }

    pub(super) const fn tempo_block_number(&self) -> u64 {
        self.tempo_block_number
    }

    pub(super) const fn block_transition(&self) -> BatchBlockTransitionInput {
        self.block_transition
    }

    pub(super) const fn deposit_transition(&self) -> BatchDepositTransitionInput {
        self.deposit_transition
    }

    pub(super) const fn withdrawal_queue_hash(&self) -> B256 {
        self.withdrawal_queue_hash
    }

    pub(super) const fn next_zone_height(&self) -> U256 {
        self.next_zone_height
    }
}

/// Authenticated aggregate refund-claim event fields.
///
/// Recipient and token select the per-origin owner prefix. The model derives
/// the aggregate independently and requires it to equal `amount`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RefundClaimInput {
    recipient: Address,
    token: Address,
    amount: u128,
}

impl RefundClaimInput {
    pub(super) const fn new(recipient: Address, token: Address, amount: u128) -> Self {
        Self {
            recipient,
            token,
            amount,
        }
    }

    pub(super) const fn recipient(&self) -> Address {
        self.recipient
    }

    pub(super) const fn token(&self) -> Address {
        self.token
    }

    pub(super) const fn amount(&self) -> u128 {
        self.amount
    }
}

/// Authenticated implementation branch for one calldata withdrawal.
///
/// Callback-created ordinary deposits are retained in their exact nested log
/// order. Every other field of the terminal Portal event is derived from the
/// finalized withdrawal and returned as an expectation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AuthenticatedWithdrawalOutcome {
    UserDelivered {
        callback_deposits: Vec<OrdinaryDeposit>,
    },
    UserBounced,
    FailedDepositPaid,
    FailedDepositPending,
}

impl AuthenticatedWithdrawalOutcome {
    pub(super) fn user_delivered(callback_deposits: Vec<OrdinaryDeposit>) -> Self {
        Self::UserDelivered { callback_deposits }
    }
}

/// One authenticated direct `processWithdrawals` call and its per-member
/// branch outcomes. The transition checks the two vectors have equal length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WithdrawalProcessingInput {
    withdrawals: Vec<Withdrawal>,
    remaining_queue: B256,
    outcomes: Vec<AuthenticatedWithdrawalOutcome>,
}

impl WithdrawalProcessingInput {
    pub(super) fn new(
        withdrawals: Vec<Withdrawal>,
        remaining_queue: B256,
        outcomes: Vec<AuthenticatedWithdrawalOutcome>,
    ) -> Self {
        Self {
            withdrawals,
            remaining_queue,
            outcomes,
        }
    }

    pub(super) fn withdrawals(&self) -> &[Withdrawal] {
        &self.withdrawals
    }

    pub(super) const fn remaining_queue(&self) -> B256 {
        self.remaining_queue
    }

    pub(super) fn outcomes(&self) -> &[AuthenticatedWithdrawalOutcome] {
        &self.outcomes
    }
}

/// One authenticated Portal operation in exact receipt/log order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ImportedTempoOperation {
    Create(PortalCreationInput),
    TokenEnabled(TokenEnable),
    BouncebackGasUpdated(u64),
    OrdinaryDepositAppended(OrdinaryDeposit),
    BatchSubmitted(Box<BatchSubmissionInput>),
    WithdrawalsProcessed(Box<WithdrawalProcessingInput>),
    PortalRefundClaimed(RefundClaimInput),
}

/// Ordered model-driving operations from one authenticated Tempo block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportedTempoBlockInput {
    tempo_block_number: u64,
    base_fee: U256,
    operations: Vec<ImportedTempoOperation>,
}

impl ImportedTempoBlockInput {
    pub(super) fn new(
        tempo_block_number: u64,
        base_fee: U256,
        operations: Vec<ImportedTempoOperation>,
    ) -> Self {
        Self {
            tempo_block_number,
            base_fee,
            operations,
        }
    }

    pub(super) const fn tempo_block_number(&self) -> u64 {
        self.tempo_block_number
    }

    pub(super) const fn base_fee(&self) -> U256 {
        self.base_fee
    }

    pub(super) fn operations(&self) -> &[ImportedTempoOperation] {
        &self.operations
    }
}

/// Authenticated implementation branch data the model deliberately cannot
/// predict. Deterministic event fields are returned separately as expectations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthenticatedDepositOutcome {
    OrdinaryMinted { recipient: Address, memo: B256 },
    OrdinaryFailed,
    WithdrawalBounceBackMinted { recipient: Address },
    WithdrawalBounceBackPending { recipient: Address },
}

/// Authenticated Zone input prefix and one branch selector per member.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ZoneDepositPrefixInput {
    enabled_tokens: Vec<TokenEnable>,
    deposits: Vec<DepositQueueMember>,
    outcomes: Vec<AuthenticatedDepositOutcome>,
}

impl ZoneDepositPrefixInput {
    pub(super) fn new(
        enabled_tokens: Vec<TokenEnable>,
        deposits: Vec<DepositQueueMember>,
        outcomes: Vec<AuthenticatedDepositOutcome>,
    ) -> Self {
        Self {
            enabled_tokens,
            deposits,
            outcomes,
        }
    }

    pub(super) fn enabled_tokens(&self) -> &[TokenEnable] {
        &self.enabled_tokens
    }

    pub(super) fn deposits(&self) -> &[DepositQueueMember] {
        &self.deposits
    }

    pub(super) fn outcomes(&self) -> &[AuthenticatedDepositOutcome] {
        &self.outcomes
    }
}

/// Canonical coordinates of the Zone block currently being modeled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ZoneBlockContext {
    block_hash: B256,
    block_number: u64,
}

impl ZoneBlockContext {
    pub(super) const fn new(block_hash: B256, block_number: u64) -> Self {
        Self {
            block_hash,
            block_number,
        }
    }

    pub(super) const fn block_hash(&self) -> B256 {
        self.block_hash
    }

    pub(super) const fn block_number(&self) -> u64 {
        self.block_number
    }
}

/// Authenticated request-like fields for one successful user withdrawal.
/// Index, fee, and fallback nonce remain outputs derived by the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UserWithdrawalInput {
    sender: Address,
    containing_transaction_hash: B256,
    request: UserWithdrawalRequest,
    reveal_to: Bytes,
}

impl UserWithdrawalInput {
    pub(super) const fn new(
        sender: Address,
        containing_transaction_hash: B256,
        request: UserWithdrawalRequest,
        reveal_to: Bytes,
    ) -> Self {
        Self {
            sender,
            containing_transaction_hash,
            request,
            reveal_to,
        }
    }

    pub(super) const fn sender(&self) -> Address {
        self.sender
    }

    pub(super) const fn containing_transaction_hash(&self) -> B256 {
        self.containing_transaction_hash
    }

    pub(super) const fn request(&self) -> &UserWithdrawalRequest {
        &self.request
    }

    pub(super) const fn reveal_to(&self) -> &Bytes {
        &self.reveal_to
    }
}

/// One successful post-advance Outbox operation in exact log order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ZoneOperation {
    TempoGasRateUpdated(u128),
    MaxWithdrawalsPerBlockUpdated(u32),
    UserWithdrawalAccepted(Box<UserWithdrawalInput>),
    InboxRefundClaimed(RefundClaimInput),
}

impl ZoneOperation {
    pub(super) fn user_withdrawal_accepted(input: UserWithdrawalInput) -> Self {
        Self::UserWithdrawalAccepted(Box::new(input))
    }
}

/// Authenticated calldata of the optional unique final system transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BatchFinalizationInput {
    declared_count: usize,
    block_number: u64,
    encrypted_senders: Vec<Bytes>,
}

impl BatchFinalizationInput {
    pub(super) fn new(
        declared_count: usize,
        block_number: u64,
        encrypted_senders: Vec<Bytes>,
    ) -> Self {
        Self {
            declared_count,
            block_number,
            encrypted_senders,
        }
    }

    pub(super) const fn declared_count(&self) -> usize {
        self.declared_count
    }

    pub(super) const fn block_number(&self) -> u64 {
        self.block_number
    }

    pub(super) fn encrypted_senders(&self) -> &[Bytes] {
        &self.encrypted_senders
    }
}

/// Complete authenticated input for one canonical post-genesis Zone block.
/// Keeping finalization separate makes its last-position requirement structural
/// after Goal 1 authenticates the system envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ZoneBlockInput {
    context: ZoneBlockContext,
    advance: ZoneDepositPrefixInput,
    operations: Vec<ZoneOperation>,
    finalization: Option<BatchFinalizationInput>,
}

impl ZoneBlockInput {
    pub(super) fn new(
        context: ZoneBlockContext,
        advance: ZoneDepositPrefixInput,
        operations: Vec<ZoneOperation>,
        finalization: Option<BatchFinalizationInput>,
    ) -> Self {
        Self {
            context,
            advance,
            operations,
            finalization,
        }
    }

    pub(super) const fn context(&self) -> ZoneBlockContext {
        self.context
    }

    pub(super) const fn advance(&self) -> &ZoneDepositPrefixInput {
        &self.advance
    }

    pub(super) fn operations(&self) -> &[ZoneOperation] {
        &self.operations
    }

    pub(super) const fn finalization(&self) -> Option<&BatchFinalizationInput> {
        self.finalization.as_ref()
    }
}
