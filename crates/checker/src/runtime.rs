//! Deterministic checkpoint builder and bounded runtime primitives.
//!
//! Observation adapters are the only intended producers of
//! [`AuthenticatedBlock`].

use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use alloy_primitives::{Address, B256, Bytes, U256};
use zone_checker_kernel::{
    BatchId, DepositId, ExpectedEffect, ExpectedState, Finding as CompactFinding, FindingData,
    FindingLocation, ImportedFacts, PortalIdentity, State, ViolationCategory, WithdrawalId,
    ZoneFacts, apply_imported, apply_zone, validate,
};

use crate::persistence::{
    BlockNumHash, ChainCut, CoverageGapReason, Finding, FindingKey, Identity, JournalEntry,
    Persistence, PersistenceError, Snapshot,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeState {
    Starting,
    Healthy,
    Retrying,
    Alerting,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureClass {
    ImmediateTerminal,
    BoundedRetry,
    TransientRetry,
    AuthenticatedDivergence,
}

#[derive(Debug, Clone)]
pub(crate) struct Failure {
    pub class: FailureClass,
    pub gap_reason: CoverageGapReason,
    pub message: String,
    pub finding: Option<Box<CompactFinding>>,
}

fn typed_failure(
    category: ViolationCategory,
    code: u16,
    location: Option<FindingLocation>,
    expected: Option<FindingData>,
    actual: Option<FindingData>,
    message: impl Into<String>,
) -> Failure {
    Failure {
        class: FailureClass::AuthenticatedDivergence,
        gap_reason: CoverageGapReason::NotCheckedAncestorDivergence,
        message: message.into(),
        finding: Some(Box::new(CompactFinding {
            category,
            code,
            location,
            expected,
            actual,
        })),
    }
}

/// Facts may be constructed only inside the checker crate after the observe
/// module has authenticated envelopes, receipts, roots and exact state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthenticatedOutputs {
    /// Constructed from authenticated receipts/state reads, never from the
    /// kernel candidate.
    pub effects: Vec<ObservedEffect>,
    pub state: ExpectedState,
    /// Exact token supplies read at this block, keyed by token address.
    pub supplies: std::collections::BTreeMap<alloy_primitives::Address, alloy_primitives::U256>,
    /// Exact Portal balance reads, keyed by token. These are collateral
    /// carriers, not model requirements; surplus is valid.
    pub collateral: std::collections::BTreeMap<Address, U256>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ObservedEffect {
    TokenEnabled {
        token: Address,
        name: String,
        symbol: String,
        currency: String,
    },
    DepositAppended {
        id: DepositId,
        queue_hash: B256,
    },
    DepositProcessed {
        deposit_hash: B256,
        sender: Address,
        token: Address,
        amount: u128,
    },
    DepositFailed {
        deposit_hash: B256,
        sender: Address,
        token: Address,
        amount: u128,
    },
    WithdrawalRequested {
        id: WithdrawalId,
        sender: Address,
        token: Address,
        to: Address,
        amount: u128,
        fee: u128,
        memo: B256,
        gas_limit: u64,
        fallback_nonce: u64,
        callback_data: Bytes,
        reveal_to: Bytes,
    },
    BatchFinalized {
        id: BatchId,
        queue_hash: B256,
    },
    BatchSubmitted {
        id: BatchId,
        queue_index: U256,
        processed_deposit_hash: B256,
        final_block_hash: B256,
        queue_hash: B256,
        processed_deposit_number: u64,
    },
    UserWithdrawalProcessed {
        to: Address,
        sender_tag: B256,
        token: Address,
        amount: u128,
        callback_success: bool,
    },
    FailedDepositRefunded {
        recipient: Address,
        token: Address,
        amount: u128,
        fee: u128,
        pending: bool,
    },
    BounceBackAppended {
        fallback_nonce: u64,
        token: Address,
        amount: u128,
        id: DepositId,
        queue_hash: B256,
    },
    BounceBackMinted {
        token: Address,
        amount: u128,
    },
    BounceBackPending {
        token: Address,
        amount: u128,
    },
    RefundClaimed {
        token: Address,
        recipient: Address,
        amount: u128,
    },
}

pub(crate) fn project_expected_effect(e: &ExpectedEffect) -> ObservedEffect {
    match e {
        ExpectedEffect::TokenEnabled {
            token,
            name,
            symbol,
            currency,
        } => ObservedEffect::TokenEnabled {
            token: *token,
            name: name.clone(),
            symbol: symbol.clone(),
            currency: currency.clone(),
        },
        ExpectedEffect::DepositAppended { id, queue_hash } => ObservedEffect::DepositAppended {
            id: *id,
            queue_hash: *queue_hash,
        },
        ExpectedEffect::DepositProcessed {
            deposit_hash,
            sender,
            token,
            amount,
        } => ObservedEffect::DepositProcessed {
            deposit_hash: *deposit_hash,
            sender: *sender,
            token: *token,
            amount: *amount,
        },
        ExpectedEffect::DepositFailed {
            deposit_hash,
            sender,
            token,
            amount,
        } => ObservedEffect::DepositFailed {
            deposit_hash: *deposit_hash,
            sender: *sender,
            token: *token,
            amount: *amount,
        },
        ExpectedEffect::WithdrawalRequested {
            id,
            sender,
            token,
            to,
            amount,
            fee,
            memo,
            gas_limit,
            fallback_nonce,
            callback_data,
            reveal_to,
        } => ObservedEffect::WithdrawalRequested {
            id: *id,
            sender: *sender,
            token: *token,
            to: *to,
            amount: *amount,
            fee: *fee,
            memo: *memo,
            gas_limit: *gas_limit,
            fallback_nonce: *fallback_nonce,
            callback_data: callback_data.clone(),
            reveal_to: reveal_to.clone(),
        },
        ExpectedEffect::BatchFinalized { id, queue_hash } => ObservedEffect::BatchFinalized {
            id: *id,
            queue_hash: *queue_hash,
        },
        ExpectedEffect::BatchSubmitted {
            id,
            queue_index,
            processed_deposit_hash,
            final_block_hash,
            queue_hash,
            processed_deposit_number,
        } => ObservedEffect::BatchSubmitted {
            id: *id,
            queue_index: *queue_index,
            processed_deposit_hash: *processed_deposit_hash,
            final_block_hash: *final_block_hash,
            queue_hash: *queue_hash,
            processed_deposit_number: *processed_deposit_number,
        },
        ExpectedEffect::UserWithdrawalProcessed {
            to,
            sender_tag,
            token,
            amount,
            callback_success,
            ..
        } => ObservedEffect::UserWithdrawalProcessed {
            to: *to,
            sender_tag: *sender_tag,
            token: *token,
            amount: *amount,
            callback_success: *callback_success,
        },
        ExpectedEffect::FailedDepositRefunded {
            recipient,
            token,
            amount,
            fee,
            pending,
            ..
        } => ObservedEffect::FailedDepositRefunded {
            recipient: *recipient,
            token: *token,
            amount: *amount,
            fee: *fee,
            pending: *pending,
        },
        ExpectedEffect::BounceBackAppended {
            fallback_nonce,
            token,
            amount,
            id,
            queue_hash,
        } => ObservedEffect::BounceBackAppended {
            fallback_nonce: *fallback_nonce,
            token: *token,
            amount: *amount,
            id: *id,
            queue_hash: *queue_hash,
        },
        ExpectedEffect::BounceBackMinted { token, amount } => ObservedEffect::BounceBackMinted {
            token: *token,
            amount: *amount,
        },
        ExpectedEffect::BounceBackPending { token, amount } => ObservedEffect::BounceBackPending {
            token: *token,
            amount: *amount,
        },
        ExpectedEffect::RefundClaimed {
            token,
            recipient,
            amount,
        } => ObservedEffect::RefundClaimed {
            token: *token,
            recipient: *recipient,
            amount: *amount,
        },
    }
}

#[cfg(test)]
impl From<&ExpectedEffect> for ObservedEffect {
    fn from(effect: &ExpectedEffect) -> Self {
        project_expected_effect(effect)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AuthenticatedBlock {
    pub zone: BlockNumHash,
    pub parent: BlockNumHash,
    pub tempo: BlockNumHash,
    pub tempo_parent: BlockNumHash,
    pub imported: ImportedFacts,
    pub zone_facts: ZoneFacts,
    pub outputs: AuthenticatedOutputs,
}

/// Shallow, validated notification geometry.  This is deliberately richer
/// than a vector of coordinates: confusing the old and new fragments (or the
/// reorg ancestor and replacement tip) can acknowledge unchecked history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotificationPlan {
    pub reverted: Vec<BlockNumHash>,
    pub ancestor: BlockNumHash,
    pub applied: Vec<BlockNumHash>,
    pub acknowledge: BlockNumHash,
}

impl NotificationPlan {
    pub(crate) fn validate(self) -> Result<Self, Failure> {
        let contiguous = |values: &[BlockNumHash]| {
            values
                .windows(2)
                .all(|p| p[0].number.checked_add(1) == Some(p[1].number))
        };
        if (!self.reverted.is_empty() && !contiguous(&self.reverted))
            || (!self.applied.is_empty() && !contiguous(&self.applied))
            || self
                .reverted
                .first()
                .is_some_and(|b| self.ancestor.number.checked_add(1) != Some(b.number))
            || self
                .applied
                .first()
                .is_some_and(|b| self.ancestor.number.checked_add(1) != Some(b.number))
            || (self.applied.is_empty() && self.acknowledge != self.ancestor)
            || self.applied.last().is_some_and(|b| *b != self.acknowledge)
            || (self.reverted.is_empty() && self.applied.is_empty())
        {
            return Err(Failure {
                class: FailureClass::ImmediateTerminal,
                gap_reason: CoverageGapReason::Other(2),
                message: "invalid notification geometry".into(),
                finding: None,
            });
        }
        Ok(self)
    }
}

/// Must inspect only notification-owned headers; no provider access is
/// permitted before this plan has been validated and descendants of an active
/// finding have been handled.
pub(crate) trait PlannedNotification {
    fn plan(&self) -> Result<NotificationPlan, Failure>;
}

/// The single observation+projection boundary shared by builder and runtime.
pub(crate) trait ObservationPipeline<N> {
    /// Authenticate exactly one block. `parent_state` is the durable state at
    /// the block's parent; implementations must not speculate across blocks.
    fn authenticate_at(
        &mut self,
        notification: &N,
        index: usize,
        parent_state: &State,
    ) -> Result<AuthenticatedBlock, Failure>;
    fn compare(
        &mut self,
        block: &AuthenticatedBlock,
        expected: &zone_checker_kernel::Candidate,
    ) -> Result<(), Failure>;
}

pub(crate) fn compare_authenticated(
    block: &AuthenticatedBlock,
    candidate: &zone_checker_kernel::Candidate,
) -> Result<(), Failure> {
    let effects: Vec<_> = candidate
        .expected_effects
        .iter()
        .map(project_expected_effect)
        .collect();
    let supplies = candidate
        .expected_accounting
        .iter()
        .map(|(token, a)| (*token, a.supply))
        .collect();
    let collateral_ok = candidate
        .expected_accounting
        .iter()
        .all(|(token, accounting)| {
            accounting.collateral().is_some_and(|required| {
                block
                    .outputs
                    .collateral
                    .get(token)
                    .is_some_and(|actual| *actual >= required)
            })
        });
    let observed = &block.outputs.state;
    let expected = &candidate.expected_state;
    let commitments_match = observed.tempo_block_hash == expected.tempo_block_hash
        && observed.tempo_block_number == expected.tempo_block_number
        && observed.processed_deposit_hash == expected.processed_deposit_hash
        && observed.processed_deposit_number == expected.processed_deposit_number
        && observed.withdrawal_queue_hash == expected.withdrawal_queue_hash
        && observed.withdrawal_batch_index == expected.withdrawal_batch_index;
    let mismatch = if block.outputs.effects != effects {
        Some((ViolationCategory::EffectMismatch, 1))
    } else if !commitments_match {
        Some((ViolationCategory::StateMismatch, 2))
    } else if block.outputs.supplies != supplies {
        Some((ViolationCategory::SupplyMismatch, 3))
    } else if !collateral_ok {
        Some((ViolationCategory::CollateralMismatch, 4))
    } else {
        None
    };
    if let Some((category, code)) = mismatch {
        return Err(typed_failure(
            category,
            code,
            Some(FindingLocation::Block),
            Some(FindingData::Bool(true)),
            Some(FindingData::Bool(false)),
            "authenticated output differs from checker candidate",
        ));
    }
    Ok(())
}

fn validate_creation_coordinate(
    identity: Identity,
    state: &State,
    block: &AuthenticatedBlock,
) -> Result<(), Failure> {
    use zone_checker_kernel::{ImportedOperation, PortalState, StateValue};

    // Height zero is retained as the pre-creation-checkpoint sentinel.
    if identity.creation_height == 0 {
        return Ok(());
    }
    let awaiting = state
        .rows()
        .values()
        .any(|value| matches!(value, StateValue::Portal(PortalState::AwaitingCreation(_))));
    let creates = block
        .imported
        .operations
        .iter()
        .filter(|operation| matches!(operation, ImportedOperation::Create { .. }))
        .count();
    let valid = if awaiting {
        match block.tempo.number.cmp(&identity.creation_height) {
            std::cmp::Ordering::Less => creates == 0,
            std::cmp::Ordering::Equal => {
                block.tempo.hash == identity.creation_block && creates == 1
            }
            std::cmp::Ordering::Greater => false,
        }
    } else {
        creates == 0
    };
    if valid {
        Ok(())
    } else {
        Err(typed_failure(
            ViolationCategory::CreationAnchor,
            1,
            Some(FindingLocation::Block),
            Some(FindingData::Hash(identity.creation_block)),
            Some(FindingData::Hash(block.tempo.hash)),
            "Portal creation height/hash/grammar diverges from configured anchor",
        ))
    }
}

pub(crate) struct RetryBudget {
    max_attempts: u32,
    max_elapsed: Duration,
}

impl RetryBudget {
    pub(crate) const fn new(max_attempts: u32, max_elapsed: Duration) -> Self {
        Self {
            max_attempts,
            max_elapsed,
        }
    }
    fn exhausted(&self, attempts: u32, started: Instant, now: Instant) -> bool {
        attempts >= self.max_attempts || now.saturating_duration_since(started) >= self.max_elapsed
    }
}

/// Exactly one current notification and one bounded FIFO.
pub(crate) struct Runtime<N> {
    state: RuntimeState,
    current: Option<N>,
    fifo: VecDeque<N>,
    capacity: usize,
    budget: RetryBudget,
    retry: Option<RetryState>,
    current_unwound: bool,
}

struct RetryState {
    attempts: u32,
    started: Instant,
    next_attempt: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeAction {
    None,
    AwaitNotification,
    RetryAt(Instant),
    Acknowledge(BlockNumHash),
    AcknowledgeAndTerminate(BlockNumHash),
    Terminal,
}

impl<N> Runtime<N> {
    pub(crate) fn new(capacity: usize, budget: RetryBudget) -> Self {
        Self {
            state: RuntimeState::Starting,
            current: None,
            fifo: VecDeque::with_capacity(capacity),
            capacity,
            budget,
            retry: None,
            current_unwound: false,
        }
    }
    #[cfg(test)]
    pub(crate) fn state(&self) -> RuntimeState {
        self.state
    }
    pub(crate) fn current(&self) -> Option<&N> {
        self.current.as_ref()
    }

    pub(crate) fn next_applied_index(
        &mut self,
        store: &Persistence,
        identity: Identity,
    ) -> Result<Option<usize>, PersistenceError>
    where
        N: PlannedNotification,
    {
        if self.state == RuntimeState::Disabled {
            return Ok(None);
        }
        let Some(notification) = self.current.as_ref() else {
            return Ok(None);
        };
        let plan = notification
            .plan()
            .and_then(NotificationPlan::validate)
            .map_err(|failure| PersistenceError::Invalid(failure.message))?;
        let before = store.load(identity)?;
        if let Some(active) = before.meta.active_finding
            && !plan.reverted.contains(&active.zone)
        {
            // Poll classifies retained-finding catch-up and descendants before
            // any observation provider is touched.
            return Ok(None);
        }
        if !plan.reverted.is_empty() && !self.current_unwound {
            self.reorg(store, identity, plan.ancestor)?;
            self.current_unwound = true;
        }
        if plan.applied.is_empty() {
            return Ok(None);
        }
        let snapshot = store.load(identity)?;
        if snapshot.meta.active_finding.is_some()
            && snapshot.meta.acknowledged_zone_tip.number.checked_add(1)
                == Some(plan.applied[0].number)
        {
            return Ok(None);
        }
        let tip = snapshot.meta.verified_zone_tip;
        if tip == plan.ancestor {
            Ok(Some(0))
        } else {
            Ok(plan
                .applied
                .iter()
                .position(|coordinate| *coordinate == tip)
                .map(|i| i + 1))
        }
    }
    pub(crate) fn push(&mut self, notification: N) -> Result<(), N> {
        if self.state == RuntimeState::Disabled {
            return Err(notification);
        }
        if self.current.is_none() {
            self.current = Some(notification);
            Ok(())
        } else if self.fifo.len() < self.capacity {
            self.fifo.push_back(notification);
            Ok(())
        } else {
            Err(notification)
        }
    }

    /// Fail open on FIFO overflow, but only after durably naming every item
    /// that will no longer be checked.
    pub(crate) fn push_or_record_overflow(
        &mut self,
        store: &Persistence,
        identity: Identity,
        notification: N,
    ) -> Result<RuntimeAction, PersistenceError>
    where
        N: PlannedNotification,
    {
        if self.state == RuntimeState::Disabled {
            return Ok(RuntimeAction::Terminal);
        }
        if self.current.is_none() || self.fifo.len() < self.capacity {
            let accepted = self.push(notification);
            debug_assert!(accepted.is_ok());
            return Ok(RuntimeAction::None);
        }
        let plans = self
            .current
            .iter()
            .chain(self.fifo.iter())
            .map(PlannedNotification::plan)
            .collect::<Result<Vec<_>, _>>();
        let Ok(mut plans) = plans else {
            self.state = RuntimeState::Disabled;
            return Ok(RuntimeAction::Terminal);
        };
        let Ok(rejected) = notification.plan() else {
            self.state = RuntimeState::Disabled;
            return Ok(RuntimeAction::Terminal);
        };
        plans.push(rejected);
        let snapshot = store.load(identity)?;
        let first_reaches_tip = plans.first().is_some_and(|plan| {
            plan.ancestor == snapshot.meta.verified_zone_tip
                || plan.applied.contains(&snapshot.meta.verified_zone_tip)
        });
        let valid = plans.iter().all(|plan| {
            plan.reverted.is_empty() && !plan.applied.is_empty() && plan.clone().validate().is_ok()
        }) && first_reaches_tip
            && plans
                .windows(2)
                .all(|pair| pair[1].ancestor == pair[0].acknowledge);
        if !valid {
            self.state = RuntimeState::Disabled;
            return Ok(RuntimeAction::Terminal);
        }
        let first_plan = &plans[0];
        let first = if snapshot.meta.verified_zone_tip == first_plan.ancestor {
            first_plan.applied[0]
        } else if let Some(index) = first_plan
            .applied
            .iter()
            .position(|coordinate| *coordinate == snapshot.meta.verified_zone_tip)
        {
            *first_plan.applied.get(index + 1).ok_or_else(|| {
                PersistenceError::Invalid("overflow has no unchecked suffix".into())
            })?
        } else {
            self.state = RuntimeState::Disabled;
            return Ok(RuntimeAction::Terminal);
        };
        let last = plans.last().expect("nonempty plan set").acknowledge;
        let reason = match &snapshot.meta.coverage {
            crate::persistence::Coverage::Gap { reason, .. } => reason.clone(),
            crate::persistence::Coverage::Complete => CoverageGapReason::ProviderUnavailable,
        };
        store.record_gap(identity, first, last, reason)?;
        self.state = RuntimeState::Disabled;
        self.current = None;
        self.fifo.clear();
        Ok(RuntimeAction::AcknowledgeAndTerminate(last))
    }

    /// Persist a canonical suffix reconstructed locally after stream failure.
    pub(crate) fn record_stream_failure(
        &mut self,
        store: &Persistence,
        identity: Identity,
        canonical_suffix: &[BlockNumHash],
    ) -> Result<RuntimeAction, PersistenceError> {
        if canonical_suffix.is_empty()
            || canonical_suffix
                .windows(2)
                .any(|pair| pair[0].number.checked_add(1) != Some(pair[1].number))
        {
            self.state = RuntimeState::Disabled;
            return Ok(RuntimeAction::Terminal);
        }
        let snapshot = store.load(identity)?;
        let (first, reason) = match &snapshot.meta.coverage {
            crate::persistence::Coverage::Complete => {
                (canonical_suffix[0], CoverageGapReason::ProviderUnavailable)
            }
            crate::persistence::Coverage::Gap {
                first_unchecked,
                reason,
                ..
            } => (*first_unchecked, reason.clone()),
        };
        let last = *canonical_suffix.last().expect("nonempty");
        if snapshot.meta.verified_zone_tip.number.checked_add(1) != Some(canonical_suffix[0].number)
            || canonical_suffix[0] != first
        {
            self.state = RuntimeState::Disabled;
            return Ok(RuntimeAction::Terminal);
        }
        store.record_gap(identity, first, last, reason)?;
        self.state = RuntimeState::Disabled;
        Ok(RuntimeAction::AcknowledgeAndTerminate(last))
    }

    pub(crate) fn reorg(
        &mut self,
        store: &Persistence,
        identity: Identity,
        ancestor: BlockNumHash,
    ) -> Result<(), PersistenceError> {
        let snapshot = store.reorg(identity, ancestor)?;
        self.retry = None;
        self.state = if snapshot.meta.active_finding.is_some() {
            RuntimeState::Alerting
        } else {
            RuntimeState::Starting
        };
        Ok(())
    }
    fn advance(&mut self) {
        self.current = self.fifo.pop_front();
        self.retry = None;
        self.current_unwound = false;
    }

    /// Process the current item. Successful blocks are committed separately;
    /// therefore a failure records precisely the remaining multi-block suffix.
    pub(crate) fn poll<P: ObservationPipeline<N>>(
        &mut self,
        store: &Persistence,
        identity: Identity,
        pipeline: &mut P,
        now: Instant,
    ) -> Result<RuntimeAction, PersistenceError>
    where
        N: PlannedNotification,
    {
        if self.state == RuntimeState::Disabled {
            return Ok(RuntimeAction::Terminal);
        }
        let Some(notification) = self.current.as_ref() else {
            return Ok(RuntimeAction::None);
        };
        let plan = match notification.plan().and_then(NotificationPlan::validate) {
            Ok(value) => value,
            _ => {
                self.state = RuntimeState::Disabled;
                return Ok(RuntimeAction::Terminal);
            }
        };
        let before = store.load(identity)?;
        if let Some(active) = before.meta.active_finding
            && !plan.reverted.contains(&active.zone)
        {
            let same_height_conflict = std::iter::once(plan.ancestor)
                .chain(plan.reverted.iter().copied())
                .chain(plan.applied.iter().copied())
                .any(|coordinate| {
                    coordinate.number == active.zone.number && coordinate.hash != active.zone.hash
                });
            let exact_commit_descendant = plan.reverted.is_empty()
                && (plan.ancestor == before.meta.acknowledged_zone_tip
                    || plan.ancestor == active.zone);
            let exact_reorg_descendant = !plan.reverted.is_empty()
                && (plan.ancestor == active.zone
                    || plan.reverted.last().copied() == Some(before.meta.acknowledged_zone_tip));
            let preserves = !same_height_conflict
                && (plan.applied.contains(&active.zone)
                    || exact_commit_descendant
                    || exact_reorg_descendant);
            if !preserves {
                self.state = RuntimeState::Disabled;
                return Ok(RuntimeAction::Terminal);
            }
            if !plan.reverted.is_empty() && !self.current_unwound {
                self.reorg(store, identity, plan.ancestor)?;
                self.current_unwound = true;
            }
            let snapshot = store.load(identity)?;
            let first = match snapshot.meta.coverage {
                crate::persistence::Coverage::Gap {
                    first_unchecked, ..
                } => first_unchecked,
                crate::persistence::Coverage::Complete => active.zone,
            };
            store.record_gap(
                identity,
                first,
                plan.acknowledge,
                CoverageGapReason::NotCheckedAncestorDivergence,
            )?;
            self.state = RuntimeState::Alerting;
            self.advance();
            return Ok(RuntimeAction::Acknowledge(plan.acknowledge));
        }
        // Canonical unwind is always durable before inspecting replacement
        // data. Repeating this after a crash is harmless.
        if !plan.reverted.is_empty() && !self.current_unwound {
            self.reorg(store, identity, plan.ancestor)?;
            self.current_unwound = true;
        }
        if plan.applied.is_empty() {
            self.advance();
            return Ok(RuntimeAction::Acknowledge(plan.acknowledge));
        }
        let coordinates = plan.applied;
        let snapshot = store.load(identity)?;
        // A finding may itself be in the unverified gap, so the durable tip
        // need not equal this notification's immediate ancestor. Descendant
        // notifications extend that same gap without touching providers.
        if snapshot.meta.active_finding.is_some()
            && snapshot.meta.acknowledged_zone_tip.number.checked_add(1)
                == Some(coordinates[0].number)
        {
            let gap_first = match &snapshot.meta.coverage {
                crate::persistence::Coverage::Gap {
                    first_unchecked, ..
                } => *first_unchecked,
                crate::persistence::Coverage::Complete => coordinates[0],
            };
            store.record_gap(
                identity,
                gap_first,
                plan.acknowledge,
                CoverageGapReason::NotCheckedAncestorDivergence,
            )?;
            self.state = RuntimeState::Alerting;
            self.advance();
            return Ok(RuntimeAction::Acknowledge(plan.acknowledge));
        }
        let first = if snapshot.meta.verified_zone_tip == plan.ancestor {
            0
        } else if let Some(index) = coordinates
            .iter()
            .position(|coordinate| *coordinate == snapshot.meta.verified_zone_tip)
        {
            index + 1
        } else {
            self.state = RuntimeState::Disabled;
            return Ok(RuntimeAction::Terminal);
        };
        if first == coordinates.len() {
            self.advance();
            return Ok(RuntimeAction::Acknowledge(plan.acknowledge));
        }
        let gap_next = match &snapshot.meta.coverage {
            crate::persistence::Coverage::Gap {
                acknowledged_through,
                ..
            } if coordinates[first] != *acknowledged_through => {
                let next = coordinates.get(first + 1).copied().or_else(|| {
                    self.fifo.front().and_then(|next| {
                        next.plan()
                            .ok()
                            .filter(|next_plan| next_plan.ancestor == plan.acknowledge)
                            .and_then(|next_plan| next_plan.applied.first().copied())
                    })
                });
                let Some(next) = next else {
                    return Ok(RuntimeAction::AwaitNotification);
                };
                Some(next)
            }
            _ => None,
        };
        if self
            .retry
            .as_ref()
            .is_some_and(|retry| now < retry.next_attempt)
        {
            return Ok(RuntimeAction::RetryAt(
                self.retry.as_ref().unwrap().next_attempt,
            ));
        }
        let block = match pipeline.authenticate_at(
            self.current
                .as_ref()
                .expect("current notification retained"),
            first,
            &snapshot.state,
        ) {
            Ok(block) => block,
            Err(f)
                if matches!(
                    f.class,
                    FailureClass::BoundedRetry | FailureClass::TransientRetry
                ) =>
            {
                let retry = self.retry.get_or_insert(RetryState {
                    attempts: 0,
                    started: now,
                    next_attempt: now,
                });
                retry.attempts += 1;
                self.state = RuntimeState::Retrying;
                if self.budget.exhausted(retry.attempts, retry.started, now) {
                    let first = coordinates[first];
                    let mut last = *coordinates.last().unwrap();
                    let mut previous = plan.acknowledge;
                    for queued in &self.fifo {
                        let queued = queued.plan().and_then(NotificationPlan::validate);
                        let Ok(queued) = queued else {
                            self.state = RuntimeState::Disabled;
                            return Ok(RuntimeAction::Terminal);
                        };
                        if !queued.reverted.is_empty()
                            || queued.applied.is_empty()
                            || queued.ancestor != previous
                        {
                            self.state = RuntimeState::Disabled;
                            return Ok(RuntimeAction::Terminal);
                        }
                        last = queued.acknowledge;
                        previous = queued.acknowledge;
                    }
                    let reason = match &snapshot.meta.coverage {
                        crate::persistence::Coverage::Gap { reason, .. } => reason.clone(),
                        crate::persistence::Coverage::Complete => f.gap_reason,
                    };
                    store.record_gap(identity, first, last, reason)?;
                    self.state = RuntimeState::Disabled;
                    self.current = None;
                    self.fifo.clear();
                    return Ok(RuntimeAction::AcknowledgeAndTerminate(last));
                }
                retry.next_attempt = now + Duration::from_millis(25);
                return Ok(RuntimeAction::RetryAt(retry.next_attempt));
            }
            Err(failure) if failure.class == FailureClass::AuthenticatedDivergence => {
                let zone = coordinates[first];
                let last = *coordinates.last().unwrap();
                let snapshot = store.load(identity)?;
                let typed = failure.finding.ok_or_else(|| {
                    PersistenceError::Invalid(
                        "authenticated divergence missing typed finding".into(),
                    )
                })?;
                let (category, code, location, operation, expected, actual) = finding_parts(&typed);
                let (evidence_len, digest) = evidence_identity(&expected, &actual)?;
                let key = FindingKey {
                    zone,
                    operation,
                    code,
                };
                store.record_finding(
                    identity,
                    key,
                    Finding {
                        zone,
                        parent: snapshot.meta.verified_zone_tip,
                        imported_tempo: None,
                        category,
                        code,
                        location,
                        operation,
                        expected,
                        actual,
                        evidence_len,
                        evidence_digest: digest,
                        summary: failure.message,
                    },
                )?;
                store.record_gap(
                    identity,
                    zone,
                    last,
                    CoverageGapReason::NotCheckedAncestorDivergence,
                )?;
                self.state = RuntimeState::Alerting;
                self.advance();
                return Ok(RuntimeAction::Acknowledge(last));
            }
            Err(_) => {
                self.state = RuntimeState::Disabled;
                return Ok(RuntimeAction::Terminal);
            }
        };
        self.retry = None;
        if block.zone != coordinates[first] {
            store.record_gap(
                identity,
                coordinates[first],
                *coordinates.last().unwrap(),
                CoverageGapReason::MissingReceipts,
            )?;
            self.state = RuntimeState::Disabled;
            return Ok(RuntimeAction::AcknowledgeAndTerminate(
                *coordinates.last().unwrap(),
            ));
        }
        let suffix_end = *coordinates
            .last()
            .expect("validated nonempty applied fragment");
        for block in std::slice::from_ref(&block) {
            let snapshot = store.load(identity)?;
            if snapshot.meta.active_finding.is_some() {
                let first = match snapshot.meta.coverage {
                    crate::persistence::Coverage::Gap {
                        first_unchecked, ..
                    } => first_unchecked,
                    crate::persistence::Coverage::Complete => block.zone,
                };
                store.record_gap(
                    identity,
                    first,
                    suffix_end,
                    CoverageGapReason::NotCheckedAncestorDivergence,
                )?;
                self.state = RuntimeState::Alerting;
                break;
            }
            if let Err(failure) = validate_creation_coordinate(identity, &snapshot.state, block) {
                self.persist_divergence(store, identity, block, failure)?;
                store.record_gap(
                    identity,
                    block.zone,
                    suffix_end,
                    CoverageGapReason::NotCheckedAncestorDivergence,
                )?;
                self.state = RuntimeState::Alerting;
                break;
            }
            let candidate = match apply_imported(&snapshot.state, &block.imported)
                .and_then(|imported| apply_zone(imported, &block.zone_facts))
            {
                Ok(candidate) => candidate,
                Err(error) => {
                    self.persist_divergence(
                        store,
                        identity,
                        block,
                        typed_failure(
                            ViolationCategory::Invariant,
                            1,
                            Some(FindingLocation::Block),
                            None,
                            Some(FindingData::Code(1)),
                            error.to_string(),
                        ),
                    )?;
                    store.record_gap(
                        identity,
                        block.zone,
                        suffix_end,
                        CoverageGapReason::NotCheckedAncestorDivergence,
                    )?;
                    self.state = RuntimeState::Alerting;
                    break;
                }
            };
            if let Err(failure) = pipeline.compare(block, &candidate) {
                if failure.class == FailureClass::AuthenticatedDivergence {
                    self.persist_divergence(store, identity, block, failure)?;
                    store.record_gap(
                        identity,
                        block.zone,
                        suffix_end,
                        CoverageGapReason::NotCheckedAncestorDivergence,
                    )?;
                    self.state = RuntimeState::Alerting;
                } else {
                    let first = block.zone;
                    store.record_gap(identity, first, suffix_end, failure.gap_reason)?;
                    self.state = RuntimeState::Disabled;
                }
                break;
            }
            // A durable gap is recovered one block at a time.  Keep its
            // original acknowledgement and reason until the final missing
            // block closes it; never jump directly from a multi-block gap to
            // Complete.
            let coverage = match &snapshot.meta.coverage {
                crate::persistence::Coverage::Gap {
                    first_unchecked,
                    acknowledged_through,
                    reason,
                } if *first_unchecked == block.zone => {
                    if block.zone == *acknowledged_through {
                        crate::persistence::Coverage::Complete
                    } else {
                        let next = gap_next.ok_or_else(|| {
                            PersistenceError::Invalid("durable gap has no next coordinate".into())
                        })?;
                        crate::persistence::Coverage::Gap {
                            first_unchecked: next,
                            acknowledged_through: *acknowledged_through,
                            reason: reason.clone(),
                        }
                    }
                }
                crate::persistence::Coverage::Complete => crate::persistence::Coverage::Complete,
                _ => {
                    return Err(PersistenceError::Invalid(
                        "applied block does not begin at durable gap".into(),
                    ));
                }
            };
            let acknowledged = match &coverage {
                crate::persistence::Coverage::Complete => block.zone,
                crate::persistence::Coverage::Gap {
                    acknowledged_through,
                    ..
                } => *acknowledged_through,
            };
            store.apply(
                identity,
                JournalEntry {
                    zone: block.zone,
                    parent: block.parent,
                    imported_tempo: block.tempo,
                    imported_tempo_parent: block.tempo_parent,
                    delta: candidate.delta,
                },
                acknowledged,
                coverage,
            )?;
            if first + 1 == coordinates.len() {
                self.state = RuntimeState::Healthy;
            }
        }
        // Persistence above is the commit point; only now may the caller ack.
        let ready = store.load(identity)?.meta.acknowledged_zone_tip;
        if first + 1 == coordinates.len()
            || matches!(self.state, RuntimeState::Alerting | RuntimeState::Disabled)
        {
            self.advance();
            if self.state == RuntimeState::Disabled {
                Ok(RuntimeAction::AcknowledgeAndTerminate(ready))
            } else {
                Ok(RuntimeAction::Acknowledge(ready))
            }
        } else {
            // Retain the notification. The next poll derives its index from
            // the newly durable tip, so restart and retry cannot reacquire or
            // unwind this committed prefix.
            Ok(RuntimeAction::None)
        }
    }

    fn persist_divergence(
        &self,
        store: &Persistence,
        identity: Identity,
        block: &AuthenticatedBlock,
        failure: Failure,
    ) -> Result<(), PersistenceError> {
        let typed = failure.finding.ok_or_else(|| {
            PersistenceError::Invalid("authenticated divergence missing typed finding".into())
        })?;
        let (category, code, location, operation, expected, actual) = finding_parts(&typed);
        let (evidence_len, evidence_digest) = evidence_identity(&expected, &actual)?;
        let key = FindingKey {
            zone: block.zone,
            operation,
            code,
        };
        store.record_finding(
            identity,
            key,
            Finding {
                zone: block.zone,
                parent: block.parent,
                imported_tempo: Some(block.tempo),
                category,
                code,
                location,
                operation,
                expected,
                actual,
                evidence_len,
                evidence_digest,
                summary: failure.message,
            },
        )?;
        Ok(())
    }
}

fn finding_parts(finding: &CompactFinding) -> (u16, u16, u16, u32, Vec<u8>, Vec<u8>) {
    let category = match finding.category {
        ViolationCategory::Authentication => 1,
        ViolationCategory::EffectMismatch => 2,
        ViolationCategory::StateMismatch => 3,
        ViolationCategory::Invariant => 4,
        ViolationCategory::Unsupported => 5,
        ViolationCategory::Observation => 6,
        ViolationCategory::Continuity => 7,
        ViolationCategory::CreationAnchor => 8,
        ViolationCategory::SupplyMismatch => 9,
        ViolationCategory::CollateralMismatch => 10,
    };
    let (location, operation) = match &finding.location {
        None => (0, 0),
        Some(FindingLocation::Block) => (1, 0),
        Some(FindingLocation::Operation(op)) => (2, *op),
        Some(FindingLocation::ImportedOperation(op)) => (3, *op),
        Some(FindingLocation::State(_)) => (4, 0),
    };
    (
        category,
        finding.code,
        location,
        operation,
        finding
            .expected
            .as_ref()
            .map(FindingData::canonical_bytes)
            .unwrap_or_default(),
        finding
            .actual
            .as_ref()
            .map(FindingData::canonical_bytes)
            .unwrap_or_default(),
    )
}

fn evidence_identity(expected: &[u8], actual: &[u8]) -> Result<(u32, B256), PersistenceError> {
    let mut bytes = Vec::with_capacity(8 + expected.len() + actual.len());
    bytes.extend(
        u32::try_from(expected.len())
            .map_err(|_| PersistenceError::Invalid("expected evidence too large".into()))?
            .to_be_bytes(),
    );
    bytes.extend(expected);
    bytes.extend(
        u32::try_from(actual.len())
            .map_err(|_| PersistenceError::Invalid("actual evidence too large".into()))?
            .to_be_bytes(),
    );
    bytes.extend(actual);
    Ok((
        u32::try_from(bytes.len())
            .map_err(|_| PersistenceError::Invalid("evidence too large".into()))?,
        alloy_primitives::keccak256(bytes),
    ))
}

/// Locally derives identity and replays every block through the exact same
/// authenticated pipeline and kernel used by the live path. There is
/// intentionally no API accepting an imported checkpoint.
pub(crate) struct BuildConfig<'a> {
    pub path: &'a Path,
    pub l1_chain_id: u64,
    pub zone_chain_id: u64,
    pub creation_block: alloy_primitives::B256,
    pub creation_height: u64,
    pub portal_identity: PortalIdentity,
    pub anchor: ChainCut,
}

#[cfg(test)]
pub(crate) fn build_checkpoint<N: PlannedNotification, P: ObservationPipeline<N>>(
    config: BuildConfig<'_>,
    history: &[N],
    pipeline: &mut P,
) -> Result<Snapshot, PersistenceError> {
    let target = config.path.to_path_buf();
    if target.exists() && !directory_is_empty(&target)? {
        return Err(PersistenceError::Invalid(
            "checkpoint target is unrelated nonempty state".into(),
        ));
    }
    let staging = staging_path(&target)?;
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|error| {
            PersistenceError::Invalid(format!("cannot clean stale checkpoint staging: {error}"))
        })?;
    }
    fs::create_dir_all(&staging).map_err(|error| {
        PersistenceError::Invalid(format!("cannot create checkpoint staging: {error}"))
    })?;
    let staged = BuildConfig {
        path: &staging,
        l1_chain_id: config.l1_chain_id,
        zone_chain_id: config.zone_chain_id,
        creation_block: config.creation_block,
        creation_height: config.creation_height,
        portal_identity: config.portal_identity,
        anchor: config.anchor,
    };
    let result = build_checkpoint_in_place(staged, history, pipeline);
    match result {
        Ok(snapshot) => {
            // MDBX handles were dropped by `build_checkpoint_in_place`; only a
            // fully reopened and validated image reaches this publication point.
            if target.exists() {
                fs::remove_dir(&target).map_err(|error| {
                    PersistenceError::Invalid(format!("cannot replace empty target: {error}"))
                })?;
            }
            if let Err(error) = fs::rename(&staging, &target) {
                let _ = fs::remove_dir_all(&staging);
                return Err(PersistenceError::Invalid(format!(
                    "cannot atomically publish checkpoint: {error}"
                )));
            }
            Ok(snapshot)
        }
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            Err(error)
        }
    }
}

