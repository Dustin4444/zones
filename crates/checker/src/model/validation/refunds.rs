use std::collections::BTreeMap;

use alloy_primitives::Address;

use super::{AuthoritativeStateError, OwnerKind, RefundLedger};
use crate::model::{
    ownership::{InboxRefundOwner, PortalRefundOwner, RefundAccount},
    state::ModelState,
};

use super::owners::{
    require_deposit_origin, require_portal, require_withdrawal_origin, require_zone,
    require_zone_token,
};

pub(super) fn validate(
    state: &ModelState,
    portal: Address,
    zone_id: u32,
    zone_processed_deposit_number: u64,
) -> Result<(), AuthoritativeStateError> {
    let mut portal_totals = BTreeMap::new();
    for (id, owner) in state.portal_refunds() {
        if id.recipient.is_zero() {
            return Err(AuthoritativeStateError::ZeroPortalRefundRecipient {
                deposit_number: id.failed_deposit.deposit_number.get(),
            });
        }
        require_portal(OwnerKind::PortalRefund, portal, id.failed_deposit.portal)?;
        require_deposit_origin(
            state,
            OwnerKind::PortalRefund,
            id.failed_deposit,
            zone_processed_deposit_number,
        )?;
        require_zone_token(state, OwnerKind::PortalRefund, id.token)?;
        let PortalRefundOwner::Pending { amount } = owner;
        add(
            &mut portal_totals,
            RefundLedger::Portal,
            RefundAccount {
                token: id.token,
                recipient: id.recipient,
            },
            *amount,
        )?;
    }

    let mut inbox_totals = BTreeMap::new();
    for (id, owner) in &state.inbox_refunds {
        if id.recipient.is_zero() {
            return Err(AuthoritativeStateError::ZeroInboxRefundRecipient {
                withdrawal_index: id.user_withdrawal.withdrawal_index,
            });
        }
        require_zone(OwnerKind::InboxRefund, zone_id, id.user_withdrawal.zone_id)?;
        require_withdrawal_origin(state, id.user_withdrawal.withdrawal_index)?;
        require_zone_token(state, OwnerKind::InboxRefund, id.token)?;
        let InboxRefundOwner::Pending { amount } = owner;
        add(
            &mut inbox_totals,
            RefundLedger::Inbox,
            RefundAccount {
                token: id.token,
                recipient: id.recipient,
            },
            amount.get(),
        )?;
    }
    Ok(())
}

fn add(
    totals: &mut BTreeMap<RefundAccount, u128>,
    ledger: RefundLedger,
    account: RefundAccount,
    amount: u128,
) -> Result<(), AuthoritativeStateError> {
    if amount == 0 {
        return Ok(());
    }
    let total = totals.entry(account).or_default();
    *total = total
        .checked_add(amount)
        .ok_or(AuthoritativeStateError::RefundAggregateOverflow {
            ledger,
            token: account.token,
            recipient: account.recipient,
        })?;
    Ok(())
}
