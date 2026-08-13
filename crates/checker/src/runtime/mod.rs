use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

#[cfg(test)]
use crate::kernel::State;
use crate::kernel::{Effect, ExpectedState, ImportedFacts, ZoneFacts};

use crate::{
    CheckerBlockedReason,
    failure::{Failure, FailureClass},
    notification::NotificationPlan,
    persistence::{
        BlockNumHash, Coverage, CoverageGapReason, Identity, JournalEntry, Persistence,
        PersistenceError, Snapshot, make_finding,
    },
};

mod logging;
mod verification;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Durable checker lifecycle state.
pub(crate) enum RuntimeState {
    Starting,
    Healthy,
    Retrying,
    Alerting,
    /// Checking stopped after a durable coverage gap; notifications may still be drained.
    Disabled,
    /// The checker cannot safely acknowledge additional notifications.
    Blocked,
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
/// One authenticated Zone block and its imported Tempo transition.
pub(crate) struct AuthenticatedBlock {
    pub zone: BlockNumHash,
    pub parent: BlockNumHash,
    pub tempo: BlockNumHash,
    pub tempo_parent: BlockNumHash,
    pub imported: ImportedFacts,
    pub zone_facts: ZoneFacts,
    pub outputs: AuthenticatedOutputs,
}

/// Bounds how long acquisition failures may delay checking.
pub(crate) struct RetryBudget {
    max_attempts: u32,
    max_elapsed: Duration,
}

impl RetryBudget {
    /// Create a budget limited by attempts and elapsed time.
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
    processed_notification_tip: Option<BlockNumHash>,
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

/// The next block selected from the current notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockWork {
    index: usize,
    coordinate: BlockNumHash,
    suffix_end: BlockNumHash,
    gap_next: Option<BlockNumHash>,
    is_last: bool,
}

/// One block the ExEx must authenticate before the runtime can progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AuthenticationRequest {
    work: BlockWork,
}

impl AuthenticationRequest {
    /// Index of this block within the current applied notification fragment.
    pub(crate) const fn index(self) -> usize {
        self.work.index
    }
}

enum WorkSelection {
    Work(BlockWork),
    Complete,
    AwaitNotification,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Work the ExEx must perform after one runtime step.
pub(crate) enum RuntimeAction {
    None,
    Authenticate(AuthenticationRequest),
    AwaitNotification,
    RetryAt(Instant),
    Acknowledge(BlockNumHash),
    /// Acknowledge a durably recorded gap and continue draining notifications.
    AcknowledgeAndDisable(BlockNumHash),
    /// Keep the ExEx alive without acknowledging further notifications.
    Blocked,
}

/// Outcome of enqueueing a notification into the bounded runtime queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnqueueAction {
    Queued,
    Acknowledge(BlockNumHash),
    AcknowledgeAndDisable(BlockNumHash),
    Blocked,
}

/// Outcome of durably recording a notification-stream failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamFailureAction {
    GapRecorded(BlockNumHash),
    Blocked,
}

