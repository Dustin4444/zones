//! Checker-owned semantic inputs and authenticated branch outcomes.

// Goal 2 keeps construction inside the pure-model boundary. Goal 5 will add
// the narrow projection from Goal 1's adapter-owned observation types; do not
// make these constructors a crate-wide authenticated-value relabeling API.

use alloy_primitives::{Address, B256, Bytes};

use super::{
    encoding::{
        DepositQueueMember, OrdinaryDeposit, UserWithdrawalRequest, WithdrawalBounceBackDeposit,
    },
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

/// One authenticated Portal operation in exact receipt/log order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ImportedTempoOperation {
    Create(PortalCreationInput),
    TokenEnabled(TokenEnable),
    BouncebackGasUpdated(u64),
    OrdinaryDepositAppended(OrdinaryDeposit),
    WithdrawalBounceBackAppended(WithdrawalBounceBackDeposit),
}

/// Ordered model-driving operations from one authenticated Tempo block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportedTempoBlockInput {
    tempo_block_number: u64,
    operations: Vec<ImportedTempoOperation>,
}

impl ImportedTempoBlockInput {
    pub(super) fn new(tempo_block_number: u64, operations: Vec<ImportedTempoOperation>) -> Self {
        Self {
            tempo_block_number,
            operations,
        }
    }

    pub(super) const fn tempo_block_number(&self) -> u64 {
        self.tempo_block_number
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
