use std::collections::BTreeSet;

use super::AuthoritativeStateError;
use crate::model::{
    ownership::{
        DepositOwner, FallbackOwner, PendingWithdrawal, WithdrawalIdentity, WithdrawalOwner,
    },
    state::ModelState,
};

pub(super) fn validate(state: &ModelState) -> Result<(), AuthoritativeStateError> {
    validate_failed_deposit_origins(state)?;
    validate_withdrawal_origins(state)
}

fn validate_failed_deposit_origins(state: &ModelState) -> Result<(), AuthoritativeStateError> {
    let mut owners = BTreeSet::new();
    for (id, owner) in state.pending_deposits() {
        if matches!(owner, DepositOwner::PendingOrdinary { .. }) {
            owners.insert(*id);
        }
    }
    for owner in state.withdrawals().values() {
        let deposit = match owner {
            WithdrawalOwner::Pending(PendingWithdrawal::FailedDeposit(pending)) => {
                Some(pending.parts().0)
            }
            WithdrawalOwner::Finalized(finalized) => match finalized.identity() {
                WithdrawalIdentity::FailedDeposit { deposit } => Some(deposit),
                WithdrawalIdentity::User(_) => None,
            },
            WithdrawalOwner::Pending(PendingWithdrawal::User(_)) => None,
        };
        if let Some(deposit) = deposit
            && !owners.insert(deposit)
        {
            return Err(AuthoritativeStateError::DuplicateDepositOriginOwner {
                deposit_number: deposit.deposit_number.get(),
            });
        }
    }
    for id in state.portal_refunds().keys() {
        if !owners.insert(id.failed_deposit) {
            return Err(AuthoritativeStateError::DuplicateDepositOriginOwner {
                deposit_number: id.failed_deposit.deposit_number.get(),
            });
        }
    }
    Ok(())
}

fn validate_withdrawal_origins(state: &ModelState) -> Result<(), AuthoritativeStateError> {
    let open_withdrawals = state.withdrawals().keys().copied().collect::<BTreeSet<_>>();
    let mut bounce_backs = BTreeSet::new();
    for owner in state.fallback_owners().values() {
        if let FallbackOwner::BounceBackQueued { withdrawal, .. } = owner
            && (!bounce_backs.insert(*withdrawal) || open_withdrawals.contains(withdrawal))
        {
            return Err(AuthoritativeStateError::DuplicateWithdrawalOriginOwner {
                withdrawal_index: withdrawal.withdrawal_index,
            });
        }
    }

    let mut refunds = BTreeSet::new();
    for id in state.inbox_refunds().keys() {
        let withdrawal = id.user_withdrawal;
        if !refunds.insert(withdrawal)
            || open_withdrawals.contains(&withdrawal)
            || bounce_backs.contains(&withdrawal)
        {
            return Err(AuthoritativeStateError::DuplicateWithdrawalOriginOwner {
                withdrawal_index: withdrawal.withdrawal_index,
            });
        }
    }
    Ok(())
}
