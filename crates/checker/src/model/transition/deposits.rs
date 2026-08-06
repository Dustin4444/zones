use std::num::NonZeroU64;

use alloy_primitives::{Address, B256, U256};

use super::{
    super::{
        accounting::{AccountingTransition, apply_token_accounting},
        encoding::DepositQueueMember,
        input::{AuthenticatedDepositOutcome, TokenEnable, ZoneDepositPrefixInput},
        output::{
            ExpectedDepositFailed, ExpectedDepositOutcome, ExpectedDepositProcessed,
            ExpectedOrdinaryDepositFailure, ExpectedTokenEnable, ExpectedWithdrawalBounceBack,
            ExpectedWithdrawalRequested, ExpectedZoneDepositPrefix,
        },
        ownership::{
            DepositId, DepositOwner, FailedDepositPendingWithdrawal, FallbackId, FallbackOwner,
            InboxRefundId, InboxRefundOwner, PendingWithdrawal, WithdrawalId, WithdrawalOwner,
        },
        state::{TokenPhase, ZoneProcessedDepositCursor},
    },
    DepositKind, DepositOutcomeKind, ModelError, ModelTransition, queue_member,
};

pub(super) fn apply_zone_prefix(
    candidate: &mut ModelTransition<'_>,
    input: &ZoneDepositPrefixInput,
) -> Result<ExpectedZoneDepositPrefix, ModelError> {
    if input.deposits().len() != input.outcomes().len() {
        return Err(ModelError::DepositOutcomeCountMismatch {
            deposits: input.deposits().len(),
            outcomes: input.outcomes().len(),
        });
    }

    let mut expected_token_enables = Vec::with_capacity(input.enabled_tokens().len());
    for enable in input.enabled_tokens() {
        enable_zone_token(candidate, enable)?;
        expected_token_enables.push(ExpectedTokenEnable::new(
            enable.token(),
            enable.name(),
            enable.symbol(),
            enable.currency(),
        ));
    }

    let mut expected_outcomes = Vec::with_capacity(input.deposits().len());
    for (member, outcome) in input.deposits().iter().zip(input.outcomes()) {
        expected_outcomes.push(consume_one(candidate, member, *outcome)?);
    }

    Ok(ExpectedZoneDepositPrefix::new(
        expected_token_enables,
        expected_outcomes,
        input.deposits().len(),
        candidate.zone().processed_deposit_cursor,
    ))
}

fn enable_zone_token(
    candidate: &mut ModelTransition<'_>,
    enable: &TokenEnable,
) -> Result<(), ModelError> {
    let mut token =
        candidate
            .token(enable.token())
            .cloned()
            .ok_or(ModelError::TokenNotPortalEnabled {
                token: enable.token(),
            })?;
    // The exact per-block enable vector can contain only tokens inserted by
    // this candidate's Portal operations: existing/duplicate tokens fail at
    // insertion, and insertion initializes supply to zero.
    token.phase = TokenPhase::ZoneEnabled;
    candidate.set_token(enable.token(), token);
    Ok(())
}

