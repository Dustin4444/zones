use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use crate::kernel::{
    Datum, Deposit, DepositOutcome, Effect, ExpectedState, Finding, FindingCategory,
    FindingLocation, ImportedFacts, ImportedOperation, State, ZoneFacts, ZoneOperation,
    apply_imported, apply_zone,
};
use alloy_primitives::keccak256;

use crate::{
    observe::{AcquisitionError, ObservationError},
    persistence::{
        BlockNumHash, CoverageGapReason, Identity, JournalEntry, Persistence, PersistenceError,
        Snapshot, make_finding,
    },
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
    pub finding: Option<Box<Finding>>,
}

impl Failure {
    pub(crate) fn terminal(message: impl Into<String>) -> Self {
        Self {
            class: FailureClass::ImmediateTerminal,
            gap_reason: CoverageGapReason::Other(2),
            message: message.into(),
            finding: None,
        }
    }

    pub(crate) fn transient(message: impl Into<String>) -> Self {
        Self {
            class: FailureClass::TransientRetry,
            gap_reason: CoverageGapReason::ProviderUnavailable,
            message: message.into(),
            finding: None,
        }
    }

    fn incomplete(message: impl Into<String>) -> Self {
        Self {
            class: FailureClass::BoundedRetry,
            gap_reason: CoverageGapReason::MissingTempoData,
            message: message.into(),
            finding: None,
        }
    }

    pub(crate) fn authenticated_divergence(message: impl Into<String>, finding: Finding) -> Self {
        Self {
            class: FailureClass::AuthenticatedDivergence,
            gap_reason: CoverageGapReason::NotCheckedAncestorDivergence,
            message: message.into(),
            finding: Some(Box::new(finding)),
        }
    }
}

impl From<AcquisitionError> for Failure {
    fn from(error: AcquisitionError) -> Self {
        let message = error.to_string();
        match error {
            AcquisitionError::Unavailable { .. } => Self::transient(message),
            AcquisitionError::Missing { .. } | AcquisitionError::Inconsistent { .. } => {
                Self::incomplete(message)
            }
        }
    }
}

impl From<ObservationError> for Failure {
    fn from(error: ObservationError) -> Self {
        let message = error.to_string();
        match error {
            ObservationError::Acquisition(error) => error.into(),
            ObservationError::MalformedAuthenticatedData {
                transaction,
                evidence,
                ..
            } => Self::authenticated_divergence(
                message,
                Finding::new(
                    FindingCategory::Observation,
                    110,
                    Some(FindingLocation::Operation(
                        transaction.transaction_index() as u32
                    )),
                    None,
                    Some(Datum::Bytes {
                        length: evidence.length(),
                        digest: evidence.digest(),
                    }),
                ),
            ),
            ObservationError::InvalidEnvelope { .. } => Self::authenticated_divergence(
                message,
                Finding::coded(FindingCategory::Observation, 120, FindingLocation::Block),
            ),
            ObservationError::ProtocolEvent {
                transaction_index, ..
            } => Self::authenticated_divergence(
                message,
                Finding::coded(
                    FindingCategory::Observation,
                    130,
                    FindingLocation::Operation(transaction_index as u32),
                ),
            ),
            ObservationError::PortalCall(_) => Self::authenticated_divergence(
                message,
                Finding::coded(FindingCategory::Observation, 140, FindingLocation::Block),
            ),
        }
    }
}

fn typed_failure(
    category: FindingCategory,
    code: u16,
    location: Option<FindingLocation>,
    expected: Option<Datum>,
    actual: Option<Datum>,
    message: impl Into<String>,
) -> Failure {
    Failure {
        class: FailureClass::AuthenticatedDivergence,
        gap_reason: CoverageGapReason::NotCheckedAncestorDivergence,
        message: message.into(),
        finding: Some(Box::new(Finding::new(
            category, code, location, expected, actual,
        ))),
    }
}

