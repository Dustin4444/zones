use std::collections::BTreeMap;

use alloy_primitives::{Address, U256};
use serde::{Deserialize, Serialize};

use crate::kernel::{
    commitments::{RING_CAPACITY, bounceback_deposit_hash, ordinary_deposit_hash},
    finding::Datum,
    state::{
        BatchState, DepositOwner, FallbackState, PortalState, State, StateKey, StateValue,
        TokenPhase, WithdrawalOrigin, WithdrawalOwner,
    },
};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct InvariantViolation {
    pub(crate) code: InvariantCode,
    pub(crate) location: Option<Box<StateKey>>,
    pub(crate) expected: Option<Datum>,
    pub(crate) actual: Option<Datum>,
}

pub(crate) fn validate(state: &State) -> Result<(), InvariantViolation> {
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
                let crate::kernel::state::DepositOwner::Ordinary(deposit) = owner else {
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
                    && (token.accounting.supply != U256::ZERO
                        || token.accounting.withdrawals != U256::ZERO)
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
    let zone = state.zone().expect("checked above");
    let identity = portal.identity();
    if state.token(identity.initial_token).is_none() {
        return Err(violation(
            InvariantCode::DepositToken,
            Some(StateKey::Token(identity.initial_token)),
        ));
    }
    let good_cursor =
        |cursor: crate::kernel::state::Cursor| cursor.number != 0 || cursor.hash.is_zero();
    if !good_cursor(*portal_cursor)
        || !good_cursor(zone.processed_deposit)
        || !good_cursor(settlement.submitted_deposit)
        || !good_cursor(zone.batch_start.deposit)
        || zone.processed_deposit.number > portal_cursor.number
        || settlement.submitted_deposit.number > zone.processed_deposit.number
        || zone.batch_start.deposit.number > zone.processed_deposit.number
        || (zone.processed_deposit.number == portal_cursor.number
            && zone.processed_deposit.hash != portal_cursor.hash)
        || (settlement.submitted_deposit.number == zone.processed_deposit.number
            && settlement.submitted_deposit.hash != zone.processed_deposit.hash)
        || (zone.batch_start.deposit.number == zone.processed_deposit.number
            && zone.batch_start.deposit.hash != zone.processed_deposit.hash)
    {
        return Err(violation(
            InvariantCode::CursorOrder,
            Some(StateKey::Portal),
        ));
    }
    if (settlement.batch_index == 0 && *settlement != crate::kernel::state::Settlement::ZERO)
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
    if settlement.queue_tail < settlement.queue_head
        || settlement.queue_tail - settlement.queue_head > U256::from(RING_CAPACITY)
    {
        return Err(violation(InvariantCode::Ring, Some(StateKey::Portal)));
    }
    let mut cursor = zone.processed_deposit;
    if let Some(first_pending) = cursor.number.checked_add(1) {
        for number in first_pending..=portal_cursor.number {
            let Some(id) = crate::kernel::state::DepositId::new(portal.identity().portal, number)
            else {
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
                crate::kernel::state::DepositOwner::Ordinary(value) => {
                    ordinary_deposit_hash(value, cursor.hash)
                }
                crate::kernel::state::DepositOwner::BounceBack {
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
    if cursor != *portal_cursor {
        return Err(violation(
            InvariantCode::DepositCursor,
            Some(StateKey::Portal),
        ));
    }

    validate_batches(state, identity, zone, settlement)?;
    let mut deposit_origins = std::collections::BTreeSet::new();
    let mut withdrawal_origins = std::collections::BTreeSet::new();
    let mut refund_totals = BTreeMap::<(bool, Address, Address), u128>::new();
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
                let fallback =
                    crate::kernel::state::FallbackId::new(withdrawal.zone_id, fallback_nonce.get())
                        .expect("stored fallback nonce is nonzero");
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
            (StateKey::Deposit(id), StateValue::Deposit(owner)) => {
                if id.portal != identity.portal
                    || id.number.get() <= zone.processed_deposit.number
                    || id.number.get() > portal_cursor.number
                    || match owner {
                        DepositOwner::Ordinary(d) => d.tempo_refund_recipient.is_zero(),
                        DepositOwner::BounceBack {
                            withdrawal, token, ..
                        } => withdrawal.zone_id != identity.zone_id || !zone_token(state, *token),
                    }
                {
                    return Err(violation(InvariantCode::Bounds, Some(*key)));
                }
                if matches!(owner, DepositOwner::Ordinary(_)) && !deposit_origins.insert(*id) {
                    return Err(violation(InvariantCode::OriginExclusivity, Some(*key)));
                }
            }
            (StateKey::Withdrawal(id), StateValue::Withdrawal(owner)) => {
                let token = match owner {
                    WithdrawalOwner::PendingFailedDeposit {
                        deposit,
                        token,
                        recipient,
                        ..
                    } => {
                        if recipient.is_zero()
                            || deposit.portal != identity.portal
                            || deposit.number.get() > zone.processed_deposit.number
                            || !deposit_origins.insert(*deposit)
                        {
                            return Err(violation(InvariantCode::OriginExclusivity, Some(*key)));
                        }
                        *token
                    }
                    WithdrawalOwner::PendingUser { data, .. } => data.token,
                    WithdrawalOwner::Finalized { data, origin } => {
                        if let WithdrawalOrigin::FailedDeposit { deposit } = origin
                            && (data.to.is_zero()
                                || deposit.portal != identity.portal
                                || deposit.number.get() > zone.processed_deposit.number
                                || !deposit_origins.insert(*deposit))
                        {
                            return Err(violation(InvariantCode::OriginExclusivity, Some(*key)));
                        }
                        data.token
                    }
                };
                if id.zone_id != identity.zone_id
                    || id.index >= zone.next_withdrawal_index
                    || !zone_token(state, token)
                {
                    return Err(violation(InvariantCode::Identity, Some(*key)));
                }
                if !withdrawal_origins.insert(*id) {
                    return Err(violation(InvariantCode::OriginExclusivity, Some(*key)));
                }
            }
            (StateKey::PortalRefund(id), StateValue::PortalRefund(credit))
                if id.deposit.portal != identity.portal
                    || id.recipient.is_zero()
                    || !zone_token(state, id.token)
                    || id.deposit.number.get() > zone.processed_deposit.number
                    || !deposit_origins.insert(id.deposit)
                    || !add_refund(
                        &mut refund_totals,
                        (false, id.token, id.recipient),
                        credit.amount,
                    ) =>
            {
                return Err(violation(InvariantCode::Refund, Some(*key)));
            }
            (StateKey::InboxRefund(id), StateValue::InboxRefund(credit))
                if id.withdrawal.zone_id != identity.zone_id
                    || id.recipient.is_zero()
                    || !zone_token(state, id.token)
                    || id.withdrawal.index >= zone.next_withdrawal_index
                    || !withdrawal_origins.insert(id.withdrawal)
                    || !add_refund(
                        &mut refund_totals,
                        (true, id.token, id.recipient),
                        credit.amount,
                    ) =>
            {
                return Err(violation(InvariantCode::Refund, Some(*key)));
            }
            _ => {}
        }
    }
    let mut expected = zone.batch_start.withdrawal_index;
    for (key, value) in state.rows() {
        if let (StateKey::Withdrawal(id), StateValue::Withdrawal(owner)) = (key, value)
            && id.index >= zone.batch_start.withdrawal_index
        {
            if id.index != expected
                || !matches!(
                    owner,
                    WithdrawalOwner::PendingFailedDeposit { .. }
                        | WithdrawalOwner::PendingUser { .. }
                )
            {
                return Err(violation(InvariantCode::WithdrawalSuffix, Some(*key)));
            }
            expected = expected
                .checked_add(1)
                .ok_or_else(|| violation(InvariantCode::Bounds, Some(*key)))?;
        }
    }
    if expected != zone.next_withdrawal_index {
        return Err(violation(
            InvariantCode::WithdrawalSuffix,
            Some(StateKey::Zone),
        ));
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
                StateValue::Deposit(crate::kernel::state::DepositOwner::Ordinary(d)),
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

fn zone_token(state: &State, token: Address) -> bool {
    matches!(state.rows().get(&StateKey::Token(token)), Some(StateValue::Token(t)) if t.phase == TokenPhase::ZoneEnabled)
}

fn add_refund(
    totals: &mut BTreeMap<(bool, Address, Address), u128>,
    account: (bool, Address, Address),
    amount: u128,
) -> bool {
    totals
        .get(&account)
        .copied()
        .unwrap_or_default()
        .checked_add(amount)
        .map(|total| totals.insert(account, total))
        .is_some()
}

fn validate_batches(
    state: &State,
    identity: crate::kernel::state::PortalIdentity,
    zone: &crate::kernel::state::ZoneState,
    settlement: &crate::kernel::state::Settlement,
) -> Result<(), InvariantViolation> {
    let bad = |key| violation(InvariantCode::Batch, Some(key));
    let mut queues = BTreeMap::new();
    let mut owned = std::collections::BTreeSet::new();
    let mut prior_end = None;
    let mut prior_submitted: Option<(u64, crate::kernel::state::BatchBoundary)> = None;
    let mut prior_unsubmitted = (
        settlement.batch_index,
        settlement.block_hash,
        settlement.submitted_deposit,
        u64::try_from(settlement.zone_height).map_err(|_| bad(StateKey::Portal))?,
        settlement.tempo_block,
    );
    let mut last: Option<(u64, u64)> = None;

    for (key, value) in state.rows() {
        let (StateKey::Batch(id), StateValue::Batch(batch)) = (key, value) else {
            continue;
        };
        let index = id.index.get();
        let (boundary, first, count, hash, ordinal, queue) = match batch {
            BatchState::Finalized {
                boundary,
                first_withdrawal,
                count,
                queue_hash,
            } => {
                if index <= settlement.batch_index {
                    return Err(bad(*key));
                }
                (*boundary, *first_withdrawal, *count, *queue_hash, 0, None)
            }
            BatchState::Submitted {
                boundary,
                first_withdrawal,
                count,
                queue_hash,
                next_ordinal,
                logical_queue_index,
            } => {
                if index > settlement.batch_index
                    || *logical_queue_index < settlement.queue_head
                    || *logical_queue_index >= settlement.queue_tail
                    || queues.insert(*logical_queue_index, index).is_some()
                {
                    return Err(violation(InvariantCode::Ring, Some(*key)));
                }
                (
                    *boundary,
                    *first_withdrawal,
                    *count,
                    *queue_hash,
                    *next_ordinal,
                    Some(*logical_queue_index),
                )
            }
        };
        let cursor_ok = |c: crate::kernel::state::Cursor| c.number != 0 || c.hash.is_zero();
        let prefix = |a: crate::kernel::state::Cursor, b: crate::kernel::state::Cursor| {
            a.number < b.number || a == b
        };
        let end = first.checked_add(count).ok_or_else(|| bad(*key))?;
        if id.zone_id != identity.zone_id
            || index > zone.withdrawal_batch_index
            || ordinal > count
            || end > zone.next_withdrawal_index
            || !cursor_ok(boundary.first_deposit)
            || !cursor_ok(boundary.final_deposit)
            || !prefix(boundary.first_deposit, boundary.final_deposit)
            || !prefix(boundary.final_deposit, zone.processed_deposit)
            || prior_end
                .or((settlement.batch_index == 0).then_some(0))
                .is_some_and(|v| v != first)
        {
            return Err(bad(*key));
        }
        prior_end = Some(end);
        last = Some((index, end));

        if let Some(q) = queue {
            if q != settlement.queue_head && ordinal != 0 {
                return Err(bad(*key));
            }
            if let Some((pi, pb)) = prior_submitted {
                let advance = index - pi;
                let adjacent = advance == 1;
                let deposit_bad = boundary.first_deposit.number < pb.final_deposit.number
                    || (boundary.first_deposit.number == pb.final_deposit.number
                        && boundary.first_deposit.hash != pb.final_deposit.hash)
                    || (adjacent && boundary.first_deposit != pb.final_deposit);
                let za = boundary.zone_height.saturating_sub(pb.zone_height);
                let ta = boundary.tempo_block.saturating_sub(pb.tempo_block);
                if (adjacent && boundary.first_parent != pb.final_block)
                    || deposit_bad
                    || za < advance
                    || ta < za
                {
                    return Err(bad(*key));
                }
            }
            prior_submitted = Some((index, boundary));
        } else {
            let (pi, block, deposit, zh, th) = prior_unsubmitted;
            let za = boundary.zone_height.checked_sub(zh).filter(|v| *v != 0);
            let ta = boundary.tempo_block.checked_sub(th).filter(|v| *v != 0);
            if index != pi.checked_add(1).ok_or_else(|| bad(*key))?
                || boundary.first_parent != block
                || boundary.first_deposit != deposit
                || za.is_none()
                || ta.is_none()
                || (pi != 0 && za > ta)
            {
                return Err(bad(*key));
            }
            prior_unsubmitted = (
                index,
                boundary.final_block,
                boundary.final_deposit,
                boundary.zone_height,
                boundary.tempo_block,
            );
        }

        let start = first.checked_add(ordinal).ok_or_else(|| bad(*key))?;
        let mut values = Vec::new();
        for wi in start..end {
            let wk = StateKey::Withdrawal(crate::kernel::state::WithdrawalId {
                zone_id: identity.zone_id,
                index: wi,
            });
            let Some(StateValue::Withdrawal(WithdrawalOwner::Finalized { data, .. })) =
                state.rows().get(&wk)
            else {
                return Err(bad(wk));
            };
            if !owned.insert(wi) {
                return Err(bad(wk));
            }
            values.push(data.clone());
        }
        if crate::kernel::commitments::withdrawal_queue_hash(&values) != hash {
            return Err(bad(*key));
        }

        if index == settlement.batch_index
            && queue.is_some()
            && (settlement.block_hash != boundary.final_block
                || settlement.tempo_block != boundary.tempo_block
                || settlement.submitted_deposit != boundary.final_deposit
                || settlement.zone_height != U256::from(boundary.zone_height))
        {
            return Err(bad(*key));
        }
        if index == zone.withdrawal_batch_index
            && (zone.withdrawal_queue_hash != hash
                || zone.batch_start.parent_hash != boundary.final_block
                || zone.batch_start.deposit != boundary.final_deposit
                || zone.batch_start.withdrawal_index != end)
        {
            return Err(bad(*key));
        }
    }
    if prior_unsubmitted.0 != zone.withdrawal_batch_index {
        return Err(bad(StateKey::Zone));
    }
    if last.is_some_and(|(_, end)| end != zone.batch_start.withdrawal_index) {
        return Err(bad(StateKey::Zone));
    }
    for (key, value) in state.rows() {
        if let (StateKey::Withdrawal(id), StateValue::Withdrawal(WithdrawalOwner::Finalized { .. })) =
            (key, value)
            && !owned.contains(&id.index)
        {
            return Err(bad(*key));
        }
    }
    let expected = usize::try_from(settlement.queue_tail - settlement.queue_head)
        .map_err(|_| violation(InvariantCode::Ring, Some(StateKey::Portal)))?;
    if queues.len() != expected
        || queues.iter().enumerate().any(|(n, (q, _))| {
            *q != settlement.queue_head + U256::from(n)
                || n != 0
                    && queues
                        .values()
                        .nth(n - 1)
                        .is_some_and(|prior| queues[q] <= *prior)
        })
    {
        return Err(violation(InvariantCode::Ring, Some(StateKey::Portal)));
    }
    if let Some((pi, pb)) = prior_submitted
        && pi != settlement.batch_index
    {
        let advance = settlement.batch_index - pi;
        let za = u64::try_from(settlement.zone_height)
            .unwrap()
            .saturating_sub(pb.zone_height);
        let ta = settlement.tempo_block.saturating_sub(pb.tempo_block);
        let deposit_bad = settlement.submitted_deposit.number < pb.final_deposit.number
            || (settlement.submitted_deposit.number == pb.final_deposit.number
                && settlement.submitted_deposit.hash != pb.final_deposit.hash);
        if deposit_bad || za < advance || ta < za {
            return Err(bad(StateKey::Portal));
        }
    }
    if settlement.batch_index == zone.withdrawal_batch_index
        && (settlement.block_hash != zone.batch_start.parent_hash
            || settlement.submitted_deposit != zone.batch_start.deposit)
    {
        return Err(bad(StateKey::Portal));
    }
    Ok(())
}

fn require_held_fallback(
    state: &State,
    withdrawal: crate::kernel::state::WithdrawalId,
    data: &crate::kernel::state::Withdrawal,
    fallback: crate::kernel::state::FallbackId,
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
