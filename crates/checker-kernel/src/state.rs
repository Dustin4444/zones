use std::{collections::BTreeMap, num::NonZeroU64, ops::Bound};

use alloy_primitives::{Address, B256, Bytes, U256};
use serde::{Deserialize, Serialize};

use crate::facts::OrdinaryDeposit;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PortalIdentity {
    pub portal: Address,
    pub zone_id: u32,
    pub initial_token: Address,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DepositId {
    pub portal: Address,
    pub number: NonZeroU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WithdrawalId {
    pub zone_id: u32,
    pub index: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BatchId {
    pub zone_id: u32,
    pub index: NonZeroU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FallbackId {
    pub zone_id: u32,
    pub nonce: NonZeroU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PortalRefundId {
    pub token: Address,
    pub recipient: Address,
    pub deposit: DepositId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct InboxRefundId {
    pub token: Address,
    pub recipient: Address,
    pub withdrawal: WithdrawalId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum StateKey {
    Portal,
    Zone,
    Token(Address),
    Deposit(DepositId),
    Withdrawal(WithdrawalId),
    Batch(BatchId),
    Fallback(FallbackId),
    PortalRefund(PortalRefundId),
    InboxRefund(InboxRefundId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub hash: B256,
    pub number: u64,
}

impl Cursor {
    pub const ZERO: Self = Self {
        hash: B256::ZERO,
        number: 0,
    };
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum PortalState {
    AwaitingCreation(PortalIdentity),
    Created {
        identity: PortalIdentity,
        bounceback_gas: u64,
        deposit: Cursor,
        settlement: Settlement,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settlement {
    pub batch_index: u64,
    pub block_hash: B256,
    pub tempo_block: u64,
    pub submitted_deposit: Cursor,
    pub zone_height: U256,
    pub queue_head: U256,
    pub queue_tail: U256,
}

impl Settlement {
    pub const ZERO: Self = Self {
        batch_index: 0,
        block_hash: B256::ZERO,
        tempo_block: 0,
        submitted_deposit: Cursor::ZERO,
        zone_height: U256::ZERO,
        queue_head: U256::ZERO,
        queue_tail: U256::ZERO,
    };
}

impl PortalState {
    pub const fn identity(&self) -> PortalIdentity {
        match self {
            Self::AwaitingCreation(identity) | Self::Created { identity, .. } => *identity,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZoneState {
    pub processed_deposit: Cursor,
    pub next_withdrawal_index: u64,
    pub withdrawal_queue_hash: B256,
    pub withdrawal_batch_index: u64,
    pub tempo_gas_rate: u128,
    pub max_withdrawals_per_block: u32,
    pub last_fallback_nonce: u64,
    pub batch_start: BatchBoundaryStart,
}

impl Default for ZoneState {
    fn default() -> Self {
        Self {
            processed_deposit: Cursor::ZERO,
            next_withdrawal_index: 0,
            withdrawal_queue_hash: B256::ZERO,
            withdrawal_batch_index: 0,
            tempo_gas_rate: 0,
            max_withdrawals_per_block: 0,
            last_fallback_nonce: 0,
            batch_start: BatchBoundaryStart::ZERO,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchBoundaryStart {
    pub parent_hash: B256,
    pub deposit: Cursor,
    pub withdrawal_index: u64,
}
impl BatchBoundaryStart {
    pub const ZERO: Self = Self {
        parent_hash: B256::ZERO,
        deposit: Cursor::ZERO,
        withdrawal_index: 0,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenPhase {
    PendingZoneEnable,
    ZoneEnabled,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenAccounting {
    pub supply: U256,
    pub deposits: U256,
    pub withdrawals: U256,
}

impl TokenAccounting {
    pub fn collateral(self) -> Option<U256> {
        self.supply
            .checked_add(self.deposits)?
            .checked_add(self.withdrawals)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenState {
    pub phase: TokenPhase,
    pub accounting: TokenAccounting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DepositOwner {
    Ordinary(OrdinaryDeposit),
    BounceBack {
        withdrawal: WithdrawalId,
        token: Address,
        fallback_nonce: NonZeroU64,
        amount: u128,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WithdrawalOwner {
    PendingFailedDeposit {
        deposit: DepositId,
        token: Address,
        recipient: Address,
        amount: u128,
    },
    PendingUser {
        data: Withdrawal,
        fallback: FallbackId,
    },
    Finalized {
        data: Withdrawal,
        origin: WithdrawalOrigin,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Withdrawal {
    pub token: Address,
    pub sender_tag: B256,
    pub to: Address,
    pub amount: u128,
    pub memo: B256,
    pub gas_limit: u64,
    pub fallback_nonce: u64,
    pub callback_data: Bytes,
    pub encrypted_sender: Bytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WithdrawalOrigin {
    User { fallback: FallbackId },
    FailedDeposit { deposit: DepositId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchBoundary {
    pub first_parent: B256,
    pub final_block: B256,
    pub first_deposit: Cursor,
    pub final_deposit: Cursor,
    pub tempo_block: u64,
    pub zone_height: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BatchState {
    Finalized {
        boundary: BatchBoundary,
        first_withdrawal: u64,
        count: u64,
        queue_hash: B256,
    },
    Submitted {
        boundary: BatchBoundary,
        first_withdrawal: u64,
        count: u64,
        queue_hash: B256,
        next_ordinal: u64,
        logical_queue_index: U256,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FallbackState {
    Held {
        withdrawal: WithdrawalId,
        token: Address,
        amount: u128,
    },
    Queued {
        withdrawal: WithdrawalId,
        token: Address,
        amount: u128,
        deposit: DepositId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundCredit {
    pub amount: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StateValue {
    Portal(PortalState),
    Zone(ZoneState),
    Token(TokenState),
    Deposit(DepositOwner),
    Withdrawal(WithdrawalOwner),
    Batch(BatchState),
    Fallback(FallbackState),
    PortalRefund(RefundCredit),
    InboxRefund(RefundCredit),
}

impl StateValue {
    pub fn matches_key(&self, key: &StateKey) -> bool {
        matches!(
            (key, self),
            (StateKey::Portal, Self::Portal(_))
                | (StateKey::Zone, Self::Zone(_))
                | (StateKey::Token(_), Self::Token(_))
                | (StateKey::Deposit(_), Self::Deposit(_))
                | (StateKey::Withdrawal(_), Self::Withdrawal(_))
                | (StateKey::Batch(_), Self::Batch(_))
                | (StateKey::Fallback(_), Self::Fallback(_))
                | (StateKey::PortalRefund(_), Self::PortalRefund(_))
                | (StateKey::InboxRefund(_), Self::InboxRefund(_))
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    rows: BTreeMap<StateKey, StateValue>,
}

impl State {
    pub fn awaiting(identity: PortalIdentity) -> Self {
        Self {
            rows: BTreeMap::from([
                (
                    StateKey::Portal,
                    StateValue::Portal(PortalState::AwaitingCreation(identity)),
                ),
                (StateKey::Zone, StateValue::Zone(ZoneState::default())),
            ]),
        }
    }

    pub fn from_rows(rows: BTreeMap<StateKey, StateValue>) -> Result<Self, StateFamilyError> {
        for (key, value) in &rows {
            if !value.matches_key(key) {
                return Err(StateFamilyError { key: *key });
            }
        }
        Ok(Self { rows })
    }

    pub fn rows(&self) -> &BTreeMap<StateKey, StateValue> {
        &self.rows
    }

    pub fn apply(&mut self, delta: &StateDelta) -> Result<(), StateFamilyError> {
        delta.validate()?;
        for (key, value) in &delta.writes {
            match value {
                Some(value) => {
                    self.rows.insert(*key, value.clone());
                }
                None => {
                    self.rows.remove(key);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("state value does not match key family {key:?}")]
pub struct StateFamilyError {
    pub key: StateKey,
}

pub(crate) struct Overlay<'a> {
    parent: &'a State,
    writes: BTreeMap<StateKey, Option<StateValue>>,
}

impl<'a> Overlay<'a> {
    pub(crate) fn new(parent: &'a State) -> Self {
        Self {
            parent,
            writes: BTreeMap::new(),
        }
    }

    pub(crate) fn get(&self, key: &StateKey) -> Option<&StateValue> {
        self.writes
            .get(key)
            .map_or_else(|| self.parent.rows.get(key), Option::as_ref)
    }

    pub(crate) fn set(&mut self, key: StateKey, value: Option<StateValue>) {
        debug_assert!(value.as_ref().is_none_or(|value| value.matches_key(&key)));
        self.writes.insert(key, value);
    }

    pub(crate) fn range(
        &self,
        start: Bound<StateKey>,
        end: Bound<StateKey>,
    ) -> impl Iterator<Item = (StateKey, &StateValue)> {
        let mut merged = self
            .parent
            .rows
            .range((start, end))
            .map(|(key, value)| (*key, value))
            .collect::<BTreeMap<_, _>>();
        for (key, value) in self.writes.range((start, end)) {
            match value {
                Some(value) => {
                    merged.insert(*key, value);
                }
                None => {
                    merged.remove(key);
                }
            }
        }
        merged.into_iter()
    }

    pub(crate) fn finish(self) -> StateDelta {
        StateDelta {
            writes: self.writes.into_iter().collect(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateDelta {
    writes: Vec<(StateKey, Option<StateValue>)>,
}

impl StateDelta {
    pub(crate) fn from_sorted_writes(writes: Vec<(StateKey, Option<StateValue>)>) -> Self {
        Self { writes }
    }

    pub fn writes(&self) -> &[(StateKey, Option<StateValue>)] {
        &self.writes
    }

    pub fn validate(&self) -> Result<(), StateFamilyError> {
        let mut previous = None;
        for (key, value) in &self.writes {
            if previous.is_some_and(|previous| previous >= *key)
                || value.as_ref().is_some_and(|value| !value.matches_key(key))
            {
                return Err(StateFamilyError { key: *key });
            }
            previous = Some(*key);
        }
        Ok(())
    }
}
