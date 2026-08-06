//! Materialized checker state for the pure logical model.

use std::collections::BTreeMap;

#[cfg(test)]
use std::num::NonZeroU64;

use alloy_primitives::{Address, B256, U256};

use super::{
    accounting::TokenAccounting,
    constants::{INITIAL_CONFIG, ZONE_PORTAL_ADDRESS_PREFIX},
    ownership::{
        BatchId, BatchOwner, DepositCursor, DepositId, DepositOwner, FallbackId, FallbackOwner,
        InboxRefundId, InboxRefundOwner, PortalRefundId, PortalRefundOwner, RefundAccount,
        WithdrawalId, WithdrawalOwner,
    },
    validation::{AuthoritativeStateError, validate_authoritative},
};

/// Immutable identity expected from the configured creation transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PortalIdentity {
    portal: Address,
    zone_id: u32,
    initial_token: Address,
}

impl PortalIdentity {
    pub(crate) const fn new(portal: Address, zone_id: u32, initial_token: Address) -> Self {
        Self {
            portal,
            zone_id,
            initial_token,
        }
    }

    pub(crate) const fn portal(&self) -> Address {
        self.portal
    }

    pub(crate) const fn zone_id(&self) -> u32 {
        self.zone_id
    }

    pub(crate) const fn initial_token(&self) -> Address {
        self.initial_token
    }
}

/// Independently derive the native ZoneFactory portal address for a Zone ID.
pub(crate) fn portal_address_for_zone(zone_id: u32) -> Address {
    let mut bytes = [0_u8; 20];
    bytes[..ZONE_PORTAL_ADDRESS_PREFIX.len()].copy_from_slice(&ZONE_PORTAL_ADDRESS_PREFIX);
    bytes[12..].copy_from_slice(&u64::from(zone_id).to_be_bytes());
    Address::from(bytes)
}

/// Ordered Portal configuration used by later settlement transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PortalConfig {
    pub(super) bounceback_gas: u64,
}

impl PortalConfig {
    pub(crate) const INITIAL: Self = Self {
        bounceback_gas: INITIAL_CONFIG.bounceback_gas,
    };

    pub(crate) const fn new(bounceback_gas: u64) -> Self {
        Self { bounceback_gas }
    }

    pub(crate) const fn bounceback_gas(&self) -> u64 {
        self.bounceback_gas
    }
}

/// Append-only Portal deposit queue cursor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PortalDepositCursor {
    hash: B256,
    number: u64,
}

impl PortalDepositCursor {
    pub(crate) const ZERO: Self = Self {
        hash: B256::ZERO,
        number: 0,
    };

    pub(crate) const fn new(hash: B256, number: u64) -> Self {
        Self { hash, number }
    }

    pub(crate) const fn hash(&self) -> B256 {
        self.hash
    }

    pub(crate) const fn number(&self) -> u64 {
        self.number
    }
}

/// Monotonic settlement and withdrawal-ring progress owned by the Portal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PortalSettlementState {
    pub(super) withdrawal_batch_index: u64,
    pub(super) block_hash: B256,
    pub(super) last_synced_tempo_block_number: u64,
    pub(super) last_submitted_deposit_cursor: DepositCursor,
    pub(super) zone_height: U256,
    pub(super) withdrawal_queue_head: U256,
    pub(super) withdrawal_queue_tail: U256,
}

impl PortalSettlementState {
    /// A newly created Portal has not accepted a batch or occupied a ring slot.
    pub(crate) const ZERO: Self = Self {
        withdrawal_batch_index: 0,
        block_hash: B256::ZERO,
        last_synced_tempo_block_number: 0,
        last_submitted_deposit_cursor: DepositCursor {
            hash: B256::ZERO,
            number: 0,
        },
        zone_height: U256::ZERO,
        withdrawal_queue_head: U256::ZERO,
        withdrawal_queue_tail: U256::ZERO,
    };

    pub(crate) const fn new(
        withdrawal_batch_index: u64,
        block_hash: B256,
        last_synced_tempo_block_number: u64,
        last_submitted_deposit_cursor: DepositCursor,
        zone_height: U256,
        withdrawal_queue_head: U256,
        withdrawal_queue_tail: U256,
    ) -> Self {
        Self {
            withdrawal_batch_index,
            block_hash,
            last_synced_tempo_block_number,
            last_submitted_deposit_cursor,
            zone_height,
            withdrawal_queue_head,
            withdrawal_queue_tail,
        }
    }