fn consume_one(
    candidate: &mut ModelTransition<'_>,
    supplied: &DepositQueueMember,
    outcome: AuthenticatedDepositOutcome,
) -> Result<ExpectedDepositOutcome, ModelError> {
    let processed = candidate.zone().processed_deposit_cursor;
    let number = processed
        .number()
        .checked_add(1)
        .ok_or(ModelError::ProcessedDepositNumberOverflow)?;
    let portal = candidate
        .portal()
        .created()
        .ok_or(ModelError::PortalNotCreated)?;
    let id = DepositId {
        portal: portal.identity.portal(),
        deposit_number: NonZeroU64::new(number)
            .expect("checked increment from a u64 cursor is nonzero"),
    };
    let owner = candidate
        .pending_deposit(id)
        .cloned()
        .ok_or(ModelError::PendingDepositMissing { number })?;
    let expected_member = queue_member(&owner);
    if &expected_member != supplied {
        return Err(ModelError::DepositPrefixMismatch { number });
    }
    let deposit_hash = expected_member.hash_after(processed.hash());

    let expected = match (owner, outcome) {
        (
            DepositOwner::PendingOrdinary { preimage },
            AuthenticatedDepositOutcome::OrdinaryMinted { .. },
        ) => ordinary_minted(candidate, deposit_hash, preimage)?,
        (
            DepositOwner::PendingOrdinary { preimage },
            AuthenticatedDepositOutcome::OrdinaryFailed,
        ) => ordinary_failed(candidate, id, deposit_hash, preimage)?,
        (
            DepositOwner::PendingWithdrawalBounceBack {
                withdrawal,
                preimage,
            },
            AuthenticatedDepositOutcome::WithdrawalBounceBackMinted { recipient },
        ) => withdrawal_bounce_back(
            candidate,
            id,
            withdrawal,
            preimage,
            BounceBackDisposition::Minted { recipient },
        )?,
        (
            DepositOwner::PendingWithdrawalBounceBack {
                withdrawal,
                preimage,
            },
            AuthenticatedDepositOutcome::WithdrawalBounceBackPending { recipient },
        ) => withdrawal_bounce_back(
            candidate,
            id,
            withdrawal,
            preimage,
            BounceBackDisposition::Pending { recipient },
        )?,
        (owner, outcome) => {
            return Err(ModelError::DepositOutcomeKindMismatch {
                number,
                expected: owner_kind(&owner),
                actual: outcome_kind(outcome),
            });
        }
    };

    candidate.set_pending_deposit(id, None);
    let mut zone = candidate.zone().clone();
    zone.processed_deposit_cursor = ZoneProcessedDepositCursor::new(deposit_hash, number);
    candidate.set_zone(zone);
    Ok(expected)
}

fn ordinary_minted(
    candidate: &mut ModelTransition<'_>,
    deposit_hash: B256,
    preimage: super::super::encoding::OrdinaryDeposit,
) -> Result<ExpectedDepositOutcome, ModelError> {
    let mut token = require_zone_token(candidate, preimage.token())?;
    token.accounting = apply_token_accounting(
        Some(token.accounting),
        AccountingTransition::OrdinaryDepositMinted {
            amount: U256::from(preimage.amount()),
        },
    )?;
    candidate.set_token(preimage.token(), token);
    Ok(ExpectedDepositOutcome::OrdinaryMinted(
        ExpectedDepositProcessed::new(
            deposit_hash,
            preimage.sender(),
            preimage.token(),
            preimage.amount(),
        ),
    ))
}

