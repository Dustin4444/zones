use std::num::NonZeroU64;

use alloy_primitives::Address;

use super::{AuthoritativeStateError, OwnerKind};
use crate::model::{
    ownership::{
        DepositId, DepositOwner, FallbackId, FallbackOwner, PendingWithdrawal, WithdrawalId,
        WithdrawalIdentity, WithdrawalOwner,
    },
    state::{ModelState, TokenPhase},
};

pub(super) fn validate_deposits(
    state: &ModelState,
    portal: Address,
    portal_deposit_number: u64,
) -> Result<(), AuthoritativeStateError> {
    let zone_processed_number = state.zone().processed_deposit_cursor().number();
    for (id, owner) in state.pending_deposits() {
        require_portal(OwnerKind::PendingDeposit, portal, id.portal)?;
        let deposit_number = id.deposit_number.get();
        if deposit_number > portal_deposit_number {
            return Err(AuthoritativeStateError::DepositBeyondPortalCursor {
                deposit_number,
                portal_deposit_number,
            });
        }
        if deposit_number <= zone_processed_number {
            return Err(AuthoritativeStateError::PendingDepositAlreadyProcessed {
                deposit_number,
                zone_processed_number,
            });
        }
        match owner {
            DepositOwner::PendingOrdinary { preimage } => {
                require_token(state, OwnerKind::PendingDeposit, preimage.token())?;
                if preimage.tempo_refund_recipient().is_zero() {
                    return Err(AuthoritativeStateError::ZeroTempoRefundRecipient {
                        deposit_number,
                    });
                }
            }
            DepositOwner::PendingWithdrawalBounceBack {
                withdrawal,
                preimage,
            } => {
                require_zone(
                    OwnerKind::PendingDeposit,
                    state.portal().zone_id(),
                    withdrawal.zone_id,
                )?;
                require_zone_token(state, OwnerKind::PendingDeposit, preimage.token())?;
                let fallback = FallbackId {
                    zone_id: withdrawal.zone_id,
                    fallback_nonce: preimage.fallback_nonce(),
                };
                match state.fallback_owners.get(&fallback) {
                    None => {
                        return Err(AuthoritativeStateError::BounceBackFallbackMissing {
                            deposit_number,
                        });
                    }
                    Some(FallbackOwner::BounceBackQueued {
                        withdrawal: owned_withdrawal,
                        token,
                        amount,
                        deposit,
                    }) if *owned_withdrawal == *withdrawal
                        && *token == preimage.token()
                        && *amount == preimage.amount()
                        && *deposit == *id => {}
                    Some(_) => {
                        return Err(AuthoritativeStateError::BounceBackFallbackMismatch {
                            deposit_number,
                        });
                    }
                }
            }
        }
    }
    let mut expected_hash = state.zone().processed_deposit_cursor().hash();
    let mut last_deposit_number = zone_processed_number;
    for (id, owner) in state.pending_deposits() {
        let expected_deposit_number = last_deposit_number.checked_add(1).ok_or(
            AuthoritativeStateError::DepositBeyondPortalCursor {
                deposit_number: id.deposit_number.get(),
                portal_deposit_number,
            },
        )?;
        if id.deposit_number.get() != expected_deposit_number {
            return Err(AuthoritativeStateError::PendingDepositMissing {
                deposit_number: expected_deposit_number,
            });
        }
        expected_hash = owner.queue_member().hash_after(expected_hash);
        last_deposit_number = expected_deposit_number;
    }
    if last_deposit_number != portal_deposit_number {
        return Err(AuthoritativeStateError::PendingDepositMissing {
            deposit_number: last_deposit_number
                .checked_add(1)
                .expect("a missing deposit before the Portal cursor fits u64"),
        });
    }
    let actual_hash = state
        .portal()
        .created()
        .expect("deposit validation requires a created Portal")
        .deposit_cursor()
        .hash();
    if expected_hash != actual_hash {
        return Err(AuthoritativeStateError::PortalDepositCommitmentMismatch {
            deposit_number: portal_deposit_number,
            expected: expected_hash,
            actual: actual_hash,
        });
    }
    Ok(())
}

