//! Independently derived outputs for later observation comparison.

use alloy_primitives::{Address, B256, Bytes};

use super::{
    encoding::OrdinaryDeposit,
    ownership::{DepositId, WithdrawalId},
    state::ZoneProcessedDepositCursor,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExpectedDepositAppend {
    id: DepositId,
    queue_hash: B256,
}

impl ExpectedDepositAppend {
    pub(super) const fn new(id: DepositId, queue_hash: B256) -> Self {
        Self { id, queue_hash }
    }

    pub(crate) const fn id(&self) -> DepositId {
        self.id
    }

    pub(crate) const fn queue_hash(&self) -> B256 {
        self.queue_hash
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ExpectedImportedTempoBlock {
    deposit_appends: Vec<ExpectedDepositAppend>,
}

impl ExpectedImportedTempoBlock {
    pub(super) fn push_deposit_append(&mut self, append: ExpectedDepositAppend) {
        self.deposit_appends.push(append);
    }

    pub(crate) fn deposit_appends(&self) -> &[ExpectedDepositAppend] {
        &self.deposit_appends
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExpectedTokenEnable {
    token: Address,
    name: String,
    symbol: String,
    currency: String,
}

impl ExpectedTokenEnable {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExpectedDepositProcessed {
    deposit_hash: B256,
    sender: Address,
    token: Address,
    amount: u128,
}

impl ExpectedDepositProcessed {
    pub(super) const fn new(
        deposit_hash: B256,
        sender: Address,
        token: Address,
        amount: u128,
    ) -> Self {
        Self {
            deposit_hash,
            sender,
            token,
            amount,
        }
    }

    pub(crate) const fn deposit_hash(&self) -> B256 {
        self.deposit_hash
    }

    pub(crate) const fn sender(&self) -> Address {
        self.sender
    }

    pub(crate) const fn token(&self) -> Address {
        self.token
    }

    pub(crate) const fn amount(&self) -> u128 {
        self.amount
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExpectedWithdrawalRequested {
    withdrawal: WithdrawalId,
    sender: Address,
    token: Address,
    to: Address,
    amount: u128,
    fee: u128,
    memo: B256,
    gas_limit: u64,
    fallback_nonce: u64,
    data: Bytes,
    reveal_to: Bytes,
}

impl ExpectedWithdrawalRequested {
    pub(super) fn for_failed_deposit(withdrawal: WithdrawalId, deposit: &OrdinaryDeposit) -> Self {
        Self {
            withdrawal,
            sender: Address::ZERO,
            token: deposit.token(),
            to: deposit.tempo_refund_recipient(),
            amount: deposit.amount(),
            fee: 0,
            memo: B256::ZERO,
            gas_limit: 0,
            fallback_nonce: 0,
            data: Bytes::new(),
            reveal_to: Bytes::new(),
        }
    }

    pub(crate) const fn withdrawal(&self) -> WithdrawalId {
        self.withdrawal
    }

    pub(crate) const fn sender(&self) -> Address {
        self.sender
    }

    pub(crate) const fn token(&self) -> Address {
        self.token
    }

    pub(crate) const fn to(&self) -> Address {
        self.to
    }

    pub(crate) const fn amount(&self) -> u128 {
        self.amount
    }

    pub(crate) const fn fee(&self) -> u128 {
        self.fee
    }

    pub(crate) const fn memo(&self) -> B256 {
        self.memo
    }

    pub(crate) const fn gas_limit(&self) -> u64 {
        self.gas_limit
    }

    pub(crate) const fn fallback_nonce(&self) -> u64 {
        self.fallback_nonce
    }

    pub(crate) const fn data(&self) -> &Bytes {
        &self.data
    }

    pub(crate) const fn reveal_to(&self) -> &Bytes {
        &self.reveal_to
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExpectedDepositFailed {
    deposit_hash: B256,
    sender: Address,
    token: Address,
    amount: u128,
}

impl ExpectedDepositFailed {
    pub(super) const fn from_ordinary(deposit_hash: B256, deposit: &OrdinaryDeposit) -> Self {
        Self {
            deposit_hash,
            sender: deposit.sender(),
            token: deposit.token(),
            amount: deposit.amount(),
        }
    }

    pub(crate) const fn deposit_hash(&self) -> B256 {
        self.deposit_hash
    }

    pub(crate) const fn sender(&self) -> Address {
        self.sender
    }

    pub(crate) const fn token(&self) -> Address {
        self.token
    }

    pub(crate) const fn amount(&self) -> u128 {
        self.amount
    }
}

/// Native failure order is Outbox `WithdrawalRequested` first, then Inbox
/// `DepositFailed`; the compound shape keeps that order explicit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExpectedOrdinaryDepositFailure {
    first: ExpectedWithdrawalRequested,
    second: ExpectedDepositFailed,
}

impl ExpectedOrdinaryDepositFailure {
    pub(super) const fn new(
        first: ExpectedWithdrawalRequested,
        second: ExpectedDepositFailed,
    ) -> Self {
        Self { first, second }
    }

    pub(crate) const fn first(&self) -> &ExpectedWithdrawalRequested {
        &self.first
    }

    pub(crate) const fn second(&self) -> &ExpectedDepositFailed {
        &self.second
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExpectedWithdrawalBounceBack {
    token: Address,
    amount: u128,
}

impl ExpectedWithdrawalBounceBack {
    pub(super) const fn new(token: Address, amount: u128) -> Self {
        Self { token, amount }
    }

    pub(crate) const fn token(&self) -> Address {
        self.token
    }

    pub(crate) const fn amount(&self) -> u128 {
        self.amount
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExpectedDepositOutcome {
    OrdinaryMinted(ExpectedDepositProcessed),
    OrdinaryFailed(Box<ExpectedOrdinaryDepositFailure>),
    WithdrawalBounceBackMinted(ExpectedWithdrawalBounceBack),
    WithdrawalBounceBackPending(ExpectedWithdrawalBounceBack),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExpectedZoneDepositPrefix {
    token_enables: Vec<ExpectedTokenEnable>,
    deposit_outcomes: Vec<ExpectedDepositOutcome>,
    deposits_processed: usize,
    processed_cursor: ZoneProcessedDepositCursor,
}

impl ExpectedZoneDepositPrefix {
    pub(super) fn new(
        token_enables: Vec<ExpectedTokenEnable>,
        deposit_outcomes: Vec<ExpectedDepositOutcome>,
        deposits_processed: usize,
        processed_cursor: ZoneProcessedDepositCursor,
    ) -> Self {
        Self {
            token_enables,
            deposit_outcomes,
            deposits_processed,
            processed_cursor,
        }
    }

    pub(crate) fn token_enables(&self) -> &[ExpectedTokenEnable] {
        &self.token_enables
    }

    pub(crate) fn deposit_outcomes(&self) -> &[ExpectedDepositOutcome] {
        &self.deposit_outcomes
    }

    pub(crate) const fn deposits_processed(&self) -> usize {
        self.deposits_processed
    }

    pub(crate) const fn processed_cursor(&self) -> ZoneProcessedDepositCursor {
        self.processed_cursor
    }
}

/// Complete Goal 2 expectations. Typestate construction makes both stages
/// mandatory and keeps authenticated branch values out of these fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExpectedOutputs {
    imported_tempo_block: ExpectedImportedTempoBlock,
    zone_deposit_prefix: ExpectedZoneDepositPrefix,
}

impl ExpectedOutputs {
    pub(super) const fn new(
        imported_tempo_block: ExpectedImportedTempoBlock,
        zone_deposit_prefix: ExpectedZoneDepositPrefix,
    ) -> Self {
        Self {
            imported_tempo_block,
            zone_deposit_prefix,
        }
    }

    pub(crate) const fn imported_tempo_block(&self) -> &ExpectedImportedTempoBlock {
        &self.imported_tempo_block
    }

    pub(crate) const fn zone_deposit_prefix(&self) -> &ExpectedZoneDepositPrefix {
        &self.zone_deposit_prefix
    }
}
