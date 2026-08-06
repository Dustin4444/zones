//! Materialized checker state for the pure logical model.

use std::collections::BTreeMap;

use alloy_primitives::{Address, B256};

use super::{
    accounting::TokenAccounting,
    constants::{INITIAL_CONFIG, ZONE_PORTAL_ADDRESS_PREFIX},
    ownership::{
        DepositId, DepositOwner, FallbackId, FallbackOwner, InboxRefundId, InboxRefundOwner,
        WithdrawalId, WithdrawalOwner,
    },
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
}

impl CreatedPortalState {
    pub(crate) const fn identity(&self) -> PortalIdentity {
        self.identity
    }

    pub(crate) const fn config(&self) -> PortalConfig {
        self.config
    }

    pub(crate) const fn deposit_cursor(&self) -> PortalDepositCursor {
        self.deposit_cursor
    }
}

/// Portal identity cannot be used before its authenticated creation transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PortalLifecycle {
    AwaitingCreation { expected: PortalIdentity },
    Created(CreatedPortalState),
}

impl PortalLifecycle {
    pub(crate) const fn created(&self) -> Option<&CreatedPortalState> {
        match self {
            Self::AwaitingCreation { .. } => None,
            Self::Created(portal) => Some(portal),
        }
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
    pub(crate) const fn phase(&self) -> TokenPhase {
        self.phase
    }

    pub(crate) const fn accounting(&self) -> TokenAccounting {
        self.accounting
    }

    pub(crate) const fn is_zone_enabled(&self) -> bool {
        matches!(self.phase, TokenPhase::ZoneEnabled)
    }
}

/// Zone-side monotonic state needed by the deposit-prefix transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ZoneState {
    pub(super) processed_deposit_cursor: ZoneProcessedDepositCursor,
    pub(super) next_withdrawal_index: u64,
}

impl ZoneState {
    pub(crate) const fn processed_deposit_cursor(&self) -> ZoneProcessedDepositCursor {
        self.processed_deposit_cursor
    }

    pub(crate) const fn next_withdrawal_index(&self) -> u64 {
        self.next_withdrawal_index
    }
}

/// Authoritative materialized state at one verified logical cut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelState {
    pub(super) portal: PortalLifecycle,
    pub(super) zone: ZoneState,
    pub(super) tokens: BTreeMap<Address, TokenState>,
    pub(super) pending_deposits: BTreeMap<DepositId, DepositOwner>,
    pub(super) withdrawals: BTreeMap<WithdrawalId, WithdrawalOwner>,
    pub(super) fallback_owners: BTreeMap<FallbackId, FallbackOwner>,
    pub(super) inbox_refunds: BTreeMap<InboxRefundId, InboxRefundOwner>,
}

impl ModelState {
    pub(crate) fn awaiting_creation(expected: PortalIdentity) -> Self {
        Self {
            portal: PortalLifecycle::AwaitingCreation { expected },
            zone: ZoneState {
                processed_deposit_cursor: ZoneProcessedDepositCursor::ZERO,
                next_withdrawal_index: 0,
            },
            tokens: BTreeMap::new(),
            pending_deposits: BTreeMap::new(),
            withdrawals: BTreeMap::new(),
            fallback_owners: BTreeMap::new(),
            inbox_refunds: BTreeMap::new(),
        }
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

    pub(crate) fn pending_deposit(&self, id: DepositId) -> Option<&DepositOwner> {
        self.pending_deposits.get(&id)
    }

    pub(crate) fn pending_deposits(&self) -> &BTreeMap<DepositId, DepositOwner> {
        &self.pending_deposits
    }

    pub(crate) fn withdrawal(&self, id: WithdrawalId) -> Option<&WithdrawalOwner> {
        self.withdrawals.get(&id)
    }

    pub(crate) fn fallback_owner(&self, id: FallbackId) -> Option<&FallbackOwner> {
        self.fallback_owners.get(&id)
    }

    pub(crate) fn inbox_refund(&self, id: InboxRefundId) -> Option<&InboxRefundOwner> {
        self.inbox_refunds.get(&id)
    }
}

#[cfg(test)]
impl ModelState {
    pub(crate) fn seed_fallback_owner_for_test(&mut self, id: FallbackId, owner: FallbackOwner) {
        assert!(self.fallback_owners.insert(id, owner).is_none());
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

    pub(crate) fn set_next_withdrawal_index_for_test(&mut self, next: u64) {
        self.zone.next_withdrawal_index = next;
    }

    pub(crate) fn seed_inbox_refund_for_test(
        &mut self,
        id: InboxRefundId,
        owner: InboxRefundOwner,
    ) {
        assert!(self.inbox_refunds.insert(id, owner).is_none());
    }
}
