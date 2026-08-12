//! Parses the exact Portal event grammar for withdrawal processing.

use alloy_primitives::Address;

use crate::{
    failure::Failure,
    kernel::{Effect, WithdrawalOutcome},
    observe::events::{L1ProtocolEvent, Portal},
};

use super::super::{AdapterFindingCode, deposits::ordinary_deposit_event, failure};

/// Parsed effects and outcomes for one `processWithdrawals` call.
pub(crate) struct WithdrawalAdaptation {
    pub(crate) outcomes: Vec<WithdrawalOutcome>,
    pub(crate) effects: Vec<Effect>,
}

/// Parse the exact event sequence produced by `processWithdrawals`.
pub(super) fn parse_withdrawal_events(
    events: &[&L1ProtocolEvent],
    member_count: usize,
    portal: Address,
) -> Result<WithdrawalAdaptation, Failure> {
    let mut cursor = 0;
    let mut outcomes = Vec::with_capacity(member_count);
    let mut effects = Vec::new();
    for _ in 0..member_count {
        let event = events.get(cursor).ok_or_else(|| {
            failure(
                AdapterFindingCode::Grammar,
                "processWithdrawals missing member outcome",
            )
        })?;
        cursor += 1;
        match event {
            L1ProtocolEvent::Portal(Portal::ZonePortalEvents::DepositBounceBack(e)) => {
                outcomes.push(WithdrawalOutcome::FailedDepositPaid {
                    collected_fee: e.bouncebackFee,
                });
                effects.push(Effect::FailedDepositRefunded {
                    recipient: e.tempoRefundRecipient,
                    token: e.token,
                    amount: e.amount,
                    fee: e.bouncebackFee,
                    pending: false,
                });
            }
            L1ProtocolEvent::Portal(Portal::ZonePortalEvents::DepositBounceBackPending(e)) => {
                outcomes.push(WithdrawalOutcome::FailedDepositPending {
                    collected_fee: e.bouncebackFee,
                });
                effects.push(Effect::FailedDepositRefunded {
                    recipient: e.tempoRefundRecipient,
                    token: e.token,
                    amount: e.amount,
                    fee: e.bouncebackFee,
                    pending: true,
                });
            }
            L1ProtocolEvent::Portal(Portal::ZonePortalEvents::WithdrawalBounceBack(e)) => {
                effects.push(Effect::BounceBackAppended {
                    fallback_nonce: e.fallbackNonce,
                    token: e.token,
                    amount: e.amount,
                    id: crate::kernel::DepositId::new(portal, e.depositNumber).ok_or_else(
                        || {
                            failure(
                                AdapterFindingCode::Grammar,
                                "zero bounceback deposit number",
                            )
                        },
                    )?,
                    queue_hash: e.newCurrentDepositQueueHash,
                });
                let Some(L1ProtocolEvent::Portal(Portal::ZonePortalEvents::WithdrawalProcessed(
                    processed,
                ))) = events.get(cursor).copied()
                else {
                    return Err(failure(
                        AdapterFindingCode::Grammar,
                        "WithdrawalBounceBack must be followed by WithdrawalProcessed",
                    ));
                };
                cursor += 1;
                if processed.callbackSuccess {
                    return Err(failure(
                        AdapterFindingCode::Grammar,
                        "bounce WithdrawalProcessed callbackSuccess must be false",
                    ));
                }
                effects.push(Effect::UserWithdrawalProcessed {
                    to: processed.to,
                    sender_tag: processed.senderTag,
                    token: processed.token,
                    amount: processed.amount,
                    callback_success: false,
                });
                outcomes.push(WithdrawalOutcome::UserBounced);
            }
            L1ProtocolEvent::Portal(Portal::ZonePortalEvents::DepositMade(first)) => {
                let mut callback_deposits = Vec::new();
                let mut next = Some(first.clone());
                while let Some(deposit) = next.take() {
                    callback_deposits.push(ordinary_deposit_event(&deposit, "callback")?);
                    effects.push(Effect::DepositAppended {
                        id: crate::kernel::DepositId::new(portal, deposit.depositNumber)
                            .ok_or_else(|| {
                                failure(AdapterFindingCode::Grammar, "zero callback deposit number")
                            })?,
                        queue_hash: deposit.newCurrentDepositQueueHash,
                    });
                    if let Some(L1ProtocolEvent::Portal(Portal::ZonePortalEvents::DepositMade(d))) =
                        events.get(cursor).copied()
                    {
                        cursor += 1;
                        next = Some(d.clone());
                    }
                }
                let Some(L1ProtocolEvent::Portal(Portal::ZonePortalEvents::WithdrawalProcessed(
                    processed,
                ))) = events.get(cursor).copied()
                else {
                    return Err(failure(
                        AdapterFindingCode::Grammar,
                        "callback deposits must be followed by WithdrawalProcessed",
                    ));
                };
                cursor += 1;
                if !processed.callbackSuccess {
                    return Err(failure(
                        AdapterFindingCode::Grammar,
                        "callback WithdrawalProcessed callbackSuccess must be true",
                    ));
                }
                effects.push(Effect::UserWithdrawalProcessed {
                    to: processed.to,
                    sender_tag: processed.senderTag,
                    token: processed.token,
                    amount: processed.amount,
                    callback_success: true,
                });
                outcomes.push(WithdrawalOutcome::UserDelivered { callback_deposits });
            }
            L1ProtocolEvent::Portal(Portal::ZonePortalEvents::WithdrawalProcessed(processed)) => {
                if !processed.callbackSuccess {
                    return Err(failure(
                        AdapterFindingCode::Grammar,
                        "delivered WithdrawalProcessed callbackSuccess must be true",
                    ));
                }
                effects.push(Effect::UserWithdrawalProcessed {
                    to: processed.to,
                    sender_tag: processed.senderTag,
                    token: processed.token,
                    amount: processed.amount,
                    callback_success: true,
                });
                outcomes.push(WithdrawalOutcome::UserDelivered {
                    callback_deposits: vec![],
                });
            }
            _ => {
                return Err(failure(
                    AdapterFindingCode::Grammar,
                    "unexpected processWithdrawals member event",
                ));
            }
        }
    }
    if cursor != events.len() {
        return Err(failure(
            AdapterFindingCode::Grammar,
            "processWithdrawals has extra or out-of-order events",
        ));
    }
    Ok(WithdrawalAdaptation { outcomes, effects })
}
