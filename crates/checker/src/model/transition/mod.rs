//! Pure read-through transition overlay and typed logical delta.

mod deposits;
mod error;
mod finalization;
mod portal;
mod processing;
mod refunds;
mod submission;
mod zone;

use std::{
    cmp::Ordering,
    collections::{BTreeMap, btree_map},
    iter::Peekable,
    ops::RangeBounds,
};

use alloy_primitives::{Address, U256};

use super::{
    constants::WITHDRAWAL_QUEUE_CAPACITY,
    encoding::DepositQueueMember,
    input::{ImportedTempoBlockInput, TokenEnable, ZoneBlockInput, ZoneDepositPrefixInput},
    output::{
        ExpectedImportedTempoBlock, ExpectedOutputs, ExpectedPostZoneState, ExpectedZoneBlock,
        ExpectedZoneDepositPrefix,
    },
    ownership::{
        BatchId, BatchOwner, DepositId, DepositOwner, FallbackId, FallbackOwner, InboxRefundId,
        InboxRefundOwner, PortalRefundId, PortalRefundOwner, RefundAccount, WithdrawalId,
        WithdrawalOwner,
    },
    state::{CreatedPortalState, ModelState, PortalLifecycle, TokenState, ZoneState},
};

pub(crate) use error::{
    BlockTransitionMismatch, DepositKind, DepositOutcomeKind, DepositTransitionMismatch,
    ModelError, WithdrawalOriginKind, WithdrawalProcessingOutcomeKind,
};

/// Typed final mutations for one candidate block. Owner and sparse aggregate
/// maps use `None` for deletion, matching the eventual persistent key/value
/// delta without a generic key/value registry.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LogicalDelta {
    portal: Option<PortalLifecycle>,
    zone: Option<ZoneState>,
    tokens: BTreeMap<Address, TokenState>,
    pending_deposits: BTreeMap<DepositId, Option<DepositOwner>>,
    withdrawals: BTreeMap<WithdrawalId, Option<WithdrawalOwner>>,
    batches: BTreeMap<BatchId, Option<BatchOwner>>,
    fallback_owners: BTreeMap<FallbackId, Option<FallbackOwner>>,
    portal_refunds: BTreeMap<PortalRefundId, Option<PortalRefundOwner>>,
    portal_refund_totals: BTreeMap<RefundAccount, Option<u128>>,
    inbox_refunds: BTreeMap<InboxRefundId, Option<InboxRefundOwner>>,
    inbox_refund_totals: BTreeMap<RefundAccount, Option<u128>>,
}

impl LogicalDelta {
    fn empty() -> Self {
        Self {
            portal: None,
            zone: None,
            tokens: BTreeMap::new(),
            pending_deposits: BTreeMap::new(),
            withdrawals: BTreeMap::new(),
            batches: BTreeMap::new(),
            fallback_owners: BTreeMap::new(),
            portal_refunds: BTreeMap::new(),
            portal_refund_totals: BTreeMap::new(),
            inbox_refunds: BTreeMap::new(),
            inbox_refund_totals: BTreeMap::new(),
        }
    }
}

/// A concrete read-through overlay. Taking `self` on each public transition
/// makes a rejected candidate unobservable while leaving the parent untouched.
pub(crate) struct ModelTransition<'a> {
    parent: &'a ModelState,
    delta: LogicalDelta,
}

/// Post-Tempo candidate retained for the collateral cut. This phase cannot
/// release a committable delta until its complete matching Zone block applies.
pub(crate) struct ImportedTempoTransition<'a> {
    candidate: ModelTransition<'a>,
    tempo_block_number: u64,
    token_enables: Vec<TokenEnable>,
    expected: ExpectedImportedTempoBlock,
}

/// Deterministic merged view of parent and candidate token rows. Candidate
/// values replace the same parent key without cloning the full token set.
pub(crate) struct TokenViewIter<'a> {
    parent: Peekable<btree_map::Iter<'a, Address, TokenState>>,
    candidate: Peekable<btree_map::Iter<'a, Address, TokenState>>,
}

impl<'a> TokenViewIter<'a> {
    fn new(parent: &'a ModelState, delta: &'a LogicalDelta) -> Self {
        Self {
            parent: parent.tokens.iter().peekable(),
            candidate: delta.tokens.iter().peekable(),
        }
    }
}