impl<N> Runtime<N> {
    /// Start from a durable snapshot with one current notification and a bounded backlog.
    pub(crate) fn new(snapshot: Snapshot, capacity: usize, budget: RetryBudget) -> Self {
        let state = if snapshot.meta.blocked.is_some() {
            RuntimeState::Blocked
        } else {
            RuntimeState::Starting
        };
        Self {
            snapshot,
            state,
            current: None,
            queue: VecDeque::with_capacity(capacity),
            capacity,
            budget,
            retry: None,
            current_unwound: false,
            processed_notification_tip: None,
        }
    }
    #[cfg(test)]
    pub(crate) fn state(&self) -> RuntimeState {
        self.state
    }
    /// Return the notification currently being applied and its validated plan.
    pub(crate) fn current(&self) -> Option<(&N, &NotificationPlan)> {
        self.current
            .as_ref()
            .map(|queued| (&queued.notification, &queued.plan))
    }
    /// Return the latest durable snapshot.
    pub(crate) fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// Persist a blocked state without advancing the acknowledgement watermark.
    pub(crate) fn block(
        &mut self,
        store: &Persistence,
        reason: CheckerBlockedReason,
    ) -> Result<(), PersistenceError> {
        self.snapshot = store.record_blocked_current(reason)?;
        self.state = RuntimeState::Blocked;
        self.current = None;
        self.queue.clear();
        self.retry = None;
        self.processed_notification_tip = None;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn push_planned(
        &mut self,
        notification: N,
        plan: NotificationPlan,
    ) -> Result<(), N> {
        self.enqueue(notification, plan)
    }

    /// Append a validated notification without recording an overflow gap.
    fn enqueue(&mut self, notification: N, plan: NotificationPlan) -> Result<(), N> {
        if matches!(self.state, RuntimeState::Disabled | RuntimeState::Blocked) {
            return Err(notification);
        }
        let queued = QueuedNotification { notification, plan };
        match self.current {
            None => self.current = Some(queued),
            Some(_) if self.queue.len() < self.capacity => self.queue.push_back(queued),
            Some(_) => return Err(queued.notification),
        }
        Ok(())
    }

    /// Fail open on queue overflow, but only after durably naming every item
    /// that will no longer be checked.
    pub(crate) fn push_planned_or_record_overflow(
        &mut self,
        store: &Persistence,
        notification: N,
        plan: NotificationPlan,
    ) -> Result<EnqueueAction, PersistenceError> {
        if self.state == RuntimeState::Disabled {
            return self.drain_disabled(store, notification, plan);
        }
        if self.state == RuntimeState::Blocked {
            return Ok(EnqueueAction::Blocked);
        }
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
            self.block(store, CheckerBlockedReason::InvalidNotificationSequence)?;
            return Ok(EnqueueAction::Blocked);
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
            self.block(store, CheckerBlockedReason::InvalidNotificationSequence)?;
            return Ok(EnqueueAction::Blocked);
        };
        let last = plans.last().expect("nonempty plan set").acknowledge;
        let reason = match &self.snapshot.meta.coverage {
            Coverage::Gap { reason, .. } => reason.clone(),
            Coverage::Complete => CoverageGapReason::ProviderUnavailable,
        };
        self.snapshot = store.record_gap(&self.snapshot, first, last, reason)?;
        self.state = RuntimeState::Disabled;
        self.current = None;
        self.queue.clear();
        Ok(EnqueueAction::AcknowledgeAndDisable(last))
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
            self.block(store, CheckerBlockedReason::NotificationStreamUnavailable)?;
            return Ok(StreamFailureAction::Blocked);
        }
        let (first, reason) = match &self.snapshot.meta.coverage {
            Coverage::Complete => (canonical_suffix[0], CoverageGapReason::ProviderUnavailable),
            Coverage::Gap {
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
            self.block(store, CheckerBlockedReason::NotificationStreamUnavailable)?;
            return Ok(StreamFailureAction::Blocked);
        }
        self.snapshot = store.record_gap(&self.snapshot, first, last, reason)?;
        self.state = RuntimeState::Disabled;
        Ok(StreamFailureAction::GapRecorded(last))
    }