pub(super) fn validate_withdrawals(
    state: &ModelState,
    zone_id: u32,
    zone_processed_deposit_number: u64,
) -> Result<(), AuthoritativeStateError> {
    for (id, owner) in state.withdrawals() {
        require_zone(OwnerKind::Withdrawal, zone_id, id.zone_id)?;
        require_withdrawal_origin(state, id.withdrawal_index)?;
        let token = withdrawal_token(owner);
        require_zone_token(state, OwnerKind::Withdrawal, token)?;

        match owner {
            WithdrawalOwner::Pending(pending) => {
                if id.withdrawal_index < state.zone().batch_start().first_withdrawal_index() {
                    return Err(AuthoritativeStateError::PendingWithdrawalBeforeBatchStart {
                        withdrawal_index: id.withdrawal_index,
                        batch_start_index: state.zone().batch_start().first_withdrawal_index(),
                    });
                }
                validate_pending_withdrawal(state, *id, pending, zone_processed_deposit_number)?;
            }
            WithdrawalOwner::Finalized(finalized) => match finalized.identity() {
                WithdrawalIdentity::User(identity) => require_user_fallback(
                    state,
                    *id,
                    identity.fallback_nonce().get(),
                    token,
                    finalized.preimage().amount(),
                )?,
                WithdrawalIdentity::FailedDeposit { deposit } => {
                    if finalized.preimage().to().is_zero() {
                        return Err(
                            AuthoritativeStateError::ZeroFinalizedFailedDepositRecipient {
                                withdrawal_index: id.withdrawal_index,
                            },
                        );
                    }
                    require_deposit_origin(
                        state,
                        OwnerKind::Withdrawal,
                        deposit,
                        zone_processed_deposit_number,
                    )?;
                }
            },
        }
    }
    let first = state.zone().batch_start().first_withdrawal_index();
    let next = state.zone().next_withdrawal_index();
    let mut expected_withdrawal_index = first;
    for (id, owner) in state.withdrawals() {
        if id.withdrawal_index < first {
            continue;
        }
        if id.withdrawal_index != expected_withdrawal_index {
            return Err(AuthoritativeStateError::PendingWithdrawalMissing {
                withdrawal_index: expected_withdrawal_index,
            });
        }
        match owner {
            WithdrawalOwner::Finalized(_) => {
                return Err(AuthoritativeStateError::CurrentWithdrawalAlreadyFinalized {
                    withdrawal_index: id.withdrawal_index,
                });
            }
            WithdrawalOwner::Pending(_) => {}
        }
        expected_withdrawal_index = expected_withdrawal_index
            .checked_add(1)
            .expect("a current withdrawal index is below the exclusive next index");
    }
    if expected_withdrawal_index != next {
        return Err(AuthoritativeStateError::PendingWithdrawalMissing {
            withdrawal_index: expected_withdrawal_index,
        });
    }
    Ok(())
}

fn validate_pending_withdrawal(
    state: &ModelState,
    id: WithdrawalId,
    pending: &PendingWithdrawal,
    zone_processed_deposit_number: u64,
) -> Result<(), AuthoritativeStateError> {
    match pending {
        PendingWithdrawal::User(user) => {
            let (identity, request, _) = user.parts();
            require_user_fallback(
                state,
                id,
                identity.fallback_nonce().get(),
                request.token(),
                request.principal().get(),
            )
        }
        PendingWithdrawal::FailedDeposit(failed) => {
            let (deposit, _, recipient, _) = failed.parts();
            if recipient.is_zero() {
                return Err(AuthoritativeStateError::ZeroPendingFailedDepositRecipient {
                    withdrawal_index: id.withdrawal_index,
                });
            }
            require_deposit_origin(
                state,
                OwnerKind::Withdrawal,
                deposit,
                zone_processed_deposit_number,
            )
        }
    }
}

