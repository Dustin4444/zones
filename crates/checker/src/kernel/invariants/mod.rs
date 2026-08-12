//! Internal consistency invariants for persisted checker state.
//!
//! These checks are independent of a particular authenticated block transition.

use std::collections::BTreeMap;

use alloy_primitives::{Address, U256};
use serde::{Deserialize, Serialize};

mod batches;
mod ownership;

use crate::kernel::{
    derivation::{bounceback_deposit_hash, ordinary_deposit_hash},
    finding::Datum,
    state::{
        Cursor, DepositOwner, FallbackState, PortalIdentity, PortalState, Settlement, State,
        StateKey, StateValue, TokenPhase, WithdrawalOrigin, WithdrawalOwner, ZoneState,
    },
};

/// Stable category for one persisted-state consistency violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum InvariantCode {
    MissingPortal,
    MissingZone,
    FamilyMismatch,
    PreCreationRows,
    DepositCursor,
    DepositToken,
    AccountingOverflow,
    AccountingMismatch,
    OwnerLink,
    Ring,
    CursorOrder,
    CounterBaseline,
    Identity,
    Bounds,
    WithdrawalSuffix,
    OriginExclusivity,
    Refund,
    Batch,
}

/// The first state location that violates an internal consistency rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct InvariantViolation {
    pub(crate) code: InvariantCode,
    pub(crate) location: Option<Box<StateKey>>,
    pub(crate) expected: Option<Datum>,
    pub(crate) actual: Option<Datum>,
}

/// Validate persisted state independently of a particular block transition.
pub(crate) fn validate(state: &State) -> Result<(), InvariantViolation> {
    let (portal, zone) = validate_structure(state)?;
    validate_common_rows(state, portal)?;
    let PortalState::Created {
        deposit: portal_cursor,
        settlement,
        ..
    } = portal
    else {
        return Ok(());
    };
    let identity = portal.identity();
    validate_created_baseline(state, identity, *portal_cursor, settlement, zone)?;
    validate_deposit_cursor(state, identity, zone.processed_deposit, *portal_cursor)?;
    batches::validate(state, identity, zone, settlement)?;
    ownership::validate(state, identity, zone, *portal_cursor)?;
    validate_withdrawal_suffix(state, zone)?;
    validate_token_accounting(state)
}

/// Validate the mandatory singleton rows and key-to-value family pairing.
fn validate_structure(state: &State) -> Result<(&PortalState, &ZoneState), InvariantViolation> {
    let Some(StateValue::Portal(portal)) = state.rows().get(&StateKey::Portal) else {
        return Err(violation(
            InvariantCode::MissingPortal,
            Some(StateKey::Portal),
        ));
    };
    let Some(StateValue::Zone(zone)) = state.rows().get(&StateKey::Zone) else {
        return Err(violation(InvariantCode::MissingZone, Some(StateKey::Zone)));
    };
    for (key, value) in state.rows() {
        if !value.matches_key(key) {
            return Err(violation(InvariantCode::FamilyMismatch, Some(*key)));
        }
    }
    Ok((portal, zone))
}