impl<'a> Iterator for TokenViewIter<'a> {
    type Item = (Address, &'a TokenState);

    fn next(&mut self) -> Option<Self::Item> {
        match (self.parent.peek(), self.candidate.peek()) {
            (Some((parent_key, _)), Some((candidate_key, _))) => {
                match parent_key.cmp(candidate_key) {
                    Ordering::Less => self.parent.next().map(|(key, value)| (*key, value)),
                    Ordering::Equal => {
                        self.parent.next();
                        self.candidate.next().map(|(key, value)| (*key, value))
                    }
                    Ordering::Greater => self.candidate.next().map(|(key, value)| (*key, value)),
                }
            }
            (Some(_), None) => self.parent.next().map(|(key, value)| (*key, value)),
            (None, Some(_)) => self.candidate.next().map(|(key, value)| (*key, value)),
            (None, None) => None,
        }
    }
}

/// Lazy merged view of one ordered owner-map range. Candidate replacements
/// and tombstones take precedence without materializing the overlay.
struct OwnerOverlay<'a, K, V> {
    parent: Peekable<btree_map::Range<'a, K, V>>,
    candidate: Peekable<btree_map::Range<'a, K, Option<V>>>,
}

impl<'a, K: Ord, V> OwnerOverlay<'a, K, V> {
    fn new<R>(parent: &'a BTreeMap<K, V>, candidate: &'a BTreeMap<K, Option<V>>, range: R) -> Self
    where
        R: RangeBounds<K> + Clone,
    {
        Self {
            parent: parent.range(range.clone()).peekable(),
            candidate: candidate.range(range).peekable(),
        }
    }
}

impl<'a, K: Ord + Copy, V> Iterator for OwnerOverlay<'a, K, V> {
    type Item = (K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match (self.parent.peek(), self.candidate.peek()) {
                (Some((parent_key, _)), Some((candidate_key, _))) => {
                    match parent_key.cmp(candidate_key) {
                        Ordering::Less => {
                            return self.parent.next().map(|(key, value)| (*key, value));
                        }
                        Ordering::Equal => {
                            self.parent.next();
                            let (key, value) =
                                self.candidate.next().expect("peeked candidate owner");
                            if let Some(value) = value {
                                return Some((*key, value));
                            }
                        }
                        Ordering::Greater => {
                            let (key, value) =
                                self.candidate.next().expect("peeked candidate owner");
                            if let Some(value) = value {
                                return Some((*key, value));
                            }
                        }
                    }
                }
                (Some(_), None) => {
                    return self.parent.next().map(|(key, value)| (*key, value));
                }
                (None, Some(_)) => {
                    let (key, value) = self.candidate.next().expect("peeked candidate owner");
                    if let Some(value) = value {
                        return Some((*key, value));
                    }
                }
                (None, None) => return None,
            }
        }
    }
}

/// Complete Zone-block candidate. It retains its exact parent cut throughout
/// transition and comparison; callers release its sparse update only at the
/// checker commit boundary.
pub(crate) struct CompletedTransition<'a> {
    parent: &'a ModelState,
    delta: LogicalDelta,
    expected: ExpectedOutputs,
}

/// Sparse mutations released only after a complete candidate transition.
///
/// The checker applies this value to the still-current parent only after every
/// external comparison succeeds. Keeping the update typed avoids cloning the
/// authoritative lifecycle maps on every verified block.
#[must_use = "a candidate update must be applied exactly once to its still-current parent"]
pub(crate) struct ModelStateUpdate {
    delta: LogicalDelta,
}

impl ModelStateUpdate {
    /// Apply to the exact parent from which this update was projected.
    ///
    /// Rust module visibility cannot grant this sibling-module method only to
    /// `check::pipeline`; the private `CandidateCommit` there is the sole
    /// production owner. Persistence must add an explicit tip/generation guard
    /// before it allows an update to outlive that immediate commit boundary.
    pub(crate) fn apply_to_current_parent(self, state: &mut ModelState) {
        apply_delta(state, self.delta);
    }
}

