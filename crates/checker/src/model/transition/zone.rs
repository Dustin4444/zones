use std::num::NonZeroU64;

use alloy_primitives::U256;

use super::{ModelError, ModelTransition, refunds, require_zone_token};
use crate::model::{
    accounting::{AccountingTransition, apply_token_accounting},
    encoding::UserWithdrawalIdentity,
    fees::withdrawal_fee,
    input::{UserWithdrawalInput, ZoneOperation},
    output::{ExpectedWithdrawalRequested, ExpectedZoneOperation},
    ownership::{
        FallbackId, FallbackOwner, PendingWithdrawal, UserPendingWithdrawal, WithdrawalId,
        WithdrawalOwner,
    },
};

pub(super) fn apply_operations(
    candidate: &mut ModelTransition<'_>,
    operations: &[ZoneOperation],
) -> Result<Vec<ExpectedZoneOperation>, ModelError> {
    let mut withdrawals_this_block = 0_u32;
    let mut expected = Vec::new();

    for operation in operations {
        match operation {
            ZoneOperation::TempoGasRateUpdated(tempo_gas_rate) => {
                let mut zone = candidate.zone().clone();
                zone.config.tempo_gas_rate = *tempo_gas_rate;
                candidate.set_zone(zone);
            }
            ZoneOperation::MaxWithdrawalsPerBlockUpdated(max_withdrawals_per_block) => {
                let mut zone = candidate.zone().clone();
                zone.config.max_withdrawals_per_block = *max_withdrawals_per_block;
                candidate.set_zone(zone);
            }
            ZoneOperation::UserWithdrawalAccepted(input) => {
                expected.push(ExpectedZoneOperation::WithdrawalRequested(Box::new(
                    accept_user_withdrawal(candidate, input, &mut withdrawals_this_block)?,
                )));
            }
            ZoneOperation::InboxRefundClaimed(input) => {
                expected.push(ExpectedZoneOperation::RefundClaimed(refunds::claim_inbox(
                    candidate, *input,
                )?));
            }
        }
    }

    Ok(expected)
}

fn accept_user_withdrawal(
    candidate: &mut ModelTransition<'_>,
    input: &UserWithdrawalInput,
    withdrawals_this_block: &mut u32,
) -> Result<ExpectedWithdrawalRequested, ModelError> {
    let mut zone = candidate.zone().clone();
    let limit = zone.config.max_withdrawals_per_block;
    if limit != 0 && *withdrawals_this_block >= limit {
        return Err(ModelError::WithdrawalBlockCapExceeded { limit });
    }

    let request = input.request();
    let mut token = require_zone_token(candidate, request.token())?;
    let identity = UserWithdrawalIdentity::new(
        input.sender(),
        input.containing_transaction_hash(),
        NonZeroU64::new(
            zone.last_fallback_nonce
                .checked_add(1)
                .ok_or(ModelError::FallbackNonceOverflow)?,
        )
        .expect("checked increment from a u64 nonce is nonzero"),
    )?;
    let withdrawal = WithdrawalId {
        zone_id: candidate.portal().zone_id(),
        withdrawal_index: zone.next_withdrawal_index,
    };
    let fallback = FallbackId {
        zone_id: withdrawal.zone_id,
        fallback_nonce: identity.fallback_nonce(),
    };
    require_open_owner_slots(candidate, withdrawal, fallback)?;
    let pending = UserPendingWithdrawal::new(identity, request.clone(), input.reveal_to().clone())?;

    let fee = withdrawal_fee(request.gas_limit(), zone.config.tempo_gas_rate)?;
    token.accounting = apply_token_accounting(
        Some(token.accounting),
        AccountingTransition::UserWithdrawalAccepted {
            amount: U256::from(request.principal().get()),
            fee: U256::from(fee),
        },
    )?;

    if limit != 0 {
        *withdrawals_this_block += 1;
    }
    zone.next_withdrawal_index = zone
        .next_withdrawal_index
        .checked_add(1)
        .ok_or(ModelError::WithdrawalIndexOverflow)?;
    zone.last_fallback_nonce = identity.fallback_nonce().get();

    let expected = ExpectedWithdrawalRequested::for_user(
        withdrawal,
        identity,
        request,
        fee,
        input.reveal_to().clone(),
    );
    candidate.set_token(request.token(), token);
    candidate.set_withdrawal(
        withdrawal,
        Some(WithdrawalOwner::Pending(PendingWithdrawal::User(pending))),
    );
    candidate.set_fallback_owner(
        fallback,
        Some(FallbackOwner::Held {
            withdrawal,
            token: request.token(),
            amount: request.principal(),
        }),
    );
    candidate.set_zone(zone);
    Ok(expected)
}

fn require_open_owner_slots(
    candidate: &ModelTransition<'_>,
    withdrawal: WithdrawalId,
    fallback: FallbackId,
) -> Result<(), ModelError> {
    if candidate.withdrawal(withdrawal).is_some() {
        return Err(ModelError::WithdrawalOwnerCollision {
            withdrawal_index: withdrawal.withdrawal_index,
        });
    }
    if candidate.fallback_owner(fallback).is_some() {
        return Err(ModelError::FallbackOwnerCollision {
            fallback_nonce: fallback.fallback_nonce.get(),
        });
    }
    Ok(())
}
