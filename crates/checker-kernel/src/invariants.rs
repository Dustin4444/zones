use std::collections::BTreeMap;

use alloy_primitives::{Address, U256};
use serde::{Deserialize, Serialize};

use crate::{
    commitments::{RING_CAPACITY, bounceback_deposit_hash, ordinary_deposit_hash},
    finding::Datum,
    state::{
        BatchState, DepositOwner, FallbackState, PortalState, State, StateKey, StateValue,
        TokenPhase, WithdrawalOrigin, WithdrawalOwner,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvariantCode {
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvariantViolation {
    pub code: InvariantCode,
    pub location: Option<Box<StateKey>>,
    pub expected: Option<Datum>,
    pub actual: Option<Datum>,
}

pub fn validate(state: &State) -> Result<(), InvariantViolation> {
    let Some(StateValue::Portal(portal)) = state.rows().get(&StateKey::Portal) else {
        return Err(violation(
            InvariantCode::MissingPortal,
            Some(StateKey::Portal),
        ));
    };
    if !matches!(state.rows().get(&StateKey::Zone), Some(StateValue::Zone(_))) {
        return Err(violation(InvariantCode::MissingZone, Some(StateKey::Zone)));
    }
    for (key, value) in state.rows() {
        if !value.matches_key(key) {
            return Err(violation(InvariantCode::FamilyMismatch, Some(*key)));
        }
    }
    if matches!(portal, PortalState::AwaitingCreation(_)) && state.rows().len() != 2 {
        return Err(violation(InvariantCode::PreCreationRows, None));
    }
    for (key, value) in state.rows() {
        match (key, value) {
            (StateKey::Deposit(id), StateValue::Deposit(owner)) => {
                let crate::state::DepositOwner::Ordinary(deposit) = owner else {
                    continue;
                };
                if !matches!(
                    state.rows().get(&StateKey::Token(deposit.token)),
                    Some(StateValue::Token(_))
                ) {
                    return Err(violation(
                        InvariantCode::DepositToken,
                        Some(StateKey::Deposit(*id)),
                    ));
                }
            }
            (StateKey::Token(_), StateValue::Token(token)) => {
                if token.accounting.collateral().is_none() {
                    return Err(violation(InvariantCode::AccountingOverflow, Some(*key)));
                }
                if token.phase == TokenPhase::PendingZoneEnable
                    && token.accounting.supply != U256::ZERO
                {
                    return Err(violation(InvariantCode::PreCreationRows, Some(*key)));
                }
            }
            _ => {}
        }
    }
    let PortalState::Created {
        deposit: portal_cursor,
        settlement,
        ..
    } = portal
    else {
        return Ok(());
    };
    let StateValue::Zone(zone) = state.rows().get(&StateKey::Zone).expect("checked above") else {
        unreachable!()
    };
    if settlement.queue_tail < settlement.queue_head
        || settlement.queue_tail - settlement.queue_head > U256::from(RING_CAPACITY)
    {
        return Err(violation(InvariantCode::Ring, Some(StateKey::Portal)));
    }
    let mut cursor = zone.processed_deposit;
    if let Some(first_pending) = cursor.number.checked_add(1) {
        for number in first_pending..=portal_cursor.number {
            let Some(number) = std::num::NonZeroU64::new(number) else {
                return Err(violation(
                    InvariantCode::DepositCursor,
                    Some(StateKey::Portal),
                ));
            };
            let id = crate::state::DepositId {
                portal: portal.identity().portal,
                number,
            };
            let Some(StateValue::Deposit(owner)) = state.rows().get(&StateKey::Deposit(id)) else {
                return Err(violation(
                    InvariantCode::DepositCursor,
                    Some(StateKey::Deposit(id)),
                ));
            };
            cursor.hash = match owner {
                crate::state::DepositOwner::Ordinary(value) => {
                    ordinary_deposit_hash(value, cursor.hash)
                }
                crate::state::DepositOwner::BounceBack {
                    token,
                    fallback_nonce,
                    amount,
                    ..
                } => bounceback_deposit_hash(
                    crate::facts::BounceBackDeposit {
                        token: *token,
                        fallback_nonce: *fallback_nonce,
                        amount: *amount,
                    },
                    cursor.hash,
                ),
            };
            cursor.number = number.get();
        }
    }
    if cursor != *portal_cursor {
        return Err(violation(
            InvariantCode::DepositCursor,
            Some(StateKey::Portal),
        ));
    }

    let mut submitted = BTreeMap::new();
    for (key, value) in state.rows() {
        match (key, value) {
            (
                StateKey::Withdrawal(id),
                StateValue::Withdrawal(WithdrawalOwner::PendingUser { data, fallback }),
            ) => require_held_fallback(state, *id, data, *fallback, *key)?,
            (
                StateKey::Withdrawal(id),
                StateValue::Withdrawal(WithdrawalOwner::Finalized {
                    data,
                    origin: WithdrawalOrigin::User { fallback },
                }),
            ) => require_held_fallback(state, *id, data, *fallback, *key)?,
            (
                StateKey::Deposit(id),
                StateValue::Deposit(DepositOwner::BounceBack {
                    withdrawal,
                    token,
                    fallback_nonce,
                    amount,
                }),
            ) => {
                let fallback = crate::state::FallbackId {
                    zone_id: withdrawal.zone_id,
                    nonce: *fallback_nonce,
                };
                if !matches!(
                    state.rows().get(&StateKey::Fallback(fallback)),
                    Some(StateValue::Fallback(FallbackState::Queued {
                        withdrawal: actual_withdrawal,
                        token: actual_token,
                        amount: actual_amount,
                        deposit: actual_deposit,
                    })) if actual_withdrawal == withdrawal
                        && actual_token == token
                        && actual_amount == amount
                        && actual_deposit == id
                ) {
                    return Err(violation(InvariantCode::OwnerLink, Some(*key)));
                }
            }
            (
                StateKey::Fallback(fallback),
                StateValue::Fallback(FallbackState::Held {
                    withdrawal,
                    token,
                    amount,
                }),
            ) => {
                let linked = matches!(
                    state.rows().get(&StateKey::Withdrawal(*withdrawal)),
                    Some(StateValue::Withdrawal(WithdrawalOwner::PendingUser { data, fallback: actual }))
                        | Some(StateValue::Withdrawal(WithdrawalOwner::Finalized { data, origin: WithdrawalOrigin::User { fallback: actual } }))
                        if actual == fallback && data.token == *token && data.amount == *amount
                );
                if !linked {
                    return Err(violation(InvariantCode::OwnerLink, Some(*key)));
                }
            }
            (
                StateKey::Fallback(fallback),
                StateValue::Fallback(FallbackState::Queued {
                    withdrawal,
                    token,
                    amount,
                    deposit,
                }),
            ) => {
                let linked = matches!(
                    state.rows().get(&StateKey::Deposit(*deposit)),
                    Some(StateValue::Deposit(DepositOwner::BounceBack { withdrawal: actual_withdrawal, token: actual_token, fallback_nonce, amount: actual_amount }))
                        if actual_withdrawal == withdrawal && actual_token == token && *fallback_nonce == fallback.nonce && actual_amount == amount
                );
                if !linked {
                    return Err(violation(InvariantCode::OwnerLink, Some(*key)));
                }
            }
            (StateKey::PortalRefund(id), _) if id.deposit.portal != portal.identity().portal => {
                return Err(violation(InvariantCode::OwnerLink, Some(*key)));
            }
            (StateKey::InboxRefund(id), _)
                if id.withdrawal.zone_id != portal.identity().zone_id =>
            {
                return Err(violation(InvariantCode::OwnerLink, Some(*key)));
            }
            (
                StateKey::Batch(id),
                StateValue::Batch(BatchState::Submitted {
                    logical_queue_index,
                    ..
                }),
            ) => {
                let duplicate = submitted.insert(*logical_queue_index, *key).is_some();
                if id.zone_id != portal.identity().zone_id || duplicate {
                    return Err(violation(InvariantCode::OwnerLink, Some(*key)));
                }
            }
            _ => {}
        }
    }
    let expected_submitted = usize::try_from(settlement.queue_tail - settlement.queue_head)
        .map_err(|_| violation(InvariantCode::Ring, Some(StateKey::Portal)))?;
    if submitted.len() != expected_submitted
        || submitted
            .keys()
            .copied()
            .ne((0..expected_submitted).map(|offset| settlement.queue_head + U256::from(offset)))
    {
        return Err(violation(InvariantCode::Ring, Some(StateKey::Portal)));
    }

    let mut deposits = BTreeMap::<Address, U256>::new();
    let mut withdrawals = BTreeMap::<Address, U256>::new();
    let add = |map: &mut BTreeMap<Address, U256>, token, amount| {
        let next = map
            .get(&token)
            .copied()
            .unwrap_or_default()
            .checked_add(U256::from(amount));
        next.map(|next| map.insert(token, next)).is_some()
    };
    for (key, value) in state.rows() {
        let valid = match (key, value) {
            (
                StateKey::Deposit(_),
                StateValue::Deposit(crate::state::DepositOwner::Ordinary(d)),
            ) => add(&mut deposits, d.token, d.amount),
            (
                StateKey::Withdrawal(_),
                StateValue::Withdrawal(WithdrawalOwner::PendingFailedDeposit {
                    token, amount, ..
                }),
            ) => add(&mut deposits, *token, *amount),
            (
                StateKey::Withdrawal(_),
                StateValue::Withdrawal(WithdrawalOwner::Finalized {
                    data,
                    origin: WithdrawalOrigin::FailedDeposit { .. },
                }),
            ) => add(&mut deposits, data.token, data.amount),
            (StateKey::PortalRefund(_), StateValue::PortalRefund(c)) => {
                let StateKey::PortalRefund(id) = key else {
                    unreachable!()
                };
                add(&mut deposits, id.token, c.amount)
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
            ) => add(&mut withdrawals, data.token, data.amount),
            (
                StateKey::Fallback(_),
                StateValue::Fallback(FallbackState::Queued { token, amount, .. }),
            ) => add(&mut withdrawals, *token, *amount),
            (StateKey::InboxRefund(_), StateValue::InboxRefund(c)) => {
                let StateKey::InboxRefund(id) = key else {
                    unreachable!()
                };
                c.amount != 0 && add(&mut withdrawals, id.token, c.amount)
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

fn require_held_fallback(
    state: &State,
    withdrawal: crate::state::WithdrawalId,
    data: &crate::state::Withdrawal,
    fallback: crate::state::FallbackId,
    location: StateKey,
) -> Result<(), InvariantViolation> {
    if matches!(
        state.rows().get(&StateKey::Fallback(fallback)),
        Some(StateValue::Fallback(FallbackState::Held {
            withdrawal: actual_withdrawal,
            token,
            amount,
        })) if *actual_withdrawal == withdrawal && *token == data.token && *amount == data.amount
    ) {
        Ok(())
    } else {
        Err(violation(InvariantCode::OwnerLink, Some(location)))
    }
}

fn violation(code: InvariantCode, location: Option<StateKey>) -> InvariantViolation {
    InvariantViolation {
        code,
        location: location.map(Box::new),
        expected: None,
        actual: None,
    }
}