impl<'a> ModelTransition<'a> {
    pub(crate) fn new(parent: &'a ModelState) -> Self {
        Self {
            parent,
            delta: LogicalDelta::empty(),
        }
    }

    pub(crate) fn apply_imported_tempo_block(
        mut self,
        input: &ImportedTempoBlockInput,
    ) -> Result<ImportedTempoTransition<'a>, ModelError> {
        let mut expected = ExpectedImportedTempoBlock::default();
        let mut token_enables = Vec::new();
        for operation in input.operations() {
            portal::apply_operation(
                &mut self,
                operation,
                input.base_fee(),
                &mut token_enables,
                &mut expected,
            )?;
        }
        Ok(ImportedTempoTransition {
            candidate: self,
            tempo_block_number: input.tempo_block_number(),
            token_enables,
            expected,
        })
    }

    fn portal(&self) -> &PortalLifecycle {
        self.delta.portal.as_ref().unwrap_or(&self.parent.portal)
    }

    fn set_portal(&mut self, portal: PortalLifecycle) {
        self.delta.portal = Some(portal);
    }

    fn zone(&self) -> &ZoneState {
        self.delta.zone.as_ref().unwrap_or(&self.parent.zone)
    }

    fn set_zone(&mut self, zone: ZoneState) {
        self.delta.zone = Some(zone);
    }

    fn token(&self, token: Address) -> Option<&TokenState> {
        self.delta
            .tokens
            .get(&token)
            .or_else(|| self.parent.tokens.get(&token))
    }

    fn set_token(&mut self, token: Address, state: TokenState) {
        self.delta.tokens.insert(token, state);
    }

    fn pending_deposit(&self, id: DepositId) -> Option<&DepositOwner> {
        match self.delta.pending_deposits.get(&id) {
            Some(Some(owner)) => Some(owner),
            Some(None) => None,
            None => self.parent.pending_deposits.get(&id),
        }
    }

    fn set_pending_deposit(&mut self, id: DepositId, owner: Option<DepositOwner>) {
        self.delta.pending_deposits.insert(id, owner);
    }

    fn withdrawal(&self, id: WithdrawalId) -> Option<&WithdrawalOwner> {
        match self.delta.withdrawals.get(&id) {
            Some(Some(owner)) => Some(owner),
            Some(None) => None,
            None => self.parent.withdrawals.get(&id),
        }
    }

    fn set_withdrawal(&mut self, id: WithdrawalId, owner: Option<WithdrawalOwner>) {
        self.delta.withdrawals.insert(id, owner);
    }

    fn batch(&self, id: BatchId) -> Option<&BatchOwner> {
        match self.delta.batches.get(&id) {
            Some(Some(owner)) => Some(owner),
            Some(None) => None,
            None => self.parent.batches.get(&id),
        }
    }

    fn set_batch(&mut self, id: BatchId, owner: Option<BatchOwner>) {
        self.delta.batches.insert(id, owner);
    }

    /// Smallest open batch in the read-through candidate. Strict submission
    /// order makes this the queue head whenever the Portal ring is non-empty.
    fn first_batch(&self) -> Option<(BatchId, BatchOwner)> {
        OwnerOverlay::new(&self.parent.batches, &self.delta.batches, ..)
            .next()
            .map(|(id, owner)| (id, owner.clone()))
    }

    fn fallback_owner(&self, id: FallbackId) -> Option<&FallbackOwner> {
        match self.delta.fallback_owners.get(&id) {
            Some(Some(owner)) => Some(owner),
            Some(None) => None,
            None => self.parent.fallback_owners.get(&id),
        }
    }

    fn set_fallback_owner(&mut self, id: FallbackId, owner: Option<FallbackOwner>) {
        self.delta.fallback_owners.insert(id, owner);
    }

    fn portal_refund(&self, id: PortalRefundId) -> Option<&PortalRefundOwner> {
        match self.delta.portal_refunds.get(&id) {
            Some(Some(owner)) => Some(owner),
            Some(None) => None,
            None => self.parent.portal_refunds.get(&id),
        }
    }

    fn set_portal_refund(&mut self, id: PortalRefundId, owner: Option<PortalRefundOwner>) {
        self.delta.portal_refunds.insert(id, owner);
    }

    fn portal_refund_total(&self, account: RefundAccount) -> u128 {
        match self.delta.portal_refund_totals.get(&account) {
            Some(Some(total)) => *total,
            Some(None) => 0,
            None => self.parent.portal_refund_total(account),
        }
    }

    fn set_portal_refund_total(&mut self, account: RefundAccount, total: u128) {
        self.delta
            .portal_refund_totals
            .insert(account, (total != 0).then_some(total));
    }

    fn inbox_refund(&self, id: InboxRefundId) -> Option<&InboxRefundOwner> {
        match self.delta.inbox_refunds.get(&id) {
            Some(Some(owner)) => Some(owner),
            Some(None) => None,
            None => self.parent.inbox_refunds.get(&id),
        }
    }

    fn set_inbox_refund(&mut self, id: InboxRefundId, owner: Option<InboxRefundOwner>) {
        self.delta.inbox_refunds.insert(id, owner);
    }

    fn inbox_refund_total(&self, account: RefundAccount) -> u128 {
        match self.delta.inbox_refund_totals.get(&account) {
            Some(Some(total)) => *total,
            Some(None) => 0,
            None => self.parent.inbox_refund_total(account),
        }
    }

    fn set_inbox_refund_total(&mut self, account: RefundAccount, total: u128) {
        self.delta
            .inbox_refund_totals
            .insert(account, (total != 0).then_some(total));
    }
}