/// Atomically publish an identity-bound genesis checkpoint assembled by
/// an authenticated bootstrap replay.
pub(crate) fn publish_genesis_checkpoint(
    config: BuildConfig<'_>,
    state: State,
) -> Result<Snapshot, PersistenceError> {
    let target = config.path.to_path_buf();
    if target.exists() && !directory_is_empty(&target)? {
        return Err(PersistenceError::Invalid(
            "checkpoint target is unrelated nonempty state".into(),
        ));
    }
    let staging = staging_path(&target)?;
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|error| {
            PersistenceError::Invalid(format!("cannot clean stale checkpoint staging: {error}"))
        })?;
    }
    fs::create_dir_all(&staging).map_err(|error| {
        PersistenceError::Invalid(format!("cannot create checkpoint staging: {error}"))
    })?;
    let identity = Identity {
        l1_chain_id: config.l1_chain_id,
        zone_chain_id: config.zone_chain_id,
        zone_id: config.portal_identity.zone_id,
        portal: config.portal_identity.portal,
        creation_block: config.creation_block,
        creation_height: config.creation_height,
    };
    let result = (|| {
        validate(&state)
            .map_err(|error| PersistenceError::Invalid(format!("bootstrap invariant {error:?}")))?;
        let (store, snapshot) = Persistence::create(&staging, identity, config.anchor, state)?;
        drop(store);
        let (reopened, verified) = Persistence::open(&staging, identity)?;
        drop(reopened);
        if snapshot != verified {
            return Err(PersistenceError::Invalid(
                "genesis checkpoint changed across final reopen".into(),
            ));
        }
        Ok(verified)
    })();
    match result {
        Ok(snapshot) => {
            if target.exists() {
                fs::remove_dir(&target).map_err(|error| {
                    PersistenceError::Invalid(format!("cannot replace empty target: {error}"))
                })?;
            }
            fs::rename(&staging, &target).map_err(|error| {
                let _ = fs::remove_dir_all(&staging);
                PersistenceError::Invalid(format!("cannot atomically publish checkpoint: {error}"))
            })?;
            Ok(snapshot)
        }
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            Err(error)
        }
    }
}