    /// Extend or rewind durable gap coverage for one notification after checking is disabled.
    fn drain_disabled(
        &mut self,
        store: &Persistence,
        notification: N,
        plan: NotificationPlan,
    ) -> Result<EnqueueAction, PersistenceError> {
        let Coverage::Gap { reason, .. } = &self.snapshot.meta.coverage else {
            self.block(store, CheckerBlockedReason::InvalidNotificationSequence)?;
            return Ok(EnqueueAction::Blocked);
        };
        let reason = reason.clone();
        if plan.ancestor != self.snapshot.meta.acknowledged_zone_tip {
            if plan.reverted.is_empty() {
                self.block(store, CheckerBlockedReason::InvalidNotificationSequence)?;
                return Ok(EnqueueAction::Blocked);
            }
            if plan.reverted.last().copied() != Some(self.snapshot.meta.acknowledged_zone_tip) {
                self.block(store, CheckerBlockedReason::InvalidNotificationSequence)?;
                return Ok(EnqueueAction::Blocked);
            }
            self.reorg(store, plan.ancestor)?;
        }
        if matches!(self.snapshot.meta.coverage, Coverage::Complete) {
            if plan.applied.is_empty() {
                return Ok(EnqueueAction::Acknowledge(plan.acknowledge));
            }
            let accepted = self.enqueue(notification, plan);
            debug_assert!(accepted.is_ok());
            return Ok(EnqueueAction::Queued);
        }
        if plan.applied.is_empty() {
            self.state = RuntimeState::Disabled;
            return Ok(EnqueueAction::AcknowledgeAndDisable(plan.acknowledge));
        }
        let first = self.first_unchecked_or(plan.applied[0]);
        self.snapshot = store.record_gap(&self.snapshot, first, plan.acknowledge, reason)?;
        self.state = RuntimeState::Disabled;
        Ok(EnqueueAction::AcknowledgeAndDisable(plan.acknowledge))
    }

    /// Restore the durable state at `ancestor` and reset in-flight retry state.
    pub(crate) fn reorg(
        &mut self,
        store: &Persistence,
        ancestor: BlockNumHash,
    ) -> Result<(), PersistenceError> {
        let snapshot = store.reorg(&self.snapshot, ancestor)?;
        self.retry = None;
        self.processed_notification_tip = Some(ancestor);
        self.state = if snapshot.meta.active_finding.is_some() {
            RuntimeState::Alerting
        } else {
            RuntimeState::Starting
        };
        self.snapshot = snapshot;
        Ok(())
    }
    /// Discard the completed current notification and promote the next queued item.
    fn advance(&mut self) {
        self.processed_notification_tip = self
            .current
            .as_ref()
            .map(|current| current.plan.acknowledge);
        self.current = self.queue.pop_front();
        self.retry = None;
        self.current_unwound = false;
    }

    /// Return the durable start of an unchecked range, or `fallback` when complete.
    fn first_unchecked_or(&self, fallback: BlockNumHash) -> BlockNumHash {
        match self.snapshot.meta.coverage {
            Coverage::Gap {
                first_unchecked, ..
            } => first_unchecked,
            Coverage::Complete => fallback,
        }
    }

    /// Extend a divergence gap without regressing its durable acknowledgement.
    fn record_unchecked_descendants(
        &mut self,
        store: &Persistence,
        first_unchecked: BlockNumHash,
        observed_through: BlockNumHash,
    ) -> Result<BlockNumHash, PersistenceError> {
        let acknowledged_through = match &self.snapshot.meta.coverage {
            Coverage::Gap {
                acknowledged_through,
                ..
            } if acknowledged_through.number > observed_through.number => *acknowledged_through,
            _ => observed_through,
        };
        self.snapshot = store.record_gap(
            &self.snapshot,
            first_unchecked,
            acknowledged_through,
            CoverageGapReason::NotCheckedAncestorDivergence,
        )?;
        Ok(self.snapshot.meta.acknowledged_zone_tip)
    }

    /// Restore the current notification's ancestor before processing its replacement.
    fn unwind_current(
        &mut self,
        store: &Persistence,
        plan: &NotificationPlan,
    ) -> Result<(), PersistenceError> {
        if !plan.reverted.is_empty() && !self.current_unwound {
            self.reorg(store, plan.ancestor)?;
            self.current_unwound = true;
        }
        Ok(())
    }

