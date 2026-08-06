//! Shared withdrawal-event projection.

use crate::model::events::Outbox;

use super::super::{ObservedWithdrawalRequested, ObservedZoneEventPosition};

pub(super) fn observed_withdrawal(
    position: ObservedZoneEventPosition,
    event: &Outbox::WithdrawalRequested,
) -> ObservedWithdrawalRequested {
    ObservedWithdrawalRequested {
        position,
        withdrawal_index: event.withdrawalIndex,
        sender: event.sender,
        token: event.token,
        to: event.to,
        amount: event.amount,
        fee: event.fee,
        memo: event.memo,
        gas_limit: event.gasLimit,
        fallback_nonce: event.fallbackNonce,
        data: event.data.clone(),
        reveal_to: event.revealTo.clone(),
    }
}