    pub(crate) const fn withdrawal_batch_index(&self) -> u64 {
        self.withdrawal_batch_index
    }

    pub(crate) const fn block_hash(&self) -> B256 {
        self.block_hash
    }

    pub(crate) const fn last_synced_tempo_block_number(&self) -> u64 {
        self.last_synced_tempo_block_number
    }

    pub(crate) const fn last_submitted_deposit_cursor(&self) -> DepositCursor {
        self.last_submitted_deposit_cursor
    }

    pub(crate) const fn zone_height(&self) -> U256 {
        self.zone_height
    }

    pub(crate) const fn withdrawal_queue_head(&self) -> U256 {
        self.withdrawal_queue_head
    }

    pub(crate) const fn withdrawal_queue_tail(&self) -> U256 {
        self.withdrawal_queue_tail
    }
}

/// Oldest-prefix cursor consumed by the Zone Inbox.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ZoneProcessedDepositCursor {
    hash: B256,
    number: u64,
}

impl ZoneProcessedDepositCursor {
    pub(crate) const ZERO: Self = Self {
        hash: B256::ZERO,
        number: 0,
    };

    pub(crate) const fn new(hash: B256, number: u64) -> Self {
        Self { hash, number }
    }

    pub(crate) const fn hash(&self) -> B256 {
        self.hash
    }

    pub(crate) const fn number(&self) -> u64 {
        self.number
    }
}

/// Created Portal state. Creation installs only literal protocol baselines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CreatedPortalState {
    pub(super) identity: PortalIdentity,
    pub(super) config: PortalConfig,
    pub(super) deposit_cursor: PortalDepositCursor,
    pub(super) settlement: PortalSettlementState,
}

impl CreatedPortalState {
    pub(crate) const fn new(
        identity: PortalIdentity,
        config: PortalConfig,
        deposit_cursor: PortalDepositCursor,
        settlement: PortalSettlementState,
    ) -> Self {
        Self {
            identity,
            config,
            deposit_cursor,
            settlement,
        }
    }

    pub(crate) const fn identity(&self) -> PortalIdentity {
        self.identity
    }

    pub(crate) const fn config(&self) -> PortalConfig {
        self.config
    }

    pub(crate) const fn deposit_cursor(&self) -> PortalDepositCursor {
        self.deposit_cursor
    }

    pub(crate) const fn settlement(&self) -> PortalSettlementState {
        self.settlement
    }
}

/// Portal identity cannot be used before its authenticated creation transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PortalLifecycle {
    AwaitingCreation { expected: PortalIdentity },
    Created(Box<CreatedPortalState>),
}

impl PortalLifecycle {
    pub(crate) const fn created(&self) -> Option<&CreatedPortalState> {
        match self {
            Self::AwaitingCreation { .. } => None,
            Self::Created(portal) => Some(portal),
        }
    }

    /// Configured identity remains authoritative before and after creation.
    pub(crate) const fn identity(&self) -> PortalIdentity {
        match self {
            Self::AwaitingCreation { expected } => *expected,
            Self::Created(portal) => portal.identity,
        }
    }

    /// Configured Zone identity is valid before the Portal creation block;
    /// Portal address and token operations must still use [`Self::created`].
    pub(crate) const fn zone_id(&self) -> u32 {
        self.identity().zone_id
    }
}

/// A token is always Portal-enabled before it can become Zone-enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenPhase {
    PendingZoneEnable,
    ZoneEnabled,
}

/// Per-token phase and exact `S/D/W` aggregates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TokenState {
    pub(super) phase: TokenPhase,
    pub(super) accounting: TokenAccounting,
}

impl TokenState {
    pub(crate) const fn new(phase: TokenPhase, accounting: TokenAccounting) -> Self {
        Self { phase, accounting }
    }

    pub(crate) const fn phase(&self) -> TokenPhase {
        self.phase
    }

    pub(crate) const fn accounting(&self) -> TokenAccounting {
        self.accounting
    }

