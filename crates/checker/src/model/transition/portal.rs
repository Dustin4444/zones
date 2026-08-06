use std::num::NonZeroU64;

use alloy_primitives::U256;

use super::{
    super::{
        accounting::{AccountingTransition, apply_token_accounting},
        encoding::DepositQueueMember,
        input::{ImportedTempoOperation, PortalCreationInput, TokenEnable},
        output::{ExpectedDepositAppend, ExpectedImportedTempoBlock},
        ownership::{DepositId, DepositOwner, FallbackId, FallbackOwner},
        state::{
            CreatedPortalState, PortalConfig, PortalDepositCursor, PortalLifecycle,
            PortalSettlementState, TokenPhase, TokenState, portal_address_for_zone,
        },
    },
    ModelError, ModelTransition, processing, refunds, submission,
};

pub(super) fn apply_operation(
    candidate: &mut ModelTransition<'_>,
    operation: &ImportedTempoOperation,
    block_base_fee: U256,
    block_token_enables: &mut Vec<TokenEnable>,
    expected: &mut ExpectedImportedTempoBlock,
) -> Result<(), ModelError> {
    match operation {
        ImportedTempoOperation::Create(input) => {
            apply_creation(candidate, input, block_token_enables)
        }
        ImportedTempoOperation::TokenEnabled(input) => {
            require_created(candidate)?;
            enable_portal_token(candidate, input, block_token_enables)
        }
        ImportedTempoOperation::BouncebackGasUpdated(bounceback_gas) => {
            let mut portal = require_created(candidate)?.clone();
            portal.config.bounceback_gas = *bounceback_gas;
            candidate.set_portal(PortalLifecycle::Created(Box::new(portal)));
            Ok(())
        }
        ImportedTempoOperation::OrdinaryDepositAppended(input) => {
            let append = append_ordinary(candidate, input)?;
            expected.push_deposit_append(append);
            Ok(())
        }
        ImportedTempoOperation::BatchSubmitted(input) => {
            expected.push_batch_submission(submission::apply(candidate, input)?);
            Ok(())
        }
        ImportedTempoOperation::WithdrawalsProcessed(input) => {
            expected.push_withdrawal_processing(processing::apply(
                candidate,
                input,
                block_base_fee,
            )?);
            Ok(())
        }
        ImportedTempoOperation::PortalRefundClaimed(input) => {
            expected.push_refund_claim(refunds::claim_portal(candidate, *input)?);
            Ok(())
        }
    }
}

fn apply_creation(
    candidate: &mut ModelTransition<'_>,
    input: &PortalCreationInput,
    block_token_enables: &mut Vec<TokenEnable>,
) -> Result<(), ModelError> {
    let expected_identity = match candidate.portal() {
        PortalLifecycle::AwaitingCreation { expected } => *expected,
        PortalLifecycle::Created(_) => return Err(ModelError::PortalAlreadyCreated),
    };
    if expected_identity != input.identity() {
        return Err(ModelError::PortalIdentityMismatch {
            expected: expected_identity,
            actual: input.identity(),
        });
    }
    let derived = portal_address_for_zone(expected_identity.zone_id());
    if derived != expected_identity.portal() {
        return Err(ModelError::PortalAddressMismatch {
            expected: derived,
            actual: expected_identity.portal(),
        });
    }
    if input.initial_token_enable().token() != expected_identity.initial_token() {
        return Err(ModelError::InitialTokenMismatch {
            expected: expected_identity.initial_token(),
            actual: input.initial_token_enable().token(),
        });
    }

    candidate.set_portal(PortalLifecycle::Created(Box::new(CreatedPortalState {
        identity: expected_identity,
        config: PortalConfig::INITIAL,
        deposit_cursor: PortalDepositCursor::ZERO,
        settlement: PortalSettlementState::ZERO,
    })));
    enable_portal_token(candidate, input.initial_token_enable(), block_token_enables)
}

fn enable_portal_token(
    candidate: &mut ModelTransition<'_>,
    input: &TokenEnable,
    block_token_enables: &mut Vec<TokenEnable>,
) -> Result<(), ModelError> {
    require_created(candidate)?;
    if candidate.token(input.token()).is_some() {
        return Err(ModelError::TokenAlreadyEnabled {
            token: input.token(),
        });
    }
    let accounting = apply_token_accounting(None, AccountingTransition::TokenEnabled)?;
    candidate.set_token(
        input.token(),
        TokenState {
            phase: TokenPhase::PendingZoneEnable,
            accounting,
        },
    );
    block_token_enables.push(input.clone());
    Ok(())
}