/// Receipt- and state-derived output, constructed separately from the kernel result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthenticatedOutputs {
    pub effects: Vec<Effect>,
    pub state: ExpectedState,
    /// Exact token supplies read at this block, keyed by token address.
    pub supplies: std::collections::BTreeMap<alloy_primitives::Address, alloy_primitives::U256>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotificationPlan {
    pub reverted: Vec<BlockNumHash>,
    pub ancestor: BlockNumHash,
    pub applied: Vec<BlockNumHash>,
    pub acknowledge: BlockNumHash,
}

impl NotificationPlan {
    pub(crate) fn new(
        reverted: Vec<BlockNumHash>,
        applied: Vec<BlockNumHash>,
        ancestor: BlockNumHash,
    ) -> Result<Self, Failure> {
        let acknowledge = applied.last().copied().unwrap_or(ancestor);
        Self {
            reverted,
            ancestor,
            applied,
            acknowledge,
        }
        .validate()
    }

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
            return Err(Failure::terminal("invalid notification shape"));
        }
        Ok(self)
    }
}

/// Builds and validates a plan using only notification-owned headers.
pub(crate) trait PlannedNotification {
    fn plan(&self) -> Result<NotificationPlan, Failure>;
}

pub(crate) trait ObservationPipeline<N> {
    /// Authenticate exactly one block. `parent_state` is the durable state at
    /// the block's parent; implementations must not speculate across blocks.
    fn authenticate_at(
        &mut self,
        notification: &N,
        index: usize,
        parent_state: &State,
    ) -> Result<AuthenticatedBlock, Failure>;
}

pub(crate) fn compare_authenticated(
    block: &AuthenticatedBlock,
    candidate: &crate::kernel::Candidate,
) -> Result<(), Failure> {
    let effects = &candidate.expected_effects;
    let supplies = candidate
        .expected_accounting
        .iter()
        .map(|(token, a)| (*token, a.supply))
        .collect();
    let observed = &block.outputs.state;
    let expected = &candidate.expected_state;
    if block.outputs.effects.as_slice() != effects.as_slice() {
        let index = block
            .outputs
            .effects
            .iter()
            .zip(effects)
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| block.outputs.effects.len().min(effects.len()));
        let expected_kind = effects.get(index).map_or("Missing", Effect::kind);
        let actual_kind = block
            .outputs
            .effects
            .get(index)
            .map_or("Missing", Effect::kind);
        let evidence = |effect: Option<&Effect>| {
            effect.map(|value| {
                let bytes = format!("{value:?}").into_bytes();
                Datum::Bytes {
                    length: bytes.len() as u64,
                    digest: keccak256(bytes),
                }
            })
        };
        return Err(typed_failure(
            FindingCategory::EffectMismatch,
            1,
            Some(FindingLocation::Operation(index as u32)),
            evidence(effects.get(index)),
            evidence(block.outputs.effects.get(index)),
            format!("effect mismatch: expected {expected_kind}, actual {actual_kind}"),
        ));
    }
    macro_rules! commitment {
        ($field:ident, $datum:ident, $code:expr) => {
            if observed.$field != expected.$field {
                return Err(typed_failure(
                    FindingCategory::StateMismatch,
                    $code,
                    Some(FindingLocation::Block),
                    Some(Datum::$datum(expected.$field.into())),
                    Some(Datum::$datum(observed.$field.into())),
                    "state commitment mismatch",
                ));
            }
        };
    }
    commitment!(tempo_block_hash, Hash, 20);
    commitment!(tempo_block_number, U64, 21);
    commitment!(processed_deposit_hash, Hash, 22);
    commitment!(processed_deposit_number, U64, 23);
    commitment!(withdrawal_queue_hash, Hash, 24);
    commitment!(withdrawal_batch_index, U64, 25);
    if block.outputs.supplies != supplies {
        let token = block
            .outputs
            .supplies
            .keys()
            .chain(supplies.keys())
            .find(|token| block.outputs.supplies.get(*token) != supplies.get(*token))
            .copied()
            .expect("unequal maps have a differing key");
        return Err(typed_failure(
            FindingCategory::SupplyMismatch,
            30,
            Some(FindingLocation::State(crate::kernel::StateKey::Token(
                token,
            ))),
            supplies.get(&token).copied().map(Datum::U256),
            block.outputs.supplies.get(&token).copied().map(Datum::U256),
            "token supply mismatch",
        ));
    }
    Ok(())
}