fn directory_is_empty(path: &Path) -> Result<bool, PersistenceError> {
    if !path.is_dir() {
        return Ok(false);
    }
    fs::read_dir(path)
        .map_err(|error| PersistenceError::Invalid(format!("cannot inspect target: {error}")))
        .map(|mut entries| entries.next().is_none())
}

fn staging_path(target: &Path) -> Result<PathBuf, PersistenceError> {
    let parent = target.parent().ok_or_else(|| {
        PersistenceError::Invalid("checkpoint target has no sibling directory".into())
    })?;
    let name = target.file_name().ok_or_else(|| {
        PersistenceError::Invalid("checkpoint target has no directory name".into())
    })?;
    Ok(parent.join(format!(
        ".{}.staging-{}",
        name.to_string_lossy(),
        std::process::id()
    )))
}

#[cfg(test)]
fn build_checkpoint_in_place<N: PlannedNotification, P: ObservationPipeline<N>>(
    config: BuildConfig<'_>,
    history: &[N],
    pipeline: &mut P,
) -> Result<Snapshot, PersistenceError> {
    let BuildConfig {
        path,
        l1_chain_id,
        zone_chain_id,
        creation_block,
        creation_height,
        portal_identity,
        anchor,
    } = config;
    let identity = Identity {
        l1_chain_id,
        zone_chain_id,
        zone_id: portal_identity.zone_id,
        portal: portal_identity.portal,
        creation_block,
        creation_height,
    };
    let initial = State::awaiting(portal_identity);
    validate(&initial)
        .map_err(|e| PersistenceError::Invalid(format!("initial invariant {e:?}")))?;
    let (store, mut snapshot) = Persistence::create(path, identity, anchor, initial)?;
    for notification in history {
        let plan = notification
            .plan()
            .and_then(NotificationPlan::validate)
            .map_err(|f| PersistenceError::Invalid(format!("local replay plan: {}", f.message)))?;
        if !plan.reverted.is_empty() {
            snapshot = store.reorg(identity, plan.ancestor)?;
        }
        for index in 0..plan.applied.len() {
            let block = pipeline
                .authenticate_at(notification, index, &snapshot.state)
                .map_err(|f| PersistenceError::Invalid(format!("local replay: {}", f.message)))?;
            validate_creation_coordinate(identity, &snapshot.state, &block)
                .map_err(|f| PersistenceError::Invalid(format!("local replay: {}", f.message)))?;
            let candidate = apply_zone(
                apply_imported(&snapshot.state, &block.imported)
                    .map_err(|e| PersistenceError::Invalid(e.to_string()))?,
                &block.zone_facts,
            )
            .map_err(|e| PersistenceError::Invalid(e.to_string()))?;
            pipeline.compare(&block, &candidate).map_err(|f| {
                PersistenceError::Invalid(format!("local replay comparison: {}", f.message))
            })?;
            snapshot = store.apply(
                identity,
                JournalEntry {
                    zone: block.zone,
                    parent: block.parent,
                    imported_tempo: block.tempo,
                    imported_tempo_parent: block.tempo_parent,
                    delta: candidate.delta,
                },
                block.zone,
                crate::persistence::Coverage::Complete,
            )?;
        }
    }
    validate(&snapshot.state)
        .map_err(|e| PersistenceError::Invalid(format!("replay invariant {e:?}")))?;
    let completed = store.checkpoint(
        identity,
        ChainCut {
            zone: snapshot.meta.verified_zone_tip,
            tempo: snapshot.meta.imported_tempo_tip,
        },
        snapshot.state,
    )?;
    drop(store);
    // A successful build is not publishable until an independent open has
    // replayed the journal and rerun all persistence/kernel invariants.
    let (reopened, verified) = Persistence::open(path, identity)?;
    drop(reopened);
    if completed != verified {
        return Err(PersistenceError::Invalid(
            "checkpoint changed across final reopen".into(),
        ));
    }
    Ok(verified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU64;

    #[test]
    fn observation_projection_omits_only_unavailable_carrier_ids() {
        let processed = ExpectedEffect::UserWithdrawalProcessed {
            id: WithdrawalId {
                zone_id: 7,
                index: 99,
            },
            to: Address::repeat_byte(1),
            sender_tag: B256::repeat_byte(2),
            token: Address::repeat_byte(3),
            amount: 4,
            callback_success: true,
        };
        assert_eq!(
            ObservedEffect::from(&processed),
            ObservedEffect::UserWithdrawalProcessed {
                to: Address::repeat_byte(1),
                sender_tag: B256::repeat_byte(2),
                token: Address::repeat_byte(3),
                amount: 4,
                callback_success: true,
            }
        );

        let refund = |number| ExpectedEffect::FailedDepositRefunded {
            deposit: DepositId {
                portal: Address::repeat_byte(5),
                number: NonZeroU64::new(number).unwrap(),
            },
            recipient: Address::repeat_byte(6),
            token: Address::repeat_byte(7),
            amount: 8,
            fee: 9,
            pending: false,
        };
        assert_eq!(
            ObservedEffect::from(&refund(1)),
            ObservedEffect::from(&refund(42)),
            "DepositBounceBack has no DepositId carrier"
        );

        let mut mutated = ObservedEffect::from(&processed);
        let ObservedEffect::UserWithdrawalProcessed { amount, .. } = &mut mutated else {
            unreachable!()
        };
        *amount += 1;
        assert_ne!(mutated, ObservedEffect::from(&processed));
    }

    #[test]
    fn bounded_fifo_keeps_one_current_and_order() {
        let mut runtime = Runtime::new(2, RetryBudget::new(1, Duration::ZERO));
        assert_eq!(runtime.state(), RuntimeState::Starting);
        assert!(runtime.push(1).is_ok());
        assert!(runtime.push(2).is_ok());
        assert!(runtime.push(3).is_ok());
        assert_eq!(runtime.push(4), Err(4));
        runtime.advance();
        assert_eq!(runtime.current, Some(2));
        runtime.advance();
        assert_eq!(runtime.current, Some(3));
    }
    #[test]
    fn attempt_and_elapsed_budgets_are_both_terminal() {
        let attempts = RetryBudget::new(2, Duration::from_secs(60));
        let now = Instant::now();
        assert!(!attempts.exhausted(1, now, now));
        assert!(attempts.exhausted(2, now, now));
        let elapsed = RetryBudget::new(99, Duration::ZERO);
        assert!(elapsed.exhausted(0, now, now));
    }
}
