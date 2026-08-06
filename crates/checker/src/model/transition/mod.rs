//! Pure read-through transition overlay and typed logical delta.

mod deposits;
mod finalization;
mod portal;
mod zone;

use std::{
    cmp::Ordering,
    collections::{BTreeMap, btree_map},
    iter::Peekable,
};

use alloy_primitives::Address;

use super::{
    accounting::AccountingError,
    encoding::{DepositQueueMember, WithdrawalDataError},
    fees::FeeError,
    input::{ImportedTempoBlockInput, TokenEnable, ZoneBlockInput, ZoneDepositPrefixInput},
    output::{
        ExpectedImportedTempoBlock, ExpectedOutputs, ExpectedZoneBlock, ExpectedZoneDepositPrefix,
    },
    ownership::{
        BatchId, BatchOwner, BatchStateError, DepositId, DepositOwner, FallbackId, FallbackOwner,
        InboxRefundId, InboxRefundOwner, WithdrawalId, WithdrawalOwner,
    },
    state::{CreatedPortalState, ModelState, PortalLifecycle, TokenState, ZoneState},
};

/// Deposit origin expected by the oldest pending owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DepositKind {
    Ordinary,
    WithdrawalBounceBack,
}

/// Authenticated implementation branch selected for one consumed deposit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DepositOutcomeKind {
    OrdinaryMinted,
    OrdinaryFailed,
    WithdrawalBounceBackMinted,
    WithdrawalBounceBackPending,
}