fn validate_creation_coordinate(
    identity: Identity,
    state: &State,
    block: &AuthenticatedBlock,
) -> Result<(), Failure> {
    use crate::kernel::{ImportedOperation, PortalState};

    // Height zero is retained as the pre-creation-checkpoint sentinel.
    if identity.creation_height == 0 {
        return Ok(());
    }
    let awaiting = matches!(state.portal(), Some(PortalState::AwaitingCreation(_)));
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
            FindingCategory::CreationAnchor,
            1,
            Some(FindingLocation::Block),
            Some(Datum::Hash(identity.creation_block)),
            Some(Datum::Hash(block.tempo.hash)),
            "portal creation anchor mismatch",
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

/// Exactly one current notification and one bounded queue.
pub(crate) struct Runtime<N> {
    snapshot: Snapshot,
    state: RuntimeState,
    current: Option<QueuedNotification<N>>,
    queue: VecDeque<QueuedNotification<N>>,
    capacity: usize,
    budget: RetryBudget,
    retry: Option<RetryState>,
    current_unwound: bool,
}

struct QueuedNotification<N> {
    notification: N,
    plan: NotificationPlan,
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

/// Outcome of enqueueing a notification into the bounded runtime queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnqueueAction {
    Queued,
    AcknowledgeAndTerminate(BlockNumHash),
    Terminal,
}

/// Outcome of durably recording a notification-stream failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamFailureAction {
    GapRecorded(BlockNumHash),
    Terminal,
}