    pub(crate) const fn is_zone_enabled(&self) -> bool {
        matches!(self.phase, TokenPhase::ZoneEnabled)
    }

    #[cfg(test)]
    pub(crate) const fn for_test(phase: TokenPhase, accounting: TokenAccounting) -> Self {
        Self { phase, accounting }
    }
}

/// Ordered Outbox configuration active at the current verified cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ZoneConfig {
    pub(super) tempo_gas_rate: u128,
    pub(super) max_withdrawals_per_block: u32,
}

impl ZoneConfig {
    pub(crate) const INITIAL: Self = Self {
        tempo_gas_rate: INITIAL_CONFIG.tempo_gas_rate,
        max_withdrawals_per_block: INITIAL_CONFIG.max_withdrawals_per_block,
    };

    pub(crate) const fn new(tempo_gas_rate: u128, max_withdrawals_per_block: u32) -> Self {
        Self {
            tempo_gas_rate,
            max_withdrawals_per_block,
        }
    }

    pub(crate) const fn tempo_gas_rate(&self) -> u128 {
        self.tempo_gas_rate
    }

    pub(crate) const fn max_withdrawals_per_block(&self) -> u32 {
        self.max_withdrawals_per_block
    }
}

/// Exact native Outbox `lastBatch` pair at the verified cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ZoneLastBatch {
    pub(super) withdrawal_queue_hash: B256,
    pub(super) withdrawal_batch_index: u64,
}

impl ZoneLastBatch {
    pub(crate) const ZERO: Self = Self {
        withdrawal_queue_hash: B256::ZERO,
        withdrawal_batch_index: 0,
    };

    pub(crate) const fn new(withdrawal_queue_hash: B256, withdrawal_batch_index: u64) -> Self {
        Self {
            withdrawal_queue_hash,
            withdrawal_batch_index,
        }
    }

    pub(crate) const fn withdrawal_queue_hash(&self) -> B256 {
        self.withdrawal_queue_hash
    }

    pub(crate) const fn withdrawal_batch_index(&self) -> u64 {
        self.withdrawal_batch_index
    }

    #[cfg(test)]
    pub(crate) const fn for_test(withdrawal_queue_hash: B256, withdrawal_batch_index: u64) -> Self {
        Self {
            withdrawal_queue_hash,
            withdrawal_batch_index,
        }
    }
}

/// Immutable start anchor of the currently accumulating withdrawal batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BatchStart {
    pub(super) first_zone_parent_hash: B256,
    pub(super) first_processed_deposit: ZoneProcessedDepositCursor,
    pub(super) first_withdrawal_index: u64,
}

impl BatchStart {
    /// The Portal's initial block/deposit transition anchors are literal zero;
    /// they are not inferred from the local Zone genesis block.
    pub(crate) const INITIAL: Self = Self {
        first_zone_parent_hash: B256::ZERO,
        first_processed_deposit: ZoneProcessedDepositCursor::ZERO,
        first_withdrawal_index: 0,
    };

    pub(crate) const fn new(
        first_zone_parent_hash: B256,
        first_processed_deposit: ZoneProcessedDepositCursor,
        first_withdrawal_index: u64,
    ) -> Self {
        Self {
            first_zone_parent_hash,
            first_processed_deposit,
            first_withdrawal_index,
        }
    }

    pub(crate) const fn first_zone_parent_hash(&self) -> B256 {
        self.first_zone_parent_hash
    }

    pub(crate) const fn first_processed_deposit(&self) -> ZoneProcessedDepositCursor {
        self.first_processed_deposit
    }

    pub(crate) const fn first_withdrawal_index(&self) -> u64 {
        self.first_withdrawal_index
    }
}

/// Zone-side configuration, counters, and current batch accumulator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ZoneState {
    pub(super) config: ZoneConfig,
    pub(super) processed_deposit_cursor: ZoneProcessedDepositCursor,
    pub(super) next_withdrawal_index: u64,
    pub(super) last_fallback_nonce: u64,
    pub(super) last_batch: ZoneLastBatch,
    pub(super) batch_start: BatchStart,
}