fn require_user_fallback(
    state: &ModelState,
    withdrawal: WithdrawalId,
    fallback_nonce: u64,
    token: Address,
    amount: u128,
) -> Result<(), AuthoritativeStateError> {
    if fallback_nonce > state.zone().last_fallback_nonce() {
        return Err(AuthoritativeStateError::FallbackBeyondLastNonce {
            fallback_nonce,
            last_fallback_nonce: state.zone().last_fallback_nonce(),
        });
    }
    let fallback_id = FallbackId {
        zone_id: withdrawal.zone_id,
        fallback_nonce: NonZeroU64::new(fallback_nonce)
            .expect("user withdrawal identity owns a nonzero fallback nonce"),
    };
    match state.fallback_owners.get(&fallback_id) {
        None => Err(AuthoritativeStateError::UserFallbackMissing {
            withdrawal_index: withdrawal.withdrawal_index,
            fallback_nonce,
        }),
        Some(FallbackOwner::Held {
            withdrawal: owned_withdrawal,
            token: owned_token,
            amount: owned_amount,
        }) if *owned_withdrawal == withdrawal
            && *owned_token == token
            && owned_amount.get() == amount =>
        {
            Ok(())
        }
        Some(_) => Err(AuthoritativeStateError::UserFallbackMismatch {
            withdrawal_index: withdrawal.withdrawal_index,
            fallback_nonce,
        }),
    }
}

pub(super) fn validate_fallbacks(
    state: &ModelState,
    zone_id: u32,
) -> Result<(), AuthoritativeStateError> {
    for (id, owner) in &state.fallback_owners {
        require_zone(OwnerKind::Fallback, zone_id, id.zone_id)?;
        let nonce = id.fallback_nonce.get();
        if nonce > state.zone().last_fallback_nonce() {
            return Err(AuthoritativeStateError::FallbackBeyondLastNonce {
                fallback_nonce: nonce,
                last_fallback_nonce: state.zone().last_fallback_nonce(),
            });
        }
        let (withdrawal, token) = match owner {
            FallbackOwner::Held {
                withdrawal, token, ..
            }
            | FallbackOwner::BounceBackQueued {
                withdrawal, token, ..
            } => (withdrawal, *token),
        };
        require_zone(OwnerKind::Fallback, zone_id, withdrawal.zone_id)?;
        require_withdrawal_origin(state, withdrawal.withdrawal_index)?;
        require_zone_token(state, OwnerKind::Fallback, token)?;

        match owner {
            FallbackOwner::Held {
                withdrawal,
                token,
                amount,
            } => validate_held_fallback(state, *id, *withdrawal, *token, amount.get())?,
            FallbackOwner::BounceBackQueued {
                withdrawal,
                token,
                amount,
                deposit,
            } => validate_queued_fallback(state, *id, *withdrawal, *token, amount.get(), *deposit)?,
        }
    }
    Ok(())
}

fn validate_held_fallback(
    state: &ModelState,
    fallback: FallbackId,
    withdrawal: WithdrawalId,
    token: Address,
    amount: u128,
) -> Result<(), AuthoritativeStateError> {
    let Some(owner) = state.withdrawal(withdrawal) else {
        return Err(AuthoritativeStateError::HeldFallbackWithdrawalMissing {
            fallback_nonce: fallback.fallback_nonce.get(),
        });
    };
    let (identity, owned_token, owned_amount) = match owner {
        WithdrawalOwner::Pending(PendingWithdrawal::User(user)) => {
            let (identity, request, _) = user.parts();
            (identity, request.token(), request.principal().get())
        }
        WithdrawalOwner::Finalized(finalized) => match finalized.identity() {
            WithdrawalIdentity::User(identity) => (
                identity,
                finalized.preimage().token(),
                finalized.preimage().amount(),
            ),
            WithdrawalIdentity::FailedDeposit { .. } => {
                return Err(AuthoritativeStateError::HeldFallbackWithdrawalMismatch {
                    fallback_nonce: fallback.fallback_nonce.get(),
                });
            }
        },
        WithdrawalOwner::Pending(PendingWithdrawal::FailedDeposit(_)) => {
            return Err(AuthoritativeStateError::HeldFallbackWithdrawalMismatch {
                fallback_nonce: fallback.fallback_nonce.get(),
            });
        }
    };
    if identity.fallback_nonce() != fallback.fallback_nonce
        || owned_token != token
        || owned_amount != amount
    {
        return Err(AuthoritativeStateError::HeldFallbackWithdrawalMismatch {
            fallback_nonce: fallback.fallback_nonce.get(),
        });
    }
    Ok(())
}