    /// Select the next unchecked block without changing durable state.
    fn select_work(&self, plan: &NotificationPlan) -> WorkSelection {
        let applied = &plan.applied;
        let index = if self.snapshot.meta.verified_zone_tip == plan.ancestor {
            0
        } else if let Some(index) = applied
            .iter()
            .position(|coordinate| *coordinate == self.snapshot.meta.verified_zone_tip)
        {
            index + 1
        } else {
            return WorkSelection::Terminal;
        };
        if index == applied.len() {
            return WorkSelection::Complete;
        }
        let gap_next = match &self.snapshot.meta.coverage {
            Coverage::Gap {
                acknowledged_through,
                ..
            } if applied[index] != *acknowledged_through => {
                let next = applied.get(index + 1).copied().or_else(|| {
                    self.queue.front().and_then(|next| {
                        (next.plan.ancestor == plan.acknowledge)
                            .then_some(&next.plan)
                            .and_then(|plan| plan.applied.first().copied())
                    })
                });
                let Some(next) = next else {
                    return WorkSelection::AwaitNotification;
                };
                Some(next)
            }
            _ => None,
        };
        WorkSelection::Work(BlockWork {
            index,
            coordinate: applied[index],
            suffix_end: *applied.last().expect("applied fragment is nonempty"),
            gap_next,
            is_last: index + 1 == applied.len(),
        })
    }