fn ordinary_failed(
    candidate: &mut ModelTransition<'_>,
    deposit: DepositId,
    deposit_hash: B256,
    preimage: super::super::encoding::OrdinaryDeposit,
) -> Result<ExpectedDepositOutcome, ModelError> {
    let mut token = require_zone_token(candidate, preimage.token())?;
    token.accounting = apply_token_accounting(
        Some(token.accounting),
        AccountingTransition::OrdinaryDepositFailed,
    )?;

    let mut zone = candidate.zone().clone();
    let withdrawal_index = zone.next_withdrawal_index;
    zone.next_withdrawal_index = withdrawal_index
        .checked_add(1)
        .ok_or(ModelError::WithdrawalIndexOverflow)?;
    let withdrawal = WithdrawalId {
        zone_id: candidate
            .portal()
            .created()
            .ok_or(ModelError::PortalNotCreated)?
            .identity()
            .zone_id(),
        withdrawal_index,
    };
    if candidate.withdrawal(withdrawal).is_some() {
        return Err(ModelError::WithdrawalOwnerCollision { withdrawal_index });
    }
    let pending = FailedDepositPendingWithdrawal::from_failed_deposit(deposit, preimage.clone());
    candidate.set_withdrawal(
        withdrawal,
        Some(WithdrawalOwner::Pending(PendingWithdrawal::FailedDeposit(
            pending,
        ))),
    );
    candidate.set_token(preimage.token(), token);
    candidate.set_zone(zone);

    Ok(ExpectedDepositOutcome::OrdinaryFailed(Box::new(
        ExpectedOrdinaryDepositFailure::new(
            ExpectedWithdrawalRequested::for_failed_deposit(withdrawal, &preimage),
            ExpectedDepositFailed::from_ordinary(deposit_hash, &preimage),
        ),
    )))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BounceBackDisposition {
    Minted { recipient: Address },
    Pending { recipient: Address },
}

impl BounceBackDisposition {
    const fn recipient(self) -> Address {
        match self {
            Self::Minted { recipient } | Self::Pending { recipient } => recipient,
        }
    }
}

fn withdrawal_bounce_back(
    candidate: &mut ModelTransition<'_>,
    deposit: DepositId,
    withdrawal: WithdrawalId,
    preimage: super::super::encoding::WithdrawalBounceBackDeposit,
    disposition: BounceBackDisposition,
) -> Result<ExpectedDepositOutcome, ModelError> {
    let recipient = disposition.recipient();
    if recipient.is_zero() {
        return Err(ModelError::ZeroBounceBackRecipient {
            withdrawal_index: withdrawal.withdrawal_index,
        });
    }
    let fallback_id = FallbackId {
        zone_id: withdrawal.zone_id,
        fallback_nonce: preimage.fallback_nonce(),
    };
    let fallback =
        candidate
            .fallback_owner(fallback_id)
            .ok_or(ModelError::FallbackOwnerMissing {
                fallback_nonce: preimage.fallback_nonce().get(),
            })?;
    let FallbackOwner::BounceBackQueued {
        withdrawal: fallback_withdrawal,
        token,
        amount,
        deposit: queued_deposit,
    } = fallback
    else {
        return Err(ModelError::FallbackOwnerMismatch {
            fallback_nonce: preimage.fallback_nonce().get(),
        });
    };
    if *fallback_withdrawal != withdrawal
        || *token != preimage.token()
        || *amount != preimage.amount()
        || *queued_deposit != deposit
    {
        return Err(ModelError::FallbackOwnerMismatch {
            fallback_nonce: preimage.fallback_nonce().get(),
        });
    }

    let mut token_state = require_zone_token(candidate, preimage.token())?;
    let expected =
        match disposition {
            BounceBackDisposition::Minted { .. } => {
                token_state.accounting = apply_token_accounting(
                    Some(token_state.accounting),
                    AccountingTransition::WithdrawalBounceBackMinted {
                        amount: U256::from(preimage.amount().get()),
                    },
                )?;
                ExpectedDepositOutcome::WithdrawalBounceBackMinted(
                    ExpectedWithdrawalBounceBack::new(preimage.token(), preimage.amount().get()),
                )
            }
            BounceBackDisposition::Pending { .. } => {
                token_state.accounting = apply_token_accounting(
                    Some(token_state.accounting),
                    AccountingTransition::WithdrawalBounceBackRefundPending,
                )?;
                let refund = InboxRefundId {
                    token: preimage.token(),
                    recipient,
                    user_withdrawal: withdrawal,
                };
                if candidate.inbox_refund(refund).is_some() {
                    return Err(ModelError::InboxRefundCollision {
                        withdrawal_index: withdrawal.withdrawal_index,
                    });
                }
                candidate.set_inbox_refund(
                    refund,
                    Some(InboxRefundOwner::Pending {
                        amount: preimage.amount(),
                    }),
                );
                ExpectedDepositOutcome::WithdrawalBounceBackPending(
                    ExpectedWithdrawalBounceBack::new(preimage.token(), preimage.amount().get()),
                )
            }
        };
    candidate.set_fallback_owner(fallback_id, None);
    candidate.set_token(preimage.token(), token_state);

    Ok(expected)
}

fn require_zone_token(
    candidate: &ModelTransition<'_>,
    token: Address,
) -> Result<super::super::state::TokenState, ModelError> {
    let state = candidate
        .token(token)
        .cloned()
        .ok_or(ModelError::TokenNotPortalEnabled { token })?;
    if !state.is_zone_enabled() {
        return Err(ModelError::TokenNotZoneEnabled { token });
    }
    Ok(state)
}

fn owner_kind(owner: &DepositOwner) -> DepositKind {
    match owner {
        DepositOwner::PendingOrdinary { .. } => DepositKind::Ordinary,
        DepositOwner::PendingWithdrawalBounceBack { .. } => DepositKind::WithdrawalBounceBack,
    }
}

fn outcome_kind(outcome: AuthenticatedDepositOutcome) -> DepositOutcomeKind {
    match outcome {
        AuthenticatedDepositOutcome::OrdinaryMinted { .. } => DepositOutcomeKind::OrdinaryMinted,
        AuthenticatedDepositOutcome::OrdinaryFailed => DepositOutcomeKind::OrdinaryFailed,
        AuthenticatedDepositOutcome::WithdrawalBounceBackMinted { .. } => {
            DepositOutcomeKind::WithdrawalBounceBackMinted
        }
        AuthenticatedDepositOutcome::WithdrawalBounceBackPending { .. } => {
            DepositOutcomeKind::WithdrawalBounceBackPending
        }
    }
}
