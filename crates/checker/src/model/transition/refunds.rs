//! Per-origin Portal and Inbox refund transitions.

use std::num::NonZeroU64;

use alloy_primitives::{Address, U256};

use super::{ModelError, ModelTransition, OwnerOverlay, require_zone_token};
use crate::model::{
    accounting::{AccountingTransition, apply_token_accounting},
    input::RefundClaimInput,
    output::ExpectedRefundClaim,
    ownership::{
        DepositId, InboxRefundId, InboxRefundOwner, PortalRefundId, PortalRefundOwner,
        RefundAccount, WithdrawalId,
    },
};

pub(super) fn create_portal_credit(
    candidate: &mut ModelTransition<'_>,
    id: PortalRefundId,
    amount: u128,
) -> Result<(), ModelError> {
    if candidate.portal_refund(id).is_some() {
        return Err(ModelError::PortalRefundCollision {
            deposit_number: id.failed_deposit.deposit_number.get(),
        });
    }
    let account = RefundAccount {
        token: id.token,
        recipient: id.recipient,
    };
    let total = candidate.portal_refund_total(account);
    let next_total = total
        .checked_add(amount)
        .ok_or(ModelError::RefundAggregateOverflow {
            token: id.token,
            recipient: id.recipient,
        })?;
    candidate.set_portal_refund_total(account, next_total);
    candidate.set_portal_refund(id, Some(PortalRefundOwner::Pending { amount }));
    Ok(())
}

pub(super) fn create_inbox_credit(
    candidate: &mut ModelTransition<'_>,
    id: InboxRefundId,
    owner: InboxRefundOwner,
) -> Result<(), ModelError> {
    if candidate.inbox_refund(id).is_some() {
        return Err(ModelError::InboxRefundCollision {
            withdrawal_index: id.user_withdrawal.withdrawal_index,
        });
    }
    let InboxRefundOwner::Pending { amount } = owner;
    let account = RefundAccount {
        token: id.token,
        recipient: id.recipient,
    };
    let total = candidate.inbox_refund_total(account);
    let next_total =
        total
            .checked_add(amount.get())
            .ok_or(ModelError::RefundAggregateOverflow {
                token: id.token,
                recipient: id.recipient,
            })?;
    candidate.set_inbox_refund_total(account, next_total);
    candidate.set_inbox_refund(id, Some(InboxRefundOwner::Pending { amount }));
    Ok(())
}

pub(super) fn claim_portal(
    candidate: &mut ModelTransition<'_>,
    input: RefundClaimInput,
) -> Result<ExpectedRefundClaim, ModelError> {
    let account = RefundAccount {
        token: input.token(),
        recipient: input.recipient(),
    };
    let (expected, credits) = portal_claim_snapshot(candidate, account)?;
    require_claim_amount(input, expected)?;

    if expected != 0 {
        let mut token =
            candidate
                .token(input.token())
                .cloned()
                .ok_or(ModelError::TokenNotPortalEnabled {
                    token: input.token(),
                })?;
        token.accounting = apply_token_accounting(
            Some(token.accounting),
            AccountingTransition::PortalRefundClaimed {
                refund_amount: U256::from(expected),
            },
        )?;
        candidate.set_token(input.token(), token);
    }
    for id in credits {
        candidate.set_portal_refund(id, None);
    }
    candidate.set_portal_refund_total(account, 0);
    Ok(ExpectedRefundClaim::new(
        input.recipient(),
        input.token(),
        expected,
    ))
}