impl<N> Runtime<N> {
    pub(crate) fn new(snapshot: Snapshot, capacity: usize, budget: RetryBudget) -> Self {
        Self {
            snapshot,
            state: RuntimeState::Starting,
            current: None,
            queue: VecDeque::with_capacity(capacity),
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
    pub(crate) fn current(&self) -> Option<(&N, &NotificationPlan)> {
        self.current
            .as_ref()
            .map(|queued| (&queued.notification, &queued.plan))
    }
    pub(crate) fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    pub(crate) fn next_applied_index(
        &mut self,
        store: &Persistence,
    ) -> Result<Option<usize>, PersistenceError> {
        if self.state == RuntimeState::Disabled {
            return Ok(None);
        }
        let Some(current) = self.current.as_ref() else {
            return Ok(None);
        };
        let plan = current.plan.clone();
        if let Some(active) = self.snapshot.meta.active_finding
            && !plan.reverted.contains(&active.zone)
        {
            return Ok(None);
        }
        if !plan.reverted.is_empty() && !self.current_unwound {
            self.reorg(store, plan.ancestor)?;
            self.current_unwound = true;
        }
        if plan.applied.is_empty() {
            return Ok(None);
        }
        if self.snapshot.meta.active_finding.is_some()
            && self
                .snapshot
                .meta
                .acknowledged_zone_tip
                .number
                .checked_add(1)
                == Some(plan.applied[0].number)
        {
            return Ok(None);
        }
        let tip = self.snapshot.meta.verified_zone_tip;
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
    #[cfg(test)]
    pub(crate) fn push(&mut self, notification: N) -> Result<(), N>
    where
        N: PlannedNotification,
    {
        let plan = match notification.plan() {
            Ok(plan) => plan,
            Err(_) => return Err(notification),
        };
        self.enqueue(notification, plan)
    }

    fn enqueue(&mut self, notification: N, plan: NotificationPlan) -> Result<(), N> {
        if self.state == RuntimeState::Disabled {
            return Err(notification);
        }
        let queued = QueuedNotification { notification, plan };
        if self.current.is_none() {
            self.current = Some(queued);
            Ok(())
        } else if self.queue.len() < self.capacity {
            self.queue.push_back(queued);
            Ok(())
        } else {
            Err(queued.notification)
        }
    }

    /// Fail open on queue overflow, but only after durably naming every item
    /// that will no longer be checked.
    pub(crate) fn push_or_record_overflow(
        &mut self,
        store: &Persistence,
        notification: N,
    ) -> Result<EnqueueAction, PersistenceError>
    where
        N: PlannedNotification,
    {
        if self.state == RuntimeState::Disabled {
            return Ok(EnqueueAction::Terminal);
        }
        let plan = match notification.plan() {
            Ok(plan) => plan,
            Err(_) => {
                self.state = RuntimeState::Disabled;
                return Ok(EnqueueAction::Terminal);
            }
        };
        if self.current.is_none() || self.queue.len() < self.capacity {
            let accepted = self.enqueue(notification, plan);
            debug_assert!(accepted.is_ok());
            return Ok(EnqueueAction::Queued);
        }
        let plans = self
            .current
            .iter()
            .chain(self.queue.iter())
            .map(|queued| &queued.plan)
            .chain(std::iter::once(&plan))
            .collect::<Vec<_>>();
        let first_reaches_tip = plans.first().is_some_and(|plan| {
            plan.ancestor == self.snapshot.meta.verified_zone_tip
                || plan.applied.contains(&self.snapshot.meta.verified_zone_tip)
        });
        let valid = plans
            .iter()
            .all(|plan| plan.reverted.is_empty() && !plan.applied.is_empty())
            && first_reaches_tip
            && plans
                .windows(2)
                .all(|pair| pair[1].ancestor == pair[0].acknowledge);
        if !valid {
            self.state = RuntimeState::Disabled;
            return Ok(EnqueueAction::Terminal);
        }
        let first_plan = &plans[0];
        let first = if self.snapshot.meta.verified_zone_tip == first_plan.ancestor {
            first_plan.applied[0]
        } else if let Some(index) = first_plan
            .applied
            .iter()
            .position(|coordinate| *coordinate == self.snapshot.meta.verified_zone_tip)
        {
            *first_plan.applied.get(index + 1).ok_or_else(|| {
                PersistenceError::Invalid("overflow has no unchecked suffix".into())
            })?
        } else {
            self.state = RuntimeState::Disabled;
            return Ok(EnqueueAction::Terminal);
        };
        let last = plans.last().expect("nonempty plan set").acknowledge;
        let reason = match &self.snapshot.meta.coverage {
            crate::persistence::Coverage::Gap { reason, .. } => reason.clone(),
            crate::persistence::Coverage::Complete => CoverageGapReason::ProviderUnavailable,
        };
        self.snapshot = store.record_gap(&self.snapshot, first, last, reason)?;
        self.state = RuntimeState::Disabled;
        self.current = None;
        self.queue.clear();
        Ok(EnqueueAction::AcknowledgeAndTerminate(last))
    }

    /// Persist a canonical suffix reconstructed locally after stream failure.
    pub(crate) fn record_stream_failure(
        &mut self,
        store: &Persistence,
        canonical_suffix: &[BlockNumHash],
    ) -> Result<StreamFailureAction, PersistenceError> {
        if canonical_suffix.is_empty()
            || canonical_suffix
                .windows(2)
                .any(|pair| pair[0].number.checked_add(1) != Some(pair[1].number))
        {
            self.state = RuntimeState::Disabled;
            return Ok(StreamFailureAction::Terminal);
        }
        let (first, reason) = match &self.snapshot.meta.coverage {
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
        if self.snapshot.meta.verified_zone_tip.number.checked_add(1)
            != Some(canonical_suffix[0].number)
            || canonical_suffix[0] != first
        {
            self.state = RuntimeState::Disabled;
            return Ok(StreamFailureAction::Terminal);
        }
        self.snapshot = store.record_gap(&self.snapshot, first, last, reason)?;
        self.state = RuntimeState::Disabled;
        Ok(StreamFailureAction::GapRecorded(last))
    }

    pub(crate) fn reorg(
        &mut self,
        store: &Persistence,
        ancestor: BlockNumHash,
    ) -> Result<(), PersistenceError> {
        let snapshot = store.reorg(&self.snapshot, ancestor)?;
        self.retry = None;
        self.state = if snapshot.meta.active_finding.is_some() {
            RuntimeState::Alerting
        } else {
            RuntimeState::Starting
        };
        self.snapshot = snapshot;
        Ok(())
    }
    fn advance(&mut self) {
        self.current = self.queue.pop_front();
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
    ) -> Result<RuntimeAction, PersistenceError> {
        if self.state == RuntimeState::Disabled {
            return Ok(RuntimeAction::Terminal);
        }
        let Some(current) = self.current.as_ref() else {
            return Ok(RuntimeAction::None);
        };
        let plan = current.plan.clone();
        if let Some(active) = self.snapshot.meta.active_finding
            && !plan.reverted.contains(&active.zone)
        {
            let same_height_conflict = std::iter::once(plan.ancestor)
                .chain(plan.reverted.iter().copied())
                .chain(plan.applied.iter().copied())
                .any(|coordinate| {
                    coordinate.number == active.zone.number && coordinate.hash != active.zone.hash
                });
            let exact_commit_descendant = plan.reverted.is_empty()
                && (plan.ancestor == self.snapshot.meta.acknowledged_zone_tip
                    || plan.ancestor == active.zone);
            let exact_reorg_descendant = !plan.reverted.is_empty()
                && (plan.ancestor == active.zone
                    || plan.reverted.last().copied()
                        == Some(self.snapshot.meta.acknowledged_zone_tip));
            let preserves = !same_height_conflict
                && (plan.applied.contains(&active.zone)
                    || exact_commit_descendant
                    || exact_reorg_descendant);
            if !preserves {
                self.state = RuntimeState::Disabled;
                return Ok(RuntimeAction::Terminal);
            }
            if !plan.reverted.is_empty() && !self.current_unwound {
                self.reorg(store, plan.ancestor)?;
                self.current_unwound = true;
            }
            let first = match self.snapshot.meta.coverage {
                crate::persistence::Coverage::Gap {
                    first_unchecked, ..
                } => first_unchecked,
                crate::persistence::Coverage::Complete => active.zone,
            };
            self.snapshot = store.record_gap(
                &self.snapshot,
                first,
                plan.acknowledge,
                CoverageGapReason::NotCheckedAncestorDivergence,
            )?;
            self.state = RuntimeState::Alerting;
            self.advance();
            return Ok(RuntimeAction::Acknowledge(plan.acknowledge));
        }
        // Persist the unwind before inspecting the replacement branch.
        if !plan.reverted.is_empty() && !self.current_unwound {
            self.reorg(store, plan.ancestor)?;
            self.current_unwound = true;
        }
        if plan.applied.is_empty() {
            self.advance();
            return Ok(RuntimeAction::Acknowledge(plan.acknowledge));
        }
        let coordinates = plan.applied;
        if self.snapshot.meta.active_finding.is_some()
            && self
                .snapshot
                .meta
                .acknowledged_zone_tip
                .number
                .checked_add(1)
                == Some(coordinates[0].number)
        {
            let gap_first = match &self.snapshot.meta.coverage {
                crate::persistence::Coverage::Gap {
                    first_unchecked, ..
                } => *first_unchecked,
                crate::persistence::Coverage::Complete => coordinates[0],
            };
            self.snapshot = store.record_gap(
                &self.snapshot,
                gap_first,
                plan.acknowledge,
                CoverageGapReason::NotCheckedAncestorDivergence,
            )?;
            self.state = RuntimeState::Alerting;
            self.advance();
            return Ok(RuntimeAction::Acknowledge(plan.acknowledge));
        }
        let first = if self.snapshot.meta.verified_zone_tip == plan.ancestor {
            0
        } else if let Some(index) = coordinates
            .iter()
            .position(|coordinate| *coordinate == self.snapshot.meta.verified_zone_tip)
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
        let gap_next = match &self.snapshot.meta.coverage {
            crate::persistence::Coverage::Gap {
                acknowledged_through,
                ..
            } if coordinates[first] != *acknowledged_through => {
                let next = coordinates.get(first + 1).copied().or_else(|| {
                    self.queue.front().and_then(|next| {
                        (next.plan.ancestor == plan.acknowledge)
                            .then_some(&next.plan)
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
            &self
                .current
                .as_ref()
                .expect("current notification retained")
                .notification,
            first,
            &self.snapshot.state,
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
                    for queued in &self.queue {
                        let queued = &queued.plan;
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
                    let reason = match &self.snapshot.meta.coverage {
                        crate::persistence::Coverage::Gap { reason, .. } => reason.clone(),
                        crate::persistence::Coverage::Complete => f.gap_reason,
                    };
                    self.snapshot = store.record_gap(&self.snapshot, first, last, reason)?;
                    self.state = RuntimeState::Disabled;
                    self.current = None;
                    self.queue.clear();
                    return Ok(RuntimeAction::AcknowledgeAndTerminate(last));
                }
                retry.next_attempt = now + Duration::from_millis(25);
                return Ok(RuntimeAction::RetryAt(retry.next_attempt));
            }
            Err(failure) if failure.class == FailureClass::AuthenticatedDivergence => {
                let zone = coordinates[first];
                let last = *coordinates.last().unwrap();
                let typed = failure
                    .finding
                    .ok_or_else(|| PersistenceError::Invalid("divergence has no finding".into()))?;
                let (key, finding) = make_finding(
                    zone,
                    self.snapshot.meta.verified_zone_tip,
                    None,
                    *typed,
                    failure.message,
                )?;
                self.snapshot = store.record_finding(&self.snapshot, key, finding)?;
                self.snapshot = store.record_gap(
                    &self.snapshot,
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
            self.snapshot = store.record_gap(
                &self.snapshot,
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
        self.process_block(
            store,
            identity,
            &block,
            suffix_end,
            gap_next,
            first + 1 == coordinates.len(),
        )?;
        // Persistence above is the commit point; only now may the caller ack.
        let ready = self.snapshot.meta.acknowledged_zone_tip;
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

    fn process_block(
        &mut self,
        store: &Persistence,
        identity: Identity,
        block: &AuthenticatedBlock,
        suffix_end: BlockNumHash,
        gap_next: Option<BlockNumHash>,
        is_last: bool,
    ) -> Result<(), PersistenceError> {
        if self.snapshot.meta.active_finding.is_some() {
            let first = match self.snapshot.meta.coverage {
                crate::persistence::Coverage::Gap {
                    first_unchecked, ..
                } => first_unchecked,
                crate::persistence::Coverage::Complete => block.zone,
            };
            self.snapshot = store.record_gap(
                &self.snapshot,
                first,
                suffix_end,
                CoverageGapReason::NotCheckedAncestorDivergence,
            )?;
            self.state = RuntimeState::Alerting;
            return Ok(());
        }

        if let Err(failure) = validate_creation_coordinate(identity, &self.snapshot.state, block) {
            return self.record_divergence(store, block, suffix_end, failure);
        }
        let candidate = match apply_imported(&self.snapshot.state, &block.imported)
            .and_then(|imported| apply_zone(imported, &block.zone_facts))
        {
            Ok(candidate) => candidate,
            Err(error) => {
                return self.record_divergence(
                    store,
                    block,
                    suffix_end,
                    typed_failure(
                        FindingCategory::Invariant,
                        1,
                        Some(FindingLocation::Block),
                        None,
                        Some(Datum::Code(1)),
                        error.to_string(),
                    ),
                );
            }
        };
        if let Err(failure) = compare_authenticated(block, &candidate) {
            if failure.class == FailureClass::AuthenticatedDivergence {
                return self.record_divergence(store, block, suffix_end, failure);
            }
            self.snapshot =
                store.record_gap(&self.snapshot, block.zone, suffix_end, failure.gap_reason)?;
            self.state = RuntimeState::Disabled;
            return Ok(());
        }

        let coverage = match &self.snapshot.meta.coverage {
            crate::persistence::Coverage::Gap {
                first_unchecked,
                acknowledged_through,
                reason,
            } if *first_unchecked == block.zone => {
                if block.zone == *acknowledged_through {
                    crate::persistence::Coverage::Complete
                } else {
                    crate::persistence::Coverage::Gap {
                        first_unchecked: gap_next.ok_or_else(|| {
                            PersistenceError::Invalid("durable gap has no next coordinate".into())
                        })?,
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
        self.snapshot = store.apply(
            &self.snapshot,
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
        log_verified_block(block);
        if is_last {
            self.state = RuntimeState::Healthy;
        }
        Ok(())
    }

    fn record_divergence(
        &mut self,
        store: &Persistence,
        block: &AuthenticatedBlock,
        suffix_end: BlockNumHash,
        failure: Failure,
    ) -> Result<(), PersistenceError> {
        let typed = failure
            .finding
            .ok_or_else(|| PersistenceError::Invalid("divergence has no finding".into()))?;
        let (key, finding) = make_finding(
            block.zone,
            block.parent,
            Some((block.tempo, block.tempo_parent)),
            *typed,
            failure.message,
        )?;
        let category = finding.details.category;
        let code = finding.details.code;
        let location = finding.details.location.clone();
        let summary = finding.summary.clone();
        self.snapshot = store.record_finding(&self.snapshot, key, finding)?;
        self.snapshot = store.record_gap(
            &self.snapshot,
            block.zone,
            suffix_end,
            CoverageGapReason::NotCheckedAncestorDivergence,
        )?;
        tracing::error!(
            target: "zone::checker",
            zone_block = block.zone.number,
            zone_hash = %block.zone.hash,
            tempo_block = block.tempo.number,
            tempo_hash = %block.tempo.hash,
            ?category,
            code,
            ?location,
            summary,
            "checker recorded authenticated divergence"
        );
        self.state = RuntimeState::Alerting;
        Ok(())
    }
}

/// Report only transitions that passed comparison and were committed durably.
fn log_verified_block(block: &AuthenticatedBlock) {
    tracing::debug!(
        target: "zone::checker",
        zone_block = block.zone.number,
        zone_hash = %block.zone.hash,
        tempo_block = block.tempo.number,
        tempo_hash = %block.tempo.hash,
        imported_operations = block.imported.operations.len(),
        enabled_tokens = block.zone_facts.enabled_tokens.len(),
        deposits = block.zone_facts.deposits.len(),
        deposit_outcomes = block.zone_facts.outcomes.len(),
        zone_operations = block.zone_facts.operations.len(),
        finalized = block.zone_facts.finalization.is_some(),
        "verified Zone block"
    );

    for operation in &block.imported.operations {
        match operation {
            ImportedOperation::Create {
                identity,
                initial_token,
            } => {
                tracing::info!(
                    target: "zone::checker",
                    zone_block = block.zone.number,
                    tempo_block = block.tempo.number,
                    portal = %identity.portal,
                    zone_id = identity.zone_id,
                    "verified Portal creation"
                );
                log_verified_token(block, initial_token);
            }
            ImportedOperation::EnableToken(token) => log_verified_token(block, token),
            ImportedOperation::AppendDeposit(_) => {}
            ImportedOperation::SubmitBatch(batch) => tracing::info!(
                target: "zone::checker",
                zone_block = block.zone.number,
                tempo_block = block.tempo.number,
                next_zone_height = %batch.next_zone_height,
                "verified batch submission"
            ),
            ImportedOperation::ProcessWithdrawals(processing) => tracing::info!(
                target: "zone::checker",
                zone_block = block.zone.number,
                tempo_block = block.tempo.number,
                withdrawals = processing.withdrawals.len(),
                outcomes = processing.outcomes.len(),
                "verified withdrawal processing"
            ),
            ImportedOperation::ClaimPortalRefund(refund) => tracing::info!(
                target: "zone::checker",
                zone_block = block.zone.number,
                tempo_block = block.tempo.number,
                token = %refund.token,
                amount = refund.amount,
                "verified Portal refund claim"
            ),
            ImportedOperation::UpdateBouncebackGas(gas) => tracing::info!(
                target: "zone::checker",
                zone_block = block.zone.number,
                tempo_block = block.tempo.number,
                bounceback_gas = gas,
                "verified bounce-back gas update"
            ),
        }
    }

    for (deposit, outcome) in block
        .zone_facts
        .deposits
        .iter()
        .zip(&block.zone_facts.outcomes)
    {
        match deposit {
            Deposit::Ordinary(deposit) => tracing::info!(
                target: "zone::checker",
                zone_block = block.zone.number,
                tempo_block = block.tempo.number,
                token = %deposit.token,
                amount = deposit.amount,
                outcome = deposit_outcome_name(outcome),
                "verified deposit"
            ),
            Deposit::BounceBack(deposit) => tracing::info!(
                target: "zone::checker",
                zone_block = block.zone.number,
                tempo_block = block.tempo.number,
                token = %deposit.token,
                amount = deposit.amount,
                fallback_nonce = deposit.fallback_nonce.get(),
                outcome = deposit_outcome_name(outcome),
                "verified bounce-back deposit"
            ),
        }
    }

    for operation in &block.zone_facts.operations {
        match operation {
            ZoneOperation::AcceptWithdrawal(withdrawal) => tracing::info!(
                target: "zone::checker",
                zone_block = block.zone.number,
                tempo_block = block.tempo.number,
                token = %withdrawal.token,
                amount = withdrawal.amount,
                gas_limit = withdrawal.gas_limit,
                "verified withdrawal request"
            ),
            ZoneOperation::ClaimInboxRefund(refund) => tracing::info!(
                target: "zone::checker",
                zone_block = block.zone.number,
                tempo_block = block.tempo.number,
                token = %refund.token,
                amount = refund.amount,
                "verified Inbox refund claim"
            ),
            ZoneOperation::UpdateTempoGasRate(rate) => tracing::info!(
                target: "zone::checker",
                zone_block = block.zone.number,
                tempo_block = block.tempo.number,
                tempo_gas_rate = rate,
                "verified Tempo gas-rate update"
            ),
            ZoneOperation::UpdateMaxWithdrawals(max) => tracing::info!(
                target: "zone::checker",
                zone_block = block.zone.number,
                tempo_block = block.tempo.number,
                max_withdrawals = max,
                "verified maximum-withdrawals update"
            ),
        }
    }

    if let Some(finalization) = &block.zone_facts.finalization {
        tracing::info!(
            target: "zone::checker",
            zone_block = block.zone.number,
            tempo_block = block.tempo.number,
            finalized_block = finalization.block_number,
            withdrawals = finalization.declared_count,
            encrypted_senders = finalization.encrypted_senders.len(),
            "verified withdrawal finalization"
        );
    }
}

fn log_verified_token(block: &AuthenticatedBlock, token: &crate::kernel::TokenEnable) {
    tracing::info!(
        target: "zone::checker",
        zone_block = block.zone.number,
        tempo_block = block.tempo.number,
        token = %token.token,
        name = %token.name,
        symbol = %token.symbol,
        currency = %token.currency,
        "verified token enablement"
    );
}

const fn deposit_outcome_name(outcome: &DepositOutcome) -> &'static str {
    match outcome {
        DepositOutcome::Minted => "minted",
        DepositOutcome::Failed => "failed",
        DepositOutcome::BounceBackMinted { .. } => "bounce_back_minted",
        DepositOutcome::BounceBackPending { .. } => "bounce_back_pending",
    }
}

#[cfg(test)]
mod tests;
