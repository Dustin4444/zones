//! Deterministic imported-Tempo and Zone state transitions.

use std::{collections::BTreeMap, num::NonZeroU64, ops::Bound};

use alloy_primitives::{Address, U256};

use crate::kernel::{
    derivation::{
        NO_QUEUE_INDEX, WITHDRAWAL_TERMINATOR, bounceback_deposit_hash, bounceback_fee,
        failed_deposit_sender_tag, ordinary_deposit_hash, portal_address, sender_tag,
        withdrawal_fee, withdrawal_hash, withdrawal_queue_hash,
    },
    effects::{Effect, ExpectedState},
    facts::{
        BatchSubmission, BounceBackDeposit, Deposit, DepositOutcome, Finalization, ImportedFacts,
        ImportedOperation, OrdinaryDeposit, RefundClaim, TokenEnable, WithdrawalOutcome,
        WithdrawalProcessing, ZoneFacts, ZoneOperation,
    },
    state::{
        BatchBoundary, BatchId, BatchState, Cursor, DepositId, DepositOwner, FallbackId,
        FallbackState, InboxRefundId, Overlay, PortalRefundId, PortalState, RefundCredit,
        Settlement, State, StateDelta, StateKey, StateValue, TokenAccounting, TokenPhase,
        TokenState, Withdrawal, WithdrawalId, WithdrawalOrigin, WithdrawalOwner, ZoneState,
    },
};

/// A deterministic transition rule violation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum TransitionError {
    #[error("portal has not been created")]
    PortalNotCreated,
    #[error("portal is already created")]
    PortalAlreadyCreated,
    #[error("portal identity mismatch")]
    PortalIdentityMismatch,
    #[error("portal address does not match Zone ID")]
    PortalAddressMismatch,
    #[error("initial token mismatch")]
    InitialTokenMismatch,
    #[error("token {0} is already enabled")]
    TokenAlreadyEnabled(Address),
    #[error("token {0} is not enabled on the portal")]
    TokenNotEnabled(Address),
    #[error("token {0} is not enabled in the zone")]
    TokenNotZoneEnabled(Address),
    #[error("zone token enablement does not match imported portal operations")]
    TokenEnableMismatch,
    #[error("refund recipient is zero")]
    ZeroRefundRecipient,
    #[error("counter or accounting overflow")]
    Overflow,
    #[error("counter or accounting underflow")]
    Underflow,
    #[error("deposit owner collision")]
    DepositCollision,
    #[error("deposit prefix mismatch")]
    DepositPrefixMismatch,
    #[error("deposit outcome count mismatch")]
    DepositOutcomeCountMismatch,
    #[error("withdrawal outcome count mismatch")]
    WithdrawalOutcomeCountMismatch,
    #[error("withdrawal owner collision")]
    WithdrawalCollision,
    #[error("corrupt state row family")]
    CorruptState,
    #[error("withdrawal block cap exceeded")]
    WithdrawalCap,
    #[error("batch, queue, or commitment mismatch")]
    CommitmentMismatch,
    #[error("owner is missing or has the wrong lifecycle phase")]
    OwnerMismatch,
    #[error("refund claim amount mismatch")]
    RefundMismatch,
}

/// Intermediate state after Tempo operations and before Zone facts are applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportedCandidate {
    state: State,
    effects: Vec<Effect>,
    delta: StateDelta,
    token_enables: Vec<TokenEnable>,
    block_hash: alloy_primitives::B256,
    block_number: u64,
}

impl ImportedCandidate {
    /// Exact accounting after the imported Tempo block and before any Zone
    /// inputs are applied. This is the collateral cut authenticated on Tempo.
    pub(crate) fn expected_accounting(
        &self,
    ) -> Result<BTreeMap<Address, TokenAccounting>, TransitionError> {
        token_accounting(&Overlay::new(&self.state))
    }

    /// Effects independently predicted from the imported block alone.
    pub(crate) fn expected_effects(&self) -> &[Effect] {
        &self.effects
    }