fn validate_queued_fallback(
    state: &ModelState,
    fallback: FallbackId,
    withdrawal: WithdrawalId,
    token: Address,
    amount: u128,
    deposit: DepositId,
) -> Result<(), AuthoritativeStateError> {
    let Some(owner) = state.pending_deposits.get(&deposit) else {
        return Err(AuthoritativeStateError::QueuedFallbackDepositMissing {
            fallback_nonce: fallback.fallback_nonce.get(),
            deposit_number: deposit.deposit_number.get(),
        });
    };
    match owner {
        DepositOwner::PendingWithdrawalBounceBack {
            withdrawal: owned_withdrawal,
            preimage,
        } if *owned_withdrawal == withdrawal
            && preimage.fallback_nonce() == fallback.fallback_nonce
            && preimage.token() == token
            && preimage.amount().get() == amount =>
        {
            Ok(())
        }
        DepositOwner::PendingOrdinary { .. } | DepositOwner::PendingWithdrawalBounceBack { .. } => {
            Err(AuthoritativeStateError::QueuedFallbackDepositMismatch {
                fallback_nonce: fallback.fallback_nonce.get(),
                deposit_number: deposit.deposit_number.get(),
            })
        }
    }
}

pub(super) fn require_token(
    state: &ModelState,
    owner: OwnerKind,
    token: Address,
) -> Result<(), AuthoritativeStateError> {
    if state.token(token).is_some() {
        Ok(())
    } else {
        Err(AuthoritativeStateError::MissingOwnerToken { owner, token })
    }
}

pub(super) fn require_zone_token(
    state: &ModelState,
    owner: OwnerKind,
    token: Address,
) -> Result<(), AuthoritativeStateError> {
    let token_state = state
        .token(token)
        .ok_or(AuthoritativeStateError::MissingOwnerToken { owner, token })?;
    if token_state.phase() == TokenPhase::ZoneEnabled {
        Ok(())
    } else {
        Err(AuthoritativeStateError::OwnerTokenNotZoneEnabled { owner, token })
    }
}

pub(super) fn require_portal(
    owner: OwnerKind,
    expected: Address,
    actual: Address,
) -> Result<(), AuthoritativeStateError> {
    if expected == actual {
        Ok(())
    } else {
        Err(AuthoritativeStateError::OwnerPortalMismatch {
            owner,
            expected,
            actual,
        })
    }
}

pub(super) fn require_zone(
    owner: OwnerKind,
    expected: u32,
    actual: u32,
) -> Result<(), AuthoritativeStateError> {
    if expected == actual {
        Ok(())
    } else {
        Err(AuthoritativeStateError::OwnerZoneMismatch {
            owner,
            expected,
            actual,
        })
    }
}

pub(super) fn require_deposit_origin(
    state: &ModelState,
    owner: OwnerKind,
    deposit: DepositId,
    zone_processed_number: u64,
) -> Result<(), AuthoritativeStateError> {
    require_portal(owner, state.portal().identity().portal(), deposit.portal)?;
    let deposit_number = deposit.deposit_number.get();
    if deposit_number > zone_processed_number {
        return Err(
            AuthoritativeStateError::DepositOriginBeyondProcessedCursor {
                deposit_number,
                zone_processed_number,
            },
        );
    }
    Ok(())
}

pub(super) fn require_withdrawal_origin(
    state: &ModelState,
    withdrawal_index: u64,
) -> Result<(), AuthoritativeStateError> {
    if withdrawal_index >= state.zone().next_withdrawal_index() {
        Err(AuthoritativeStateError::WithdrawalOriginBeyondNext {
            withdrawal_index,
            next_withdrawal_index: state.zone().next_withdrawal_index(),
        })
    } else {
        Ok(())
    }
}

fn withdrawal_token(owner: &WithdrawalOwner) -> Address {
    match owner {
        WithdrawalOwner::Pending(PendingWithdrawal::User(user)) => user.parts().1.token(),
        WithdrawalOwner::Pending(PendingWithdrawal::FailedDeposit(failed)) => failed.parts().1,
        WithdrawalOwner::Finalized(finalized) => finalized.preimage().token(),
    }
}