/// Validate rows shared by pre-creation and created state.
fn validate_common_rows(state: &State, portal: &PortalState) -> Result<(), InvariantViolation> {
    if matches!(portal, PortalState::AwaitingCreation(_)) && state.rows().len() != 2 {
        return Err(violation(InvariantCode::PreCreationRows, None));
    }
    for (key, value) in state.rows() {
        match (key, value) {
            (StateKey::Deposit(id), StateValue::Deposit(DepositOwner::Ordinary(deposit)))
                if !matches!(
                    state.rows().get(&StateKey::Token(deposit.token)),
                    Some(StateValue::Token(_))
                ) =>
            {
                return Err(violation(
                    InvariantCode::DepositToken,
                    Some(StateKey::Deposit(*id)),
                ));
            }
            (StateKey::Token(_), StateValue::Token(token))
                if token.accounting.collateral().is_none() =>
            {
                return Err(violation(InvariantCode::AccountingOverflow, Some(*key)));
            }
            (StateKey::Token(_), StateValue::Token(token))
                if token.phase == TokenPhase::PendingZoneEnable
                    && (token.accounting.supply != U256::ZERO
                        || token.accounting.withdrawals != U256::ZERO) =>
            {
                return Err(violation(InvariantCode::PreCreationRows, Some(*key)));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Validate counters, cursors, and queue bounds after portal creation.
fn validate_created_baseline(
    state: &State,
    identity: PortalIdentity,
    portal_cursor: Cursor,
    settlement: &Settlement,
    zone: &ZoneState,
) -> Result<(), InvariantViolation> {
    if state.token(identity.initial_token).is_none() {
        return Err(violation(
            InvariantCode::DepositToken,
            Some(StateKey::Token(identity.initial_token)),
        ));
    }
    if !is_well_formed_cursor(portal_cursor)
        || !is_well_formed_cursor(zone.processed_deposit)
        || !is_well_formed_cursor(settlement.submitted_deposit)
        || !is_well_formed_cursor(zone.batch_start.deposit)
        || !is_cursor_prefix(zone.processed_deposit, portal_cursor)
        || !is_cursor_prefix(settlement.submitted_deposit, zone.processed_deposit)
        || !is_cursor_prefix(zone.batch_start.deposit, zone.processed_deposit)
    {
        return Err(violation(
            InvariantCode::CursorOrder,
            Some(StateKey::Portal),
        ));
    }
    if (settlement.batch_index == 0 && *settlement != Settlement::ZERO)
        || settlement.batch_index > zone.withdrawal_batch_index
        || settlement.zone_height > U256::from(u64::MAX)
        || zone.last_fallback_nonce > zone.next_withdrawal_index
        || (zone.withdrawal_batch_index == 0
            && (!zone.withdrawal_queue_hash.is_zero()
                || zone.batch_start != crate::kernel::state::BatchBoundaryStart::ZERO))
        || zone.batch_start.withdrawal_index > zone.next_withdrawal_index
    {
        return Err(violation(
            InvariantCode::CounterBaseline,
            Some(StateKey::Zone),
        ));
    }
    if settlement.queue_tail < settlement.queue_head {
        return Err(violation(InvariantCode::Ring, Some(StateKey::Portal)));
    }
    Ok(())
}

/// Rebuild the unprocessed deposit chain and compare it with the portal cursor.
fn validate_deposit_cursor(
    state: &State,
    identity: PortalIdentity,
    processed: Cursor,
    portal_cursor: Cursor,
) -> Result<(), InvariantViolation> {
    let mut cursor = processed;
    if let Some(first_pending) = cursor.number.checked_add(1) {
        for number in first_pending..=portal_cursor.number {
            let Some(id) = crate::kernel::state::DepositId::new(identity.portal, number) else {
                return Err(violation(
                    InvariantCode::DepositCursor,
                    Some(StateKey::Portal),
                ));
            };
            let Some(StateValue::Deposit(owner)) = state.rows().get(&StateKey::Deposit(id)) else {
                return Err(violation(
                    InvariantCode::DepositCursor,
                    Some(StateKey::Deposit(id)),
                ));
            };
            cursor.hash = match owner {
                DepositOwner::Ordinary(value) => ordinary_deposit_hash(value, cursor.hash),
                DepositOwner::BounceBack {
                    token,
                    fallback_nonce,
                    amount,
                    ..
                } => bounceback_deposit_hash(
                    crate::kernel::facts::BounceBackDeposit {
                        token: *token,
                        fallback_nonce: *fallback_nonce,
                        amount: *amount,
                    },
                    cursor.hash,
                ),
            };
            cursor.number = number;
        }
    }
    if cursor != portal_cursor {
        return Err(violation(
            InvariantCode::DepositCursor,
            Some(StateKey::Portal),
        ));
    }
    Ok(())
}

/// Validate that pending withdrawals form the suffix after the latest batch boundary.
fn validate_withdrawal_suffix(state: &State, zone: &ZoneState) -> Result<(), InvariantViolation> {
    let mut expected = zone.batch_start.withdrawal_index;
    for (key, value) in state.rows() {
        let (StateKey::Withdrawal(id), StateValue::Withdrawal(owner)) = (key, value) else {
            continue;
        };
        if id.index < zone.batch_start.withdrawal_index {
            continue;
        }
        if id.index != expected
            || !matches!(
                owner,
                WithdrawalOwner::PendingFailedDeposit { .. } | WithdrawalOwner::PendingUser { .. }
            )
        {
            return Err(violation(InvariantCode::WithdrawalSuffix, Some(*key)));
        }
        expected = expected
            .checked_add(1)
            .ok_or_else(|| violation(InvariantCode::Bounds, Some(*key)))?;
    }
    if expected != zone.next_withdrawal_index {
        return Err(violation(
            InvariantCode::WithdrawalSuffix,
            Some(StateKey::Zone),
        ));
    }
    Ok(())
}

/// Reconstruct token accounting from live rows and compare it with token state.
fn validate_token_accounting(state: &State) -> Result<(), InvariantViolation> {
    let mut deposits = BTreeMap::<Address, U256>::new();
    let mut withdrawals = BTreeMap::<Address, U256>::new();
    let add_amount = |map: &mut BTreeMap<Address, U256>, token, amount| {
        let next = map
            .get(&token)
            .copied()
            .unwrap_or_default()
            .checked_add(U256::from(amount));
        next.map(|next| map.insert(token, next)).is_some()
    };
    for (key, value) in state.rows() {
        let valid = match (key, value) {
            (StateKey::Deposit(_), StateValue::Deposit(DepositOwner::Ordinary(deposit))) => {
                add_amount(&mut deposits, deposit.token, deposit.amount)
            }
            (
                StateKey::Withdrawal(_),
                StateValue::Withdrawal(WithdrawalOwner::PendingFailedDeposit {
                    token, amount, ..
                }),
            ) => add_amount(&mut deposits, *token, *amount),
            (
                StateKey::Withdrawal(_),
                StateValue::Withdrawal(WithdrawalOwner::Finalized {
                    data,
                    origin: WithdrawalOrigin::FailedDeposit { .. },
                }),
            ) => add_amount(&mut deposits, data.token, data.amount),
            (StateKey::PortalRefund(id), StateValue::PortalRefund(credit)) => {
                add_amount(&mut deposits, id.token, credit.amount)
            }
            (
                StateKey::Withdrawal(_),
                StateValue::Withdrawal(WithdrawalOwner::PendingUser { data, .. }),
            )
            | (
                StateKey::Withdrawal(_),
                StateValue::Withdrawal(WithdrawalOwner::Finalized {
                    data,
                    origin: WithdrawalOrigin::User { .. },
                }),
            ) => add_amount(&mut withdrawals, data.token, data.amount),
            (
                StateKey::Fallback(_),
                StateValue::Fallback(FallbackState::Queued { token, amount, .. }),
            ) => add_amount(&mut withdrawals, *token, *amount),
            (StateKey::InboxRefund(id), StateValue::InboxRefund(credit)) => {
                credit.amount != 0 && add_amount(&mut withdrawals, id.token, credit.amount)
            }
            _ => true,
        };
        if !valid {
            return Err(violation(InvariantCode::AccountingOverflow, Some(*key)));
        }
    }
    for (key, value) in state.rows() {
        if let (StateKey::Token(address), StateValue::Token(token)) = (key, value)
            && (token.accounting.deposits != deposits.remove(address).unwrap_or_default()
                || token.accounting.withdrawals != withdrawals.remove(address).unwrap_or_default())
        {
            return Err(violation(InvariantCode::AccountingMismatch, Some(*key)));
        }
    }
    if !deposits.is_empty() || !withdrawals.is_empty() {
        return Err(violation(InvariantCode::DepositToken, None));
    }
    Ok(())
}

/// Return whether a zero cursor carries no hash.
pub(super) fn is_well_formed_cursor(cursor: Cursor) -> bool {
    cursor.number != 0 || cursor.hash.is_zero()
}

/// Return whether `start` is a hash-consistent prefix of `end`.
pub(super) fn is_cursor_prefix(start: Cursor, end: Cursor) -> bool {
    start.number < end.number || start == end
}

/// Construct an invariant violation without an expected or actual datum.
pub(super) fn violation(code: InvariantCode, location: Option<StateKey>) -> InvariantViolation {
    InvariantViolation {
        code,
        location: location.map(Box::new),
        expected: None,
        actual: None,
    }
}