impl ZoneState {
    pub(crate) const INITIAL: Self = Self {
        config: ZoneConfig::INITIAL,
        processed_deposit_cursor: ZoneProcessedDepositCursor::ZERO,
        next_withdrawal_index: 0,
        last_fallback_nonce: 0,
        last_batch: ZoneLastBatch::ZERO,
        batch_start: BatchStart::INITIAL,
    };

    pub(crate) const fn new(
        config: ZoneConfig,
        processed_deposit_cursor: ZoneProcessedDepositCursor,
        next_withdrawal_index: u64,
        last_fallback_nonce: u64,
        last_batch: ZoneLastBatch,
        batch_start: BatchStart,
    ) -> Self {
        Self {
            config,
            processed_deposit_cursor,
            next_withdrawal_index,
            last_fallback_nonce,
            last_batch,
            batch_start,
        }
    }

    pub(crate) const fn config(&self) -> ZoneConfig {
        self.config
    }

    pub(crate) const fn processed_deposit_cursor(&self) -> ZoneProcessedDepositCursor {
        self.processed_deposit_cursor
    }

    pub(crate) const fn next_withdrawal_index(&self) -> u64 {
        self.next_withdrawal_index
    }

    pub(crate) const fn last_fallback_nonce(&self) -> u64 {
        self.last_fallback_nonce
    }

    pub(crate) const fn last_batch(&self) -> ZoneLastBatch {
        self.last_batch
    }

    pub(crate) const fn batch_start(&self) -> BatchStart {
        self.batch_start
    }
}

/// Deterministic acceleration index derived exclusively from open refund
/// origins. It is rebuilt after persistence decoding and never appears in
/// [`ModelStateParts`] or the checker database schema.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DerivedRefundTotals {
    portal: BTreeMap<RefundAccount, u128>,
    inbox: BTreeMap<RefundAccount, u128>,
}

impl DerivedRefundTotals {
    fn from_origins(
        portal_refunds: &BTreeMap<PortalRefundId, PortalRefundOwner>,
        inbox_refunds: &BTreeMap<InboxRefundId, InboxRefundOwner>,
    ) -> Self {
        let mut totals = Self::default();
        for (id, owner) in portal_refunds {
            let PortalRefundOwner::Pending { amount } = owner;
            totals.add_portal(
                RefundAccount {
                    token: id.token,
                    recipient: id.recipient,
                },
                *amount,
            );
        }
        for (id, owner) in inbox_refunds {
            let InboxRefundOwner::Pending { amount } = owner;
            totals.add_inbox(
                RefundAccount {
                    token: id.token,
                    recipient: id.recipient,
                },
                amount.get(),
            );
        }
        totals
    }

    fn portal(&self, account: RefundAccount) -> u128 {
        self.portal.get(&account).copied().unwrap_or_default()
    }

    fn inbox(&self, account: RefundAccount) -> u128 {
        self.inbox.get(&account).copied().unwrap_or_default()
    }

    fn add_portal(&mut self, account: RefundAccount, amount: u128) {
        Self::add(&mut self.portal, account, amount)
    }

    fn add_inbox(&mut self, account: RefundAccount, amount: u128) {
        Self::add(&mut self.inbox, account, amount)
    }

    fn add(totals: &mut BTreeMap<RefundAccount, u128>, account: RefundAccount, amount: u128) {
        let total = totals
            .get(&account)
            .copied()
            .unwrap_or_default()
            .checked_add(amount)
            .expect("refund aggregate overflow must be rejected before index update");
        if total != 0 {
            totals.insert(account, total);
        }
    }

    fn apply_updates(
        &mut self,
        portal: BTreeMap<RefundAccount, u128>,
        inbox: BTreeMap<RefundAccount, u128>,
    ) {
        Self::apply_ledger_updates(&mut self.portal, portal);
        Self::apply_ledger_updates(&mut self.inbox, inbox);
    }

    fn apply_ledger_updates(
        totals: &mut BTreeMap<RefundAccount, u128>,
        updates: BTreeMap<RefundAccount, u128>,
    ) {
        for (account, total) in updates {
            if total == 0 {
                totals.remove(&account);
            } else {
                totals.insert(account, total);
            }
        }
    }
}