pub(super) fn claim_inbox(
    candidate: &mut ModelTransition<'_>,
    input: RefundClaimInput,
) -> Result<ExpectedRefundClaim, ModelError> {
    let account = RefundAccount {
        token: input.token(),
        recipient: input.recipient(),
    };
    let (expected, credits) = inbox_claim_snapshot(candidate, account)?;
    require_claim_amount(input, expected)?;

    if expected != 0 {
        let mut token = require_zone_token(candidate, input.token())?;
        token.accounting = apply_token_accounting(
            Some(token.accounting),
            AccountingTransition::InboxRefundClaimed {
                amount: U256::from(expected),
            },
        )?;
        candidate.set_token(input.token(), token);
    }
    for id in credits {
        candidate.set_inbox_refund(id, None);
    }
    candidate.set_inbox_refund_total(account, 0);
    Ok(ExpectedRefundClaim::new(
        input.recipient(),
        input.token(),
        expected,
    ))
}

fn portal_claim_snapshot(
    candidate: &ModelTransition<'_>,
    account: RefundAccount,
) -> Result<(u128, Vec<PortalRefundId>), ModelError> {
    let mut ids = Vec::new();
    let total = checked_sum(
        portal_credits(candidate, account.token, account.recipient)?.map(|(id, owner)| {
            ids.push(id);
            let PortalRefundOwner::Pending { amount } = owner;
            *amount
        }),
        account.token,
        account.recipient,
    )?;
    Ok((total, ids))
}

fn inbox_claim_snapshot(
    candidate: &ModelTransition<'_>,
    account: RefundAccount,
) -> Result<(u128, Vec<InboxRefundId>), ModelError> {
    let mut ids = Vec::new();
    let total = checked_sum(
        inbox_credits(candidate, account.token, account.recipient).map(|(id, owner)| {
            ids.push(id);
            let InboxRefundOwner::Pending { amount } = owner;
            amount.get()
        }),
        account.token,
        account.recipient,
    )?;
    Ok((total, ids))
}

fn checked_sum(
    amounts: impl IntoIterator<Item = u128>,
    token: Address,
    recipient: Address,
) -> Result<u128, ModelError> {
    amounts.into_iter().try_fold(0_u128, |total, amount| {
        total
            .checked_add(amount)
            .ok_or(ModelError::RefundAggregateOverflow { token, recipient })
    })
}

fn require_claim_amount(input: RefundClaimInput, expected: u128) -> Result<(), ModelError> {
    if input.amount() != expected {
        return Err(ModelError::RefundClaimAmountMismatch {
            token: input.token(),
            recipient: input.recipient(),
            expected,
            actual: input.amount(),
        });
    }
    Ok(())
}

fn portal_credits<'a>(
    candidate: &'a ModelTransition<'_>,
    token: Address,
    recipient: Address,
) -> Result<OwnerOverlay<'a, PortalRefundId, PortalRefundOwner>, ModelError> {
    let portal = candidate
        .portal()
        .created()
        .ok_or(ModelError::PortalNotCreated)?
        .identity()
        .portal();
    let lower = PortalRefundId {
        token,
        recipient,
        failed_deposit: DepositId {
            portal,
            deposit_number: NonZeroU64::MIN,
        },
    };
    let upper = PortalRefundId {
        token,
        recipient,
        failed_deposit: DepositId {
            portal,
            deposit_number: NonZeroU64::new(u64::MAX).expect("u64::MAX is nonzero"),
        },
    };
    Ok(OwnerOverlay::new(
        &candidate.parent.portal_refunds,
        &candidate.delta.portal_refunds,
        lower..=upper,
    ))
}

fn inbox_credits<'a>(
    candidate: &'a ModelTransition<'_>,
    token: Address,
    recipient: Address,
) -> OwnerOverlay<'a, InboxRefundId, InboxRefundOwner> {
    let zone_id = candidate.portal().zone_id();
    let lower = InboxRefundId {
        token,
        recipient,
        user_withdrawal: WithdrawalId {
            zone_id,
            withdrawal_index: 0,
        },
    };
    let upper = InboxRefundId {
        token,
        recipient,
        user_withdrawal: WithdrawalId {
            zone_id,
            withdrawal_index: u64::MAX,
        },
    };
    OwnerOverlay::new(
        &candidate.parent.inbox_refunds,
        &candidate.delta.inbox_refunds,
        lower..=upper,
    )
}
