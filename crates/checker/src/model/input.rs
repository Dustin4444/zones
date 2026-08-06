//! Checker-owned semantic inputs and authenticated branch outcomes.

// Goal 2 keeps construction inside the pure-model boundary. Goal 5 will add
// the narrow projection from Goal 1's adapter-owned observation types; do not
// make these constructors a crate-wide authenticated-value relabeling API.

use alloy_primitives::{Address, B256};

use super::{
    encoding::{DepositQueueMember, OrdinaryDeposit, WithdrawalBounceBackDeposit},
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ImportedTempoBlockInput {
    operations: Vec<ImportedTempoOperation>,
}

impl ImportedTempoBlockInput {
    pub(super) fn new(operations: Vec<ImportedTempoOperation>) -> Self {
        Self { operations }
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