/// Authoritative materialized state at one verified logical cut. The refund
/// total index is derived acceleration data; refund origins remain the sole
/// authoritative refund representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelState {
    pub(super) portal: PortalLifecycle,
    pub(super) zone: ZoneState,
    pub(super) tokens: BTreeMap<Address, TokenState>,
    pub(super) pending_deposits: BTreeMap<DepositId, DepositOwner>,
    pub(super) withdrawals: BTreeMap<WithdrawalId, WithdrawalOwner>,
    pub(super) batches: BTreeMap<BatchId, BatchOwner>,
    pub(super) fallback_owners: BTreeMap<FallbackId, FallbackOwner>,
    pub(super) portal_refunds: BTreeMap<PortalRefundId, PortalRefundOwner>,
    pub(super) inbox_refunds: BTreeMap<InboxRefundId, InboxRefundOwner>,
    derived_refund_totals: DerivedRefundTotals,
}

/// Semantic parts accepted at the model reconstruction boundary.
///
/// Storage adapters may populate this value, but only [`ModelState::from_parts`]
/// can turn it into authoritative state.
pub(crate) struct ModelStateParts {
    pub(crate) portal: PortalLifecycle,
    pub(crate) zone: ZoneState,
    pub(crate) tokens: BTreeMap<Address, TokenState>,
    pub(crate) pending_deposits: BTreeMap<DepositId, DepositOwner>,
    pub(crate) withdrawals: BTreeMap<WithdrawalId, WithdrawalOwner>,
    pub(crate) batches: BTreeMap<BatchId, BatchOwner>,
    pub(crate) fallback_owners: BTreeMap<FallbackId, FallbackOwner>,
    pub(crate) portal_refunds: BTreeMap<PortalRefundId, PortalRefundOwner>,
    pub(crate) inbox_refunds: BTreeMap<InboxRefundId, InboxRefundOwner>,
}

impl ModelState {
    pub(crate) fn awaiting_creation(expected: PortalIdentity) -> Self {
        Self {
            portal: PortalLifecycle::AwaitingCreation { expected },
            zone: ZoneState::INITIAL,
            tokens: BTreeMap::new(),
            pending_deposits: BTreeMap::new(),
            withdrawals: BTreeMap::new(),
            batches: BTreeMap::new(),
            fallback_owners: BTreeMap::new(),
            portal_refunds: BTreeMap::new(),
            inbox_refunds: BTreeMap::new(),
            derived_refund_totals: DerivedRefundTotals::default(),
        }
    }

    pub(crate) fn from_parts(parts: ModelStateParts) -> Result<Self, AuthoritativeStateError> {
        let mut state = Self {
            portal: parts.portal,
            zone: parts.zone,
            tokens: parts.tokens,
            pending_deposits: parts.pending_deposits,
            withdrawals: parts.withdrawals,
            batches: parts.batches,
            fallback_owners: parts.fallback_owners,
            portal_refunds: parts.portal_refunds,
            inbox_refunds: parts.inbox_refunds,
            derived_refund_totals: DerivedRefundTotals::default(),
        };
        validate_authoritative(&state)?;
        state.derived_refund_totals =
            DerivedRefundTotals::from_origins(&state.portal_refunds, &state.inbox_refunds);
        Ok(state)
    }

    pub(super) fn derived_portal_refund_total(&self, account: RefundAccount) -> u128 {
        self.derived_refund_totals.portal(account)
    }

    pub(super) fn derived_inbox_refund_total(&self, account: RefundAccount) -> u128 {
        self.derived_refund_totals.inbox(account)
    }

    pub(super) fn apply_derived_refund_total_updates(
        &mut self,
        portal: BTreeMap<RefundAccount, u128>,
        inbox: BTreeMap<RefundAccount, u128>,
    ) {
        self.derived_refund_totals.apply_updates(portal, inbox);
    }

    #[cfg(test)]
    pub(super) fn assert_derived_refund_totals_consistent(&self) {
        assert_eq!(
            self.derived_refund_totals,
            DerivedRefundTotals::from_origins(&self.portal_refunds, &self.inbox_refunds),
            "maintained refund-total index drifted from authoritative origins"
        );
    }

    pub(crate) const fn portal(&self) -> &PortalLifecycle {
        &self.portal
    }