    /// Materialize the authenticated post-import state without applying a
    /// synthetic Zone transition.
    pub(crate) fn into_state(self) -> State {
        self.state
    }
}

/// Expected effects, state commitments, and writes after one full transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TransitionCandidate {
    pub(crate) delta: StateDelta,
    pub(crate) expected_effects: Vec<Effect>,
    pub(crate) expected_state: ExpectedState,
    /// Exact accounting for every token in the resulting overlay, including
    /// unchanged token rows which therefore do not occur in `delta`.
    pub(crate) expected_accounting: BTreeMap<Address, TokenAccounting>,
}

#[derive(Clone, Copy)]
enum RefundSide {
    Portal,
    Inbox,
}

/// Promote tokens enabled before Zone genesis.
///
/// Genesis has no ordinary Zone transition, so this is the only phase promotion
/// without matching Zone token-enable effects. The caller must authenticate
/// zero genesis supply for every promoted token.
pub(crate) fn apply_genesis_handoff(parent: &State) -> Result<StateDelta, TransitionError> {
    if !matches!(portal(&Overlay::new(parent))?, PortalState::Created { .. }) {
        return Err(TransitionError::PortalNotCreated);
    }
    let tokens = parent
        .rows()
        .range((
            Bound::Included(StateKey::Token(Address::ZERO)),
            Bound::Included(StateKey::Token(Address::repeat_byte(0xff))),
        ))
        .map(|(key, value)| match (key, value) {
            (StateKey::Token(address), StateValue::Token(token)) => Ok((*address, *token)),
            _ => Err(TransitionError::CorruptState),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut overlay = Overlay::new(parent);
    for (address, mut token) in tokens {
        if token.phase == TokenPhase::PendingZoneEnable {
            token.phase = TokenPhase::ZoneEnabled;
            overlay.set(StateKey::Token(address), Some(StateValue::Token(token)));
        }
    }
    Ok(overlay.finish())
}

fn token_accounting(
    overlay: &Overlay<'_>,
) -> Result<BTreeMap<Address, TokenAccounting>, TransitionError> {
    overlay
        .range(
            std::ops::Bound::Included(StateKey::Token(Address::ZERO)),
            std::ops::Bound::Included(StateKey::Token(Address::repeat_byte(0xff))),
        )
        .map(|(key, value)| match (key, value) {
            (StateKey::Token(token), StateValue::Token(state)) => {
                Ok((token.to_owned(), state.accounting))
            }
            _ => Err(TransitionError::CorruptState),
        })
        .collect()
}

fn portal(overlay: &Overlay<'_>) -> Result<PortalState, TransitionError> {
    match overlay.get(&StateKey::Portal) {
        Some(StateValue::Portal(portal)) => Ok(portal.clone()),
        _ => Err(TransitionError::CorruptState),
    }
}

fn zone(overlay: &Overlay<'_>) -> Result<ZoneState, TransitionError> {
    match overlay.get(&StateKey::Zone) {
        Some(StateValue::Zone(zone)) => Ok(zone.clone()),
        _ => Err(TransitionError::CorruptState),
    }
}

fn token(overlay: &Overlay<'_>, address: Address) -> Result<TokenState, TransitionError> {
    match overlay.get(&StateKey::Token(address)) {
        Some(StateValue::Token(token)) => Ok(*token),
        _ => Err(TransitionError::TokenNotEnabled(address)),
    }
}

/// Consume all matching Portal or Inbox refund credits for one claim.
fn claim_refund(
    overlay: &mut Overlay<'_>,
    claim: RefundClaim,
    side: RefundSide,
    effects: &mut Vec<Effect>,
) -> Result<(), TransitionError> {
    let PortalState::Created { identity, .. } = portal(overlay)? else {
        return Err(TransitionError::PortalNotCreated);
    };
    let (start, end) = match side {
        RefundSide::Portal => (
            StateKey::PortalRefund(PortalRefundId {
                token: Address::ZERO,
                recipient: Address::ZERO,
                deposit: DepositId::new(Address::ZERO, 1).expect("nonzero lower bound"),
            }),
            StateKey::PortalRefund(PortalRefundId {
                token: Address::repeat_byte(0xff),
                recipient: Address::repeat_byte(0xff),
                deposit: DepositId::new(Address::repeat_byte(0xff), u64::MAX)
                    .expect("nonzero upper bound"),
            }),
        ),
        RefundSide::Inbox => (
            StateKey::InboxRefund(InboxRefundId {
                token: Address::ZERO,
                recipient: Address::ZERO,
                withdrawal: WithdrawalId {
                    zone_id: 0,
                    index: 0,
                },
            }),
            StateKey::InboxRefund(InboxRefundId {
                token: Address::repeat_byte(0xff),
                recipient: Address::repeat_byte(0xff),
                withdrawal: WithdrawalId {
                    zone_id: u32::MAX,
                    index: u64::MAX,
                },
            }),
        ),
    };
    let credits: Vec<_> = overlay
        .range(
            std::ops::Bound::Included(start),
            std::ops::Bound::Included(end),
        )
        .filter_map(|(key, value)| match (key, value) {
            (StateKey::PortalRefund(id), StateValue::PortalRefund(c))
                if matches!(side, RefundSide::Portal)
                    && id.deposit.portal == identity.portal
                    && id.token == claim.token
                    && id.recipient == claim.recipient =>
            {
                Some((key, c.amount))
            }
            (StateKey::InboxRefund(id), StateValue::InboxRefund(c))
                if matches!(side, RefundSide::Inbox)
                    && id.withdrawal.zone_id == identity.zone_id
                    && id.token == claim.token
                    && id.recipient == claim.recipient =>
            {
                Some((key, c.amount))
            }
            _ => None,
        })
        .collect();
    let total = credits
        .iter()
        .try_fold(0u128, |sum, (_, amount)| sum.checked_add(*amount))
        .ok_or(TransitionError::Overflow)?;
    if total != claim.amount {
        return Err(TransitionError::RefundMismatch);
    }
    if total != 0 {
        let mut t = token(overlay, claim.token)?;
        match side {
            RefundSide::Portal => {
                t.accounting.deposits = t
                    .accounting
                    .deposits
                    .checked_sub(U256::from(total))
                    .ok_or(TransitionError::Underflow)?;
            }
            RefundSide::Inbox => {
                if t.phase != TokenPhase::ZoneEnabled {
                    return Err(TransitionError::TokenNotZoneEnabled(claim.token));
                }
                t.accounting.supply = t
                    .accounting
                    .supply
                    .checked_add(U256::from(total))
                    .ok_or(TransitionError::Overflow)?;
                t.accounting.withdrawals = t
                    .accounting
                    .withdrawals
                    .checked_sub(U256::from(total))
                    .ok_or(TransitionError::Underflow)?;
            }
        }
        overlay.set(StateKey::Token(claim.token), Some(StateValue::Token(t)));
    }
    for (key, _) in credits {
        overlay.set(key, None);
    }
    effects.push(Effect::RefundClaimed {
        token: claim.token,
        recipient: claim.recipient,
        amount: total,
    });
    Ok(())
}

fn expected_state(
    overlay: &Overlay<'_>,
    tempo_block_hash: alloy_primitives::B256,
    tempo_block_number: u64,
) -> Result<ExpectedState, TransitionError> {
    let zone = zone(overlay)?;
    Ok(ExpectedState {
        tempo_block_hash,
        tempo_block_number,
        processed_deposit_hash: zone.processed_deposit.hash,
        processed_deposit_number: zone.processed_deposit.number,
        withdrawal_queue_hash: zone.withdrawal_queue_hash,
        withdrawal_batch_index: zone.withdrawal_batch_index,
    })
}
mod tempo;
mod zone;

pub(crate) use tempo::apply_imported;
pub(crate) use zone::apply_zone;