    /// Reconcile reorgs and active findings before selecting another block.
    fn reconcile(
        &mut self,
        store: &Persistence,
        plan: &NotificationPlan,
    ) -> Result<Option<RuntimeAction>, PersistenceError> {
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
                    || plan.ancestor == active.zone
                    || self.processed_notification_tip == Some(plan.ancestor));
            let exact_reorg_descendant = !plan.reverted.is_empty()
                && (plan.ancestor == active.zone
                    || plan.reverted.last().copied()
                        == Some(self.snapshot.meta.acknowledged_zone_tip)
                    || plan.reverted.last().copied() == self.processed_notification_tip);
            let preserves = !same_height_conflict
                && (plan.applied.contains(&active.zone)
                    || exact_commit_descendant
                    || exact_reorg_descendant);
            if !preserves {
                self.block(store, CheckerBlockedReason::InvalidNotificationSequence)?;
                return Ok(Some(RuntimeAction::Blocked));
            }
            self.unwind_current(store, plan)?;
            let first = self.first_unchecked_or(active.zone);
            let acknowledged = self.record_unchecked_descendants(store, first, plan.acknowledge)?;
            self.state = RuntimeState::Alerting;
            self.advance();
            return Ok(Some(RuntimeAction::Acknowledge(acknowledged)));
        }
        self.unwind_current(store, plan)?;
        if plan.applied.is_empty() {
            self.advance();
            return Ok(Some(RuntimeAction::Acknowledge(plan.acknowledge)));
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
            let first = self.first_unchecked_or(plan.applied[0]);
            let acknowledged = self.record_unchecked_descendants(store, first, plan.acknowledge)?;
            self.state = RuntimeState::Alerting;
            self.advance();
            return Ok(Some(RuntimeAction::Acknowledge(acknowledged)));
        }
        Ok(None)
    }

    /// Apply retry, gap, or finding policy for an authentication failure.
    fn handle_authentication_failure(
        &mut self,
        store: &Persistence,
        plan: &NotificationPlan,
        work: &BlockWork,
        failure: Failure,
        now: Instant,
    ) -> Result<RuntimeAction, PersistenceError> {
        match failure.class {
            FailureClass::BoundedRetry | FailureClass::TransientRetry => {
                let retry = self.retry.get_or_insert(RetryState {
                    attempts: 0,
                    started: now,
                    next_attempt: now,
                });
                retry.attempts += 1;
                self.state = RuntimeState::Retrying;
                if !self.budget.exhausted(retry.attempts, retry.started, now) {
                    retry.next_attempt = now + Duration::from_millis(25);
                    return Ok(RuntimeAction::RetryAt(retry.next_attempt));
                }
                let mut last = work.suffix_end;
                let mut previous = plan.acknowledge;
                for queued in &self.queue {
                    let plan = &queued.plan;
                    if !plan.reverted.is_empty()
                        || plan.applied.is_empty()
                        || plan.ancestor != previous
                    {
                        self.block(store, CheckerBlockedReason::InvalidNotificationSequence)?;
                        return Ok(RuntimeAction::Blocked);
                    }
                    last = plan.acknowledge;
                    previous = plan.acknowledge;
                }
                let reason = match &self.snapshot.meta.coverage {
                    Coverage::Gap { reason, .. } => reason.clone(),
                    Coverage::Complete => failure.gap_reason(),
                };
                self.snapshot = store.record_gap(&self.snapshot, work.coordinate, last, reason)?;
                self.state = RuntimeState::Disabled;
                self.current = None;
                self.queue.clear();
                Ok(RuntimeAction::AcknowledgeAndDisable(last))
            }
            FailureClass::AuthenticatedDivergence => {
                let typed = failure
                    .finding
                    .ok_or_else(|| PersistenceError::Invalid("divergence has no finding".into()))?;
                let (key, finding) = make_finding(
                    work.coordinate,
                    self.snapshot.meta.verified_zone_tip,
                    None,
                    *typed,
                    failure.message,
                )?;
                self.snapshot =
                    store.record_divergence(&self.snapshot, key, finding, work.suffix_end)?;
                self.state = RuntimeState::Alerting;
                self.advance();
                Ok(RuntimeAction::Acknowledge(
                    self.snapshot.meta.acknowledged_zone_tip,
                ))
            }
            FailureClass::ImmediateTerminal => {
                self.block(store, CheckerBlockedReason::InvalidAuthenticatedData)?;
                Ok(RuntimeAction::Blocked)
            }
        }
    }

    /// Commit one authenticated block and derive the resulting ExEx action.
    fn commit_authenticated_block(
        &mut self,
        store: &Persistence,
        identity: Identity,
        work: BlockWork,
        block: AuthenticatedBlock,
    ) -> Result<RuntimeAction, PersistenceError> {
        self.retry = None;
        if block.zone != work.coordinate {
            self.snapshot = store.record_gap(
                &self.snapshot,
                work.coordinate,
                work.suffix_end,
                CoverageGapReason::MissingReceipts,
            )?;
            self.state = RuntimeState::Disabled;
            self.current = None;
            self.queue.clear();
            return Ok(RuntimeAction::AcknowledgeAndDisable(work.suffix_end));
        }
        self.process_block(
            store,
            identity,
            &block,
            work.suffix_end,
            work.gap_next,
            work.is_last,
        )?;
        if self.state == RuntimeState::Blocked {
            return Ok(RuntimeAction::Blocked);
        }
        let ready = self.snapshot.meta.acknowledged_zone_tip;
        if self.state == RuntimeState::Disabled {
            self.current = None;
            self.queue.clear();
            return Ok(RuntimeAction::AcknowledgeAndDisable(ready));
        }
        if work.is_last || self.state == RuntimeState::Alerting {
            self.advance();
            Ok(RuntimeAction::Acknowledge(ready))
        } else {
            Ok(RuntimeAction::None)
        }
    }

    /// Select the next runtime action without acquiring external data.
    pub(crate) fn next_action(
        &mut self,
        store: &Persistence,
        now: Instant,
    ) -> Result<RuntimeAction, PersistenceError> {
        if self.state == RuntimeState::Disabled {
            return Ok(RuntimeAction::AwaitNotification);
        }
        if self.state == RuntimeState::Blocked {
            return Ok(RuntimeAction::Blocked);
        }
        let Some(current) = self.current.as_ref() else {
            return Ok(RuntimeAction::None);
        };
        let plan = current.plan.clone();
        if let Some(action) = self.reconcile(store, &plan)? {
            return Ok(action);
        }
        let work = match self.select_work(&plan) {
            WorkSelection::Work(work) => work,
            WorkSelection::Complete => {
                self.advance();
                return Ok(RuntimeAction::Acknowledge(plan.acknowledge));
            }
            WorkSelection::Terminal => {
                self.block(store, CheckerBlockedReason::InvalidNotificationSequence)?;
                return Ok(RuntimeAction::Blocked);
            }
            WorkSelection::AwaitNotification => return Ok(RuntimeAction::AwaitNotification),
        };
        if let Some(retry) = &self.retry
            && now < retry.next_attempt
        {
            return Ok(RuntimeAction::RetryAt(retry.next_attempt));
        }
        Ok(RuntimeAction::Authenticate(AuthenticationRequest { work }))
    }

    /// Apply the ExEx authentication result for a previously requested block.
    pub(crate) fn complete_authentication(
        &mut self,
        store: &Persistence,
        identity: Identity,
        request: AuthenticationRequest,
        result: Result<AuthenticatedBlock, Failure>,
        now: Instant,
    ) -> Result<RuntimeAction, PersistenceError> {
        let plan = self
            .current
            .as_ref()
            .ok_or_else(|| {
                PersistenceError::Invalid("authentication has no current notification".into())
            })?
            .plan
            .clone();
        match result {
            Ok(block) => self.commit_authenticated_block(store, identity, request.work, block),
            Err(failure) => {
                self.handle_authentication_failure(store, &plan, &request.work, failure, now)
            }
        }
    }

    #[cfg(test)]
    /// Authenticate synchronously for runtime unit tests.
    pub(crate) fn poll(
        &mut self,
        store: &Persistence,
        identity: Identity,
        mut authenticate: impl FnMut(&N, usize, &State) -> Result<AuthenticatedBlock, Failure>,
        now: Instant,
    ) -> Result<RuntimeAction, PersistenceError> {
        match self.next_action(store, now)? {
            RuntimeAction::Authenticate(request) => {
                let notification = &self
                    .current
                    .as_ref()
                    .expect("current notification retained")
                    .notification;
                let result = authenticate(notification, request.index(), &self.snapshot.state);
                self.complete_authentication(store, identity, request, result, now)
            }
            action => Ok(action),
        }
    }

    /// Evaluate and durably commit one authenticated block or record its terminal outcome.
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
            let first = self.first_unchecked_or(block.zone);
            self.record_unchecked_descendants(store, first, suffix_end)?;
            self.state = RuntimeState::Alerting;
            return Ok(());
        }

        let candidate = match verification::verify_block(identity, &self.snapshot.state, block) {
            Ok(candidate) => candidate,
            Err(failure) if failure.class == FailureClass::AuthenticatedDivergence => {
                return self.record_divergence(store, block, suffix_end, failure);
            }
            Err(failure) if failure.class == FailureClass::ImmediateTerminal => {
                self.block(store, CheckerBlockedReason::InvalidAuthenticatedData)?;
                return Ok(());
            }
            Err(failure) => {
                self.snapshot = store.record_gap(
                    &self.snapshot,
                    block.zone,
                    suffix_end,
                    failure.gap_reason(),
                )?;
                self.state = RuntimeState::Disabled;
                return Ok(());
            }
        };

        let (coverage, acknowledged) = match &self.snapshot.meta.coverage {
            Coverage::Gap {
                first_unchecked,
                acknowledged_through,
                reason,
            } if first_unchecked.number == block.zone.number => {
                if block.zone.number == acknowledged_through.number {
                    (Coverage::Complete, block.zone)
                } else {
                    let next_first = gap_next.ok_or_else(|| {
                        PersistenceError::Invalid("durable gap has no next coordinate".into())
                    })?;
                    let next_through = if next_first.number == acknowledged_through.number {
                        next_first
                    } else {
                        *acknowledged_through
                    };
                    (
                        Coverage::Gap {
                            first_unchecked: next_first,
                            acknowledged_through: next_through,
                            reason: reason.clone(),
                        },
                        next_through,
                    )
                }
            }
            Coverage::Complete => (Coverage::Complete, block.zone),
            _ => {
                return Err(PersistenceError::Invalid(
                    "applied block does not begin at durable gap".into(),
                ));
            }
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
        logging::verified(block);
        if is_last {
            self.state = RuntimeState::Healthy;
        }
        Ok(())
    }

    /// Persist an authenticated finding and mark its unchecked descendant suffix.
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
        let logged_finding = finding.clone();
        self.snapshot = store.record_divergence(&self.snapshot, key, finding, suffix_end)?;
        logging::divergence(block, &logged_finding);
        self.state = RuntimeState::Alerting;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