    pub(crate) const fn zone(&self) -> &ZoneState {
        &self.zone
    }

    pub(crate) fn token(&self, token: Address) -> Option<&TokenState> {
        self.tokens.get(&token)
    }

    pub(crate) fn tokens(&self) -> &BTreeMap<Address, TokenState> {
        &self.tokens
    }

    pub(crate) fn pending_deposit(&self, id: DepositId) -> Option<&DepositOwner> {
        self.pending_deposits.get(&id)
    }

    pub(crate) fn pending_deposits(&self) -> &BTreeMap<DepositId, DepositOwner> {
        &self.pending_deposits
    }

    pub(crate) fn withdrawal(&self, id: WithdrawalId) -> Option<&WithdrawalOwner> {
        self.withdrawals.get(&id)
    }

    pub(crate) fn withdrawals(&self) -> &BTreeMap<WithdrawalId, WithdrawalOwner> {
        &self.withdrawals
    }

    pub(crate) fn batch(&self, id: BatchId) -> Option<&BatchOwner> {
        self.batches.get(&id)
    }

    pub(crate) fn batches(&self) -> &BTreeMap<BatchId, BatchOwner> {
        &self.batches
    }

    pub(crate) fn fallback_owner(&self, id: FallbackId) -> Option<&FallbackOwner> {
        self.fallback_owners.get(&id)
    }

    pub(crate) fn fallback_owners(&self) -> &BTreeMap<FallbackId, FallbackOwner> {
        &self.fallback_owners
    }

    pub(crate) fn portal_refund(&self, id: PortalRefundId) -> Option<&PortalRefundOwner> {
        self.portal_refunds.get(&id)
    }

    pub(crate) fn portal_refunds(&self) -> &BTreeMap<PortalRefundId, PortalRefundOwner> {
        &self.portal_refunds
    }

    #[cfg(test)]
    pub(crate) fn portal_refund_total(&self, account: RefundAccount) -> u128 {
        let portal = self.portal.identity().portal();
        let lower = PortalRefundId {
            token: account.token,
            recipient: account.recipient,
            failed_deposit: DepositId {
                portal,
                deposit_number: NonZeroU64::MIN,
            },
        };
        let upper = PortalRefundId {
            token: account.token,
            recipient: account.recipient,
            failed_deposit: DepositId {
                portal,
                deposit_number: NonZeroU64::new(u64::MAX).expect("u64::MAX is nonzero"),
            },
        };
        self.portal_refunds
            .range(lower..=upper)
            .try_fold(0_u128, |total, (_, owner)| {
                let PortalRefundOwner::Pending { amount } = owner;
                total.checked_add(*amount)
            })
            .expect("authoritative Portal refund credits cannot overflow")
    }

    pub(crate) fn inbox_refund(&self, id: InboxRefundId) -> Option<&InboxRefundOwner> {
        self.inbox_refunds.get(&id)
    }

    pub(crate) fn inbox_refunds(&self) -> &BTreeMap<InboxRefundId, InboxRefundOwner> {
        &self.inbox_refunds
    }

    #[cfg(test)]
    pub(crate) fn inbox_refund_total(&self, account: RefundAccount) -> u128 {
        let zone_id = self.portal.zone_id();
        let lower = InboxRefundId {
            token: account.token,
            recipient: account.recipient,
            user_withdrawal: WithdrawalId {
                zone_id,
                withdrawal_index: 0,
            },
        };
        let upper = InboxRefundId {
            token: account.token,
            recipient: account.recipient,
            user_withdrawal: WithdrawalId {
                zone_id,
                withdrawal_index: u64::MAX,
            },
        };
        self.inbox_refunds
            .range(lower..=upper)
            .try_fold(0_u128, |total, (_, owner)| {
                let InboxRefundOwner::Pending { amount } = owner;
                total.checked_add(amount.get())
            })
            .expect("authoritative Inbox refund credits cannot overflow")
    }
}