/// Fail-closed logical transition errors. Acquisition and event decoding
/// failures remain in the observation adapter and never enter this enum.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ModelError {
    #[error("the Portal has not been authenticated as created")]
    PortalNotCreated,
    #[error("the Portal creation transition was already applied")]
    PortalAlreadyCreated,
    #[error("Portal creation identity mismatch: expected {expected:?}, got {actual:?}")]
    PortalIdentityMismatch {
        expected: super::state::PortalIdentity,
        actual: super::state::PortalIdentity,
    },
    #[error("configured Portal address {actual} does not match derived address {expected}")]
    PortalAddressMismatch { expected: Address, actual: Address },
    #[error("constructor TokenEnabled token mismatch: expected {expected}, got {actual}")]
    InitialTokenMismatch { expected: Address, actual: Address },
    #[error("token {token} is already enabled on the Portal")]
    TokenAlreadyEnabled { token: Address },
    #[error("token {token} is not enabled on the Portal")]
    TokenNotPortalEnabled { token: Address },
    #[error("token {token} is not enabled on the Zone")]
    TokenNotZoneEnabled { token: Address },
    #[error("ordinary deposit refund recipient is zero")]
    ZeroTempoRefundRecipient,
    #[error("Zone token enable count mismatch: expected {expected}, got {actual}")]
    ZoneTokenEnableCountMismatch { expected: usize, actual: usize },
    #[error(
        "Zone token enable at position {index} does not match the imported Portal event: expected {expected:?}, got {actual:?}"
    )]
    ZoneTokenEnableMismatch {
        index: usize,
        expected: Box<TokenEnable>,
        actual: Box<TokenEnable>,
    },
    #[error("Portal deposit number overflow")]
    PortalDepositNumberOverflow,
    #[error("Portal deposit {number} already has an open owner")]
    DepositOwnerCollision { number: u64 },
    #[error("withdrawal bounce-back nonce {fallback_nonce} has no matching fallback owner")]
    FallbackOwnerMissing { fallback_nonce: u64 },
    #[error("withdrawal bounce-back nonce {fallback_nonce} does not match its fallback owner")]
    FallbackOwnerMismatch { fallback_nonce: u64 },
    #[error("withdrawal {withdrawal_index} already has an open bounce-back deposit")]
    WithdrawalBounceBackAlreadyPending { withdrawal_index: u64 },
    #[error("deposit prefix/outcome count mismatch: {deposits} deposits, {outcomes} outcomes")]
    DepositOutcomeCountMismatch { deposits: usize, outcomes: usize },
    #[error("processed deposit number overflow")]
    ProcessedDepositNumberOverflow,
    #[error("pending deposit {number} does not exist")]
    PendingDepositMissing { number: u64 },
    #[error("deposit prefix member {number} does not match the oldest pending owner")]
    DepositPrefixMismatch { number: u64 },
    #[error("deposit {number} expected {expected:?} outcome, got authenticated {actual:?} outcome")]
    DepositOutcomeKindMismatch {
        number: u64,
        expected: DepositKind,
        actual: DepositOutcomeKind,
    },
    #[error("withdrawal index overflow")]
    WithdrawalIndexOverflow,
    #[error("withdrawal index {withdrawal_index} already has an open owner")]
    WithdrawalOwnerCollision { withdrawal_index: u64 },
    #[error("withdrawal cap {limit} was exceeded in this Zone block")]
    WithdrawalBlockCapExceeded { limit: u32 },
    #[error("fallback nonce overflow")]
    FallbackNonceOverflow,
    #[error("fallback nonce {fallback_nonce} already has an open owner")]
    FallbackOwnerCollision { fallback_nonce: u64 },
    #[error("finalization block number mismatch: expected {expected}, got {actual}")]
    FinalizationBlockNumberMismatch { expected: u64, actual: u64 },
    #[error("finalization count mismatch: expected {expected}, got {actual}")]
    FinalizationCountMismatch { expected: u64, actual: usize },
    #[error(
        "finalization encrypted-sender count mismatch: declared {declared}, got {actual} entries"
    )]
    FinalizationSenderCountMismatch { declared: usize, actual: usize },
    #[error("current batch withdrawal range is invalid: first {first}, next {next}")]
    InvalidBatchWithdrawalRange { first: u64, next: u64 },
    #[error("withdrawal {withdrawal_index} is missing from the current batch range")]
    WithdrawalOwnerMissing { withdrawal_index: u64 },
    #[error("withdrawal {withdrawal_index} was already finalized")]
    WithdrawalAlreadyFinalized { withdrawal_index: u64 },
    #[error("withdrawal batch index overflow")]
    WithdrawalBatchIndexOverflow,
    #[error("withdrawal batch index {withdrawal_batch_index} already has an open owner")]
    BatchOwnerCollision { withdrawal_batch_index: u64 },
    #[error("Inbox refund credit already exists for withdrawal {withdrawal_index}")]
    InboxRefundCollision { withdrawal_index: u64 },
    #[error("withdrawal {withdrawal_index} bounce-back outcome recipient is zero")]
    ZeroBounceBackRecipient { withdrawal_index: u64 },
    #[error(transparent)]
    Accounting(#[from] AccountingError),
    #[error(transparent)]
    Fee(#[from] FeeError),
    #[error(transparent)]
    WithdrawalData(#[from] WithdrawalDataError),
    #[error(transparent)]
    BatchState(#[from] BatchStateError),
}

/// Typed final mutations for one candidate block. Owner maps use `None` for a
/// deletion, matching the eventual persistent key/value delta without a
/// generic key/value registry.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LogicalDelta {
    portal: Option<PortalLifecycle>,
    zone: Option<ZoneState>,
    tokens: BTreeMap<Address, TokenState>,
    pending_deposits: BTreeMap<DepositId, Option<DepositOwner>>,
    withdrawals: BTreeMap<WithdrawalId, Option<WithdrawalOwner>>,
    batches: BTreeMap<BatchId, Option<BatchOwner>>,
    fallback_owners: BTreeMap<FallbackId, Option<FallbackOwner>>,
    inbox_refunds: BTreeMap<InboxRefundId, Option<InboxRefundOwner>>,
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
            inbox_refunds: BTreeMap::new(),
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

/// Complete Zone-block commit capsule. It retains its exact parent cut so the
/// mutation cannot be detached and applied to a different or newer state.
pub(crate) struct CompletedTransition<'a> {
    parent: &'a ModelState,
    delta: LogicalDelta,
    expected: ExpectedOutputs,
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
            portal::apply_operation(&mut self, operation, &mut token_enables, &mut expected)?;
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
}

impl<'a> ImportedTempoTransition<'a> {
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
        let user_withdrawals = zone::apply_operations(&mut self.candidate, input.operations())?;
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
                ExpectedZoneBlock::new(user_withdrawals, finalized_batch),
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

    #[cfg(test)]
    pub(crate) fn materialize_for_test(self) -> (ModelState, ExpectedOutputs) {
        let mut state = self.parent.clone();
        apply_delta_for_test(&mut state, self.delta);
        (state, self.expected)
    }
}

#[cfg(test)]
fn apply_delta_for_test(state: &mut ModelState, delta: LogicalDelta) {
    if let Some(portal) = delta.portal {
        state.portal = portal;
    }
    if let Some(zone) = delta.zone {
        state.zone = zone;
    }
    state.tokens.extend(delta.tokens);
    apply_owner_delta_for_test(&mut state.pending_deposits, delta.pending_deposits);
    apply_owner_delta_for_test(&mut state.withdrawals, delta.withdrawals);
    apply_owner_delta_for_test(&mut state.batches, delta.batches);
    apply_owner_delta_for_test(&mut state.fallback_owners, delta.fallback_owners);
    apply_owner_delta_for_test(&mut state.inbox_refunds, delta.inbox_refunds);
}

#[cfg(test)]
fn apply_owner_delta_for_test<K: Ord, V>(
    state: &mut BTreeMap<K, V>,
    delta: BTreeMap<K, Option<V>>,
) {
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

#[cfg(test)]
mod tests;