impl<'a> ImportedTempoTransition<'a> {
    /// Independently derived Portal outputs available before any external
    /// collateral call or Zone transition is attempted.
    pub(crate) const fn expected(&self) -> &ExpectedImportedTempoBlock {
        &self.expected
    }

    /// Read-only post-L1/pre-Zone cut used by the collateral check in Goal 5.
    pub(crate) fn created_portal(&self) -> Option<&CreatedPortalState> {
        self.candidate.portal().created()
    }

    /// Read-only token view at the post-L1/pre-Zone collateral cut.
    pub(crate) fn token(&self, token: Address) -> Option<&TokenState> {
        self.candidate.token(token)
    }

    /// All Portal-enabled tokens at the collateral cut, in address order.
    pub(crate) fn tokens(&self) -> TokenViewIter<'_> {
        TokenViewIter::new(self.candidate.parent, &self.candidate.delta)
    }

    pub(crate) fn apply_zone_block(
        mut self,
        input: &ZoneBlockInput,
    ) -> Result<CompletedTransition<'a>, ModelError> {
        let zone_deposit_prefix = self.apply_deposit_stage(input.advance())?;
        let zone_operations = zone::apply_operations(&mut self.candidate, input.operations())?;
        let finalized_batch = match input.finalization() {
            Some(finalization) => Some(finalization::apply(
                &mut self.candidate,
                input.context(),
                self.tempo_block_number,
                finalization,
            )?),
            None => None,
        };
        Ok(CompletedTransition {
            parent: self.candidate.parent,
            delta: self.candidate.delta,
            expected: ExpectedOutputs::new(
                self.expected,
                zone_deposit_prefix,
                ExpectedZoneBlock::new(zone_operations, finalized_batch),
            ),
        })
    }

    fn apply_deposit_stage(
        &mut self,
        input: &ZoneDepositPrefixInput,
    ) -> Result<ExpectedZoneDepositPrefix, ModelError> {
        if self.token_enables.len() != input.enabled_tokens().len() {
            return Err(ModelError::ZoneTokenEnableCountMismatch {
                expected: self.token_enables.len(),
                actual: input.enabled_tokens().len(),
            });
        }
        for (index, (expected, actual)) in self
            .token_enables
            .iter()
            .zip(input.enabled_tokens())
            .enumerate()
        {
            if expected != actual {
                return Err(ModelError::ZoneTokenEnableMismatch {
                    index,
                    expected: Box::new(expected.clone()),
                    actual: Box::new(actual.clone()),
                });
            }
        }

        deposits::apply_zone_prefix(&mut self.candidate, input)
    }
}