pub(super) fn append_ordinary(
    candidate: &mut ModelTransition<'_>,
    input: &super::super::encoding::OrdinaryDeposit,
) -> Result<ExpectedDepositAppend, ModelError> {
    require_created(candidate)?;
    if input.tempo_refund_recipient().is_zero() {
        return Err(ModelError::ZeroTempoRefundRecipient);
    }
    let mut token =
        candidate
            .token(input.token())
            .cloned()
            .ok_or(ModelError::TokenNotPortalEnabled {
                token: input.token(),
            })?;
    let member = DepositQueueMember::Ordinary(input.clone());
    let (id, queue_hash) = append_member(candidate, &member)?;
    token.accounting = apply_token_accounting(
        Some(token.accounting),
        AccountingTransition::OrdinaryDepositMade {
            net_amount: U256::from(input.amount()),
        },
    )?;
    candidate.set_token(input.token(), token);
    candidate.set_pending_deposit(
        id,
        Some(DepositOwner::PendingOrdinary {
            preimage: input.clone(),
        }),
    );
    Ok(ExpectedDepositAppend::new(id, queue_hash))
}

pub(super) fn append_withdrawal_bounce_back(
    candidate: &mut ModelTransition<'_>,
    input: super::super::encoding::WithdrawalBounceBackDeposit,
) -> Result<ExpectedDepositAppend, ModelError> {
    let zone_id = require_created(candidate)?.identity.zone_id();
    if candidate.token(input.token()).is_none() {
        return Err(ModelError::TokenNotPortalEnabled {
            token: input.token(),
        });
    }
    let fallback_id = FallbackId {
        zone_id,
        fallback_nonce: input.fallback_nonce(),
    };
    let fallback =
        candidate
            .fallback_owner(fallback_id)
            .cloned()
            .ok_or(ModelError::FallbackOwnerMissing {
                fallback_nonce: input.fallback_nonce().get(),
            })?;
    let (withdrawal, token, amount) = match fallback {
        FallbackOwner::Held {
            withdrawal,
            token,
            amount,
        } => (withdrawal, token, amount),
        FallbackOwner::BounceBackQueued { withdrawal, .. } => {
            return Err(ModelError::WithdrawalBounceBackAlreadyPending {
                withdrawal_index: withdrawal.withdrawal_index,
            });
        }
    };
    if withdrawal.zone_id != zone_id || token != input.token() || amount != input.amount() {
        return Err(ModelError::FallbackOwnerMismatch {
            fallback_nonce: input.fallback_nonce().get(),
        });
    }
    let member = DepositQueueMember::WithdrawalBounceBack(input);
    let (id, queue_hash) = append_member(candidate, &member)?;
    candidate.set_pending_deposit(
        id,
        Some(DepositOwner::PendingWithdrawalBounceBack {
            withdrawal,
            preimage: input,
        }),
    );
    candidate.set_fallback_owner(
        fallback_id,
        Some(FallbackOwner::BounceBackQueued {
            withdrawal,
            token,
            amount,
            deposit: id,
        }),
    );
    Ok(ExpectedDepositAppend::new(id, queue_hash))
}

fn append_member(
    candidate: &mut ModelTransition<'_>,
    member: &DepositQueueMember,
) -> Result<(DepositId, alloy_primitives::B256), ModelError> {
    let mut portal = require_created(candidate)?.clone();
    let number = portal
        .deposit_cursor
        .number()
        .checked_add(1)
        .ok_or(ModelError::PortalDepositNumberOverflow)?;
    let queue_hash = member.hash_after(portal.deposit_cursor.hash());
    let id = DepositId {
        portal: portal.identity.portal(),
        deposit_number: NonZeroU64::new(number)
            .expect("checked increment from a u64 cursor is nonzero"),
    };
    if candidate.pending_deposit(id).is_some() {
        return Err(ModelError::DepositOwnerCollision { number });
    }
    portal.deposit_cursor = PortalDepositCursor::new(queue_hash, number);
    candidate.set_portal(PortalLifecycle::Created(Box::new(portal)));
    Ok((id, queue_hash))
}

pub(super) fn require_created<'a>(
    candidate: &'a ModelTransition<'_>,
) -> Result<&'a CreatedPortalState, ModelError> {
    candidate
        .portal()
        .created()
        .ok_or(ModelError::PortalNotCreated)
}