#[cfg(test)]
impl ModelState {
    pub(crate) fn created_with_zone_token_for_test(
        identity: PortalIdentity,
        accounting: TokenAccounting,
    ) -> Self {
        let mut state = Self::awaiting_creation(identity);
        state.portal = PortalLifecycle::Created(Box::new(CreatedPortalState {
            identity,
            config: PortalConfig::INITIAL,
            deposit_cursor: PortalDepositCursor::ZERO,
            settlement: PortalSettlementState::ZERO,
        }));
        state.tokens.insert(
            identity.initial_token(),
            TokenState {
                phase: TokenPhase::ZoneEnabled,
                accounting,
            },
        );
        state
    }

    pub(crate) fn seed_fallback_owner_for_test(&mut self, id: FallbackId, owner: FallbackOwner) {
        assert!(self.fallback_owners.insert(id, owner).is_none());
    }

    pub(crate) fn seed_pending_deposit_for_test(&mut self, id: DepositId, owner: DepositOwner) {
        assert!(self.pending_deposits.insert(id, owner).is_none());
    }

    pub(crate) fn seed_token_for_test(&mut self, token: Address, state: TokenState) {
        assert!(self.tokens.insert(token, state).is_none());
    }

    pub(crate) fn set_token_accounting_for_test(
        &mut self,
        token: Address,
        accounting: TokenAccounting,
    ) {
        self.tokens
            .get_mut(&token)
            .expect("fixture token must be enabled")
            .accounting = accounting;
    }

    pub(crate) fn set_portal_deposit_cursor_for_test(&mut self, cursor: PortalDepositCursor) {
        let PortalLifecycle::Created(portal) = &mut self.portal else {
            panic!("fixture portal must be created")
        };
        portal.deposit_cursor = cursor;
    }

    pub(crate) fn set_zone_processed_deposit_cursor_for_test(
        &mut self,
        cursor: ZoneProcessedDepositCursor,
    ) {
        self.zone.processed_deposit_cursor = cursor;
    }

    pub(crate) fn set_portal_withdrawal_queue_for_test(&mut self, head: U256, tail: U256) {
        let PortalLifecycle::Created(portal) = &mut self.portal else {
            panic!("fixture portal must be created")
        };
        portal.settlement.withdrawal_queue_head = head;
        portal.settlement.withdrawal_queue_tail = tail;
    }

    pub(crate) fn set_next_withdrawal_index_for_test(&mut self, next: u64) {
        self.zone.next_withdrawal_index = next;
    }

    pub(crate) fn set_last_fallback_nonce_for_test(&mut self, nonce: u64) {
        self.zone.last_fallback_nonce = nonce;
    }

    pub(crate) fn set_zone_config_for_test(
        &mut self,
        tempo_gas_rate: u128,
        max_withdrawals_per_block: u32,
    ) {
        self.zone.config = ZoneConfig {
            tempo_gas_rate,
            max_withdrawals_per_block,
        };
    }

    pub(crate) fn set_last_batch_for_test(&mut self, last_batch: ZoneLastBatch) {
        self.zone.last_batch = last_batch;
    }

    pub(crate) fn seed_withdrawal_for_test(&mut self, id: WithdrawalId, owner: WithdrawalOwner) {
        assert!(self.withdrawals.insert(id, owner).is_none());
    }

    pub(crate) fn seed_batch_for_test(&mut self, id: BatchId, owner: BatchOwner) {
        assert!(self.batches.insert(id, owner).is_none());
    }

    pub(crate) fn seed_inbox_refund_for_test(
        &mut self,
        id: InboxRefundId,
        owner: InboxRefundOwner,
    ) {
        assert!(!self.inbox_refunds.contains_key(&id));
        let InboxRefundOwner::Pending { amount } = &owner;
        self.derived_refund_totals.add_inbox(
            RefundAccount {
                token: id.token,
                recipient: id.recipient,
            },
            amount.get(),
        );
        assert!(self.inbox_refunds.insert(id, owner).is_none());
    }

    pub(crate) fn seed_portal_refund_for_test(
        &mut self,
        id: PortalRefundId,
        owner: PortalRefundOwner,
    ) {
        assert!(!self.portal_refunds.contains_key(&id));
        let PortalRefundOwner::Pending { amount } = &owner;
        self.derived_refund_totals.add_portal(
            RefundAccount {
                token: id.token,
                recipient: id.recipient,
            },
            *amount,
        );
        assert!(self.portal_refunds.insert(id, owner).is_none());
    }
}