impl CompletedTransition<'_> {
    pub(crate) const fn expected(&self) -> &ExpectedOutputs {
        &self.expected
    }

    /// Zone state at the post-Zone candidate cut used by fixed commitment
    /// comparisons.
    pub(crate) fn zone(&self) -> &ZoneState {
        self.delta.zone.as_ref().unwrap_or(&self.parent.zone)
    }

    pub(crate) fn expected_post_zone_state(
        &self,
        tempo_block_hash: alloy_primitives::B256,
        tempo_block_number: u64,
    ) -> ExpectedPostZoneState {
        let zone = self.zone();
        ExpectedPostZoneState::new(
            tempo_block_hash,
            tempo_block_number,
            zone.processed_deposit_cursor(),
            zone.last_batch(),
        )
    }

    /// Token state at the post-Zone candidate cut used by Goal 5 supply checks.
    pub(crate) fn token(&self, token: Address) -> Option<&TokenState> {
        self.delta
            .tokens
            .get(&token)
            .or_else(|| self.parent.tokens.get(&token))
    }

    /// All Portal-enabled tokens at the post-Zone cut, in address order.
    pub(crate) fn tokens(&self) -> TokenViewIter<'_> {
        TokenViewIter::new(self.parent, &self.delta)
    }

    /// Release the sparse candidate only after callers finish all comparisons.
    pub(crate) fn into_state_update(self) -> ModelStateUpdate {
        ModelStateUpdate { delta: self.delta }
    }

    #[cfg(test)]
    pub(crate) fn materialize_for_test(self) -> (ModelState, ExpectedOutputs) {
        let mut state = self.parent.clone();
        apply_delta(&mut state, self.delta);
        (state, self.expected)
    }
}

fn apply_delta(state: &mut ModelState, delta: LogicalDelta) {
    if let Some(portal) = delta.portal {
        state.portal = portal;
    }
    if let Some(zone) = delta.zone {
        state.zone = zone;
    }
    state.tokens.extend(delta.tokens);
    apply_sparse_map_delta(&mut state.pending_deposits, delta.pending_deposits);
    apply_sparse_map_delta(&mut state.withdrawals, delta.withdrawals);
    apply_sparse_map_delta(&mut state.batches, delta.batches);
    apply_sparse_map_delta(&mut state.fallback_owners, delta.fallback_owners);
    apply_sparse_map_delta(&mut state.portal_refunds, delta.portal_refunds);
    apply_sparse_map_delta(&mut state.portal_refund_totals, delta.portal_refund_totals);
    apply_sparse_map_delta(&mut state.inbox_refunds, delta.inbox_refunds);
    apply_sparse_map_delta(&mut state.inbox_refund_totals, delta.inbox_refund_totals);
}

fn apply_sparse_map_delta<K: Ord, V>(state: &mut BTreeMap<K, V>, delta: BTreeMap<K, Option<V>>) {
    for (key, value) in delta {
        match value {
            Some(value) => {
                state.insert(key, value);
            }
            None => {
                state.remove(&key);
            }
        }
    }
}

fn queue_member(owner: &DepositOwner) -> DepositQueueMember {
    match owner {
        DepositOwner::PendingOrdinary { preimage } => {
            DepositQueueMember::Ordinary(preimage.clone())
        }
        DepositOwner::PendingWithdrawalBounceBack { preimage, .. } => {
            DepositQueueMember::WithdrawalBounceBack(*preimage)
        }
    }
}

fn require_zone_token(
    candidate: &ModelTransition<'_>,
    token: Address,
) -> Result<TokenState, ModelError> {
    let state = candidate
        .token(token)
        .cloned()
        .ok_or(ModelError::TokenNotPortalEnabled { token })?;
    if !state.is_zone_enabled() {
        return Err(ModelError::TokenNotZoneEnabled { token });
    }
    Ok(state)
}

/// Validate the raw logical Portal ring counters and return their occupied
/// length. Physical-slot modulo arithmetic never belongs in model state.
fn validated_portal_queue_len(head: U256, tail: U256) -> Result<U256, ModelError> {
    let len = tail
        .checked_sub(head)
        .ok_or(ModelError::InvalidPortalWithdrawalQueueProgress { head, tail })?;
    if len > WITHDRAWAL_QUEUE_CAPACITY {
        return Err(ModelError::InvalidPortalWithdrawalQueueProgress { head, tail });
    }
    Ok(len)
}

#[cfg(test)]
mod tests;
