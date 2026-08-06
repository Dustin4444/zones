use std::collections::BTreeMap;

use alloy_primitives::{Address, U256};

use super::{AuthoritativeStateError, LiabilityKind, OwnerKind};
use crate::model::{
    ownership::{
        FallbackOwner, InboxRefundOwner, PendingWithdrawal, PortalRefundOwner, WithdrawalIdentity,
        WithdrawalOwner,
    },
    state::{ModelState, TokenPhase},
};

#[derive(Debug, Clone, Copy, Default)]
struct Liabilities {
    deposit: U256,
    withdrawal: U256,
}

pub(super) fn validate_token_basics(
    state: &ModelState,
    initial_token: Address,
) -> Result<(), AuthoritativeStateError> {
    if state.token(initial_token).is_none() {
        return Err(AuthoritativeStateError::MissingInitialToken {
            token: initial_token,
        });
    }
    for (token, state) in state.tokens() {
        let accounting = state.accounting();
        if state.phase() == TokenPhase::PendingZoneEnable
            && (!accounting.supply.is_zero() || !accounting.withdrawal_liability.is_zero())
        {
            return Err(AuthoritativeStateError::PendingZoneTokenHasZoneState {
                token: *token,
                supply: accounting.supply,
                withdrawal_liability: accounting.withdrawal_liability,
            });
        }
        accounting
            .collateral_requirement()
            .map_err(|_| AuthoritativeStateError::TokenCollateralOverflow { token: *token })?;
    }
    Ok(())
}

pub(super) fn validate_liabilities(state: &ModelState) -> Result<(), AuthoritativeStateError> {
    let mut expected = state
        .tokens()
        .keys()
        .copied()
        .map(|token| (token, Liabilities::default()))
        .collect::<BTreeMap<_, _>>();

    for owner in state.pending_deposits().values() {
        if let crate::model::ownership::DepositOwner::PendingOrdinary { preimage } = owner {
            add(
                &mut expected,
                OwnerKind::PendingDeposit,
                preimage.token(),
                LiabilityKind::Deposit,
                preimage.amount(),
            )?;
        }
    }
    for owner in state.withdrawals().values() {
        match owner {
            WithdrawalOwner::Pending(PendingWithdrawal::FailedDeposit(pending)) => {
                let (_, token, _, amount) = pending.parts();
                add(
                    &mut expected,
                    OwnerKind::Withdrawal,
                    token,
                    LiabilityKind::Deposit,
                    amount,
                )?;
            }
            WithdrawalOwner::Finalized(finalized)
                if matches!(
                    finalized.identity(),
                    WithdrawalIdentity::FailedDeposit { .. }
                ) =>
            {
                add(
                    &mut expected,
                    OwnerKind::Withdrawal,
                    finalized.preimage().token(),
                    LiabilityKind::Deposit,
                    finalized.preimage().amount(),
                )?;
            }
            WithdrawalOwner::Pending(PendingWithdrawal::User(_))
            | WithdrawalOwner::Finalized(_) => {}
        }
    }
    for (id, owner) in state.portal_refunds() {
        let PortalRefundOwner::Pending { amount } = owner;
        add(
            &mut expected,
            OwnerKind::PortalRefund,
            id.token,
            LiabilityKind::Deposit,
            *amount,
        )?;
    }
    for owner in state.fallback_owners().values() {
        let (token, amount) = match owner {
            FallbackOwner::Held { token, amount, .. }
            | FallbackOwner::BounceBackQueued { token, amount, .. } => (*token, amount.get()),
        };
        add(
            &mut expected,
            OwnerKind::Fallback,
            token,
            LiabilityKind::Withdrawal,
            amount,
        )?;
    }
    for (id, owner) in state.inbox_refunds() {
        let InboxRefundOwner::Pending { amount } = owner;
        add(
            &mut expected,
            OwnerKind::InboxRefund,
            id.token,
            LiabilityKind::Withdrawal,
            amount.get(),
        )?;
    }

    for (token, state) in state.tokens() {
        let expected = expected
            .get(token)
            .expect("every token initializes one liability accumulator");
        let actual = state.accounting();
        require_equal(
            *token,
            LiabilityKind::Deposit,
            expected.deposit,
            actual.deposit_liability,
        )?;
        require_equal(
            *token,
            LiabilityKind::Withdrawal,
            expected.withdrawal,
            actual.withdrawal_liability,
        )?;
    }
    Ok(())
}

fn add(
    totals: &mut BTreeMap<Address, Liabilities>,
    owner: OwnerKind,
    token: Address,
    kind: LiabilityKind,
    amount: u128,
) -> Result<(), AuthoritativeStateError> {
    let total = totals
        .get_mut(&token)
        .ok_or(AuthoritativeStateError::MissingOwnerToken { owner, token })?;
    let component = match kind {
        LiabilityKind::Deposit => &mut total.deposit,
        LiabilityKind::Withdrawal => &mut total.withdrawal,
    };
    *component = component
        .checked_add(U256::from(amount))
        .ok_or(AuthoritativeStateError::TokenLiabilityOverflow { token, kind })?;
    Ok(())
}

fn require_equal(
    token: Address,
    kind: LiabilityKind,
    expected: U256,
    actual: U256,
) -> Result<(), AuthoritativeStateError> {
    if expected == actual {
        Ok(())
    } else {
        Err(AuthoritativeStateError::TokenLiabilityMismatch {
            token,
            kind,
            expected,
            actual,
        })
    }
}
