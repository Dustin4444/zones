//! Zone transition application.

use super::*;

/// Apply Zone facts to the state produced by their imported Tempo block.
pub(crate) fn apply_zone(
    imported: ImportedCandidate,
    facts: &ZoneFacts,
) -> Result<TransitionCandidate, TransitionError> {
    if imported.token_enables != facts.enabled_tokens {
        return Err(TransitionError::TokenEnableMismatch);
    }
    if facts.deposits.len() != facts.outcomes.len() {
        return Err(TransitionError::DepositOutcomeCountMismatch);
    }
    let mut overlay = Overlay::new(&imported.state);
    let mut effects = imported.effects;
    for enable in &facts.enabled_tokens {
        let mut token = token(&overlay, enable.token)?;
        token.phase = TokenPhase::ZoneEnabled;
        overlay.set(
            StateKey::Token(enable.token),
            Some(StateValue::Token(token)),
        );
        effects.push(Effect::TokenEnabled {
            token: enable.token,
            name: enable.name.clone(),
            symbol: enable.symbol.clone(),
            currency: enable.currency.clone(),
        });
    }
    for (deposit, outcome) in facts.deposits.iter().zip(&facts.outcomes) {
        consume_deposit(&mut overlay, deposit, *outcome, &mut effects)?;
    }
    if let PortalState::Created { deposit, .. } = portal(&overlay)?
        && zone(&overlay)?.processed_deposit != deposit
    {
        return Err(TransitionError::CommitmentMismatch);
    }
    apply_zone_operations(&mut overlay, &facts.operations, &mut effects)?;
    if let Some(finalization) = &facts.finalization {
        finalize(
            &mut overlay,
            facts,
            imported.block_number,
            finalization,
            &mut effects,
        )?;
    }
    let expected_accounting = token_accounting(&overlay)?;
    let expected_state = expected_state(&overlay, imported.block_hash, imported.block_number)?;
    let zone_delta = overlay.finish();
    let mut writes = imported
        .delta
        .writes()
        .iter()
        .cloned()
        .collect::<BTreeMap<_, _>>();
    writes.extend(zone_delta.writes().iter().cloned());
    Ok(TransitionCandidate {
        delta: StateDelta::from_sorted_writes(writes.into_iter().collect()),
        expected_effects: effects,
        expected_state,
        expected_accounting,
    })
}
/// Consume the next Portal deposit using its authenticated Zone outcome.
fn consume_deposit(
    overlay: &mut Overlay<'_>,
    supplied: &Deposit,
    outcome: DepositOutcome,
    effects: &mut Vec<Effect>,
) -> Result<(), TransitionError> {
    let mut zone = zone(overlay)?;
    let number = zone
        .processed_deposit
        .number
        .checked_add(1)
        .ok_or(TransitionError::Overflow)?;
    let identity = portal(overlay)?.identity();
    let id = DepositId::new(identity.portal, number).ok_or(TransitionError::Overflow)?;
    let Some(StateValue::Deposit(owner)) = overlay.get(&StateKey::Deposit(id)) else {
        return Err(TransitionError::DepositPrefixMismatch);
    };
    let (token_address, amount, hash) = match owner {
        DepositOwner::Ordinary(expected) => {
            if supplied != &Deposit::Ordinary(expected.clone()) {
                return Err(TransitionError::DepositPrefixMismatch);
            }
            (
                expected.token,
                expected.amount,
                ordinary_deposit_hash(expected, zone.processed_deposit.hash),
            )
        }
        DepositOwner::BounceBack {
            token,
            fallback_nonce,
            amount,
            ..
        } => {
            let expected = BounceBackDeposit {
                token: *token,
                fallback_nonce: *fallback_nonce,
                amount: *amount,
            };
            if supplied != &Deposit::BounceBack(expected) {
                return Err(TransitionError::DepositPrefixMismatch);
            }
            (
                *token,
                *amount,
                bounceback_deposit_hash(expected, zone.processed_deposit.hash),
            )
        }
    };
    let owner = owner.clone();
    let mut token = token(overlay, token_address)?;
    if token.phase != TokenPhase::ZoneEnabled {
        return Err(TransitionError::TokenNotZoneEnabled(token_address));
    }
    match (owner, outcome) {
        (DepositOwner::Ordinary(expected), DepositOutcome::Minted) => {
            token.accounting.supply = token
                .accounting
                .supply
                .checked_add(U256::from(amount))
                .ok_or(TransitionError::Overflow)?;
            token.accounting.deposits = token
                .accounting
                .deposits
                .checked_sub(U256::from(amount))
                .ok_or(TransitionError::Underflow)?;
            effects.push(Effect::DepositProcessed {
                deposit_hash: hash,
                sender: expected.sender,
                token: expected.token,
                amount: expected.amount,
            });
        }
        (DepositOwner::Ordinary(expected), DepositOutcome::Failed) => {
            let withdrawal = WithdrawalId {
                zone_id: identity.zone_id,
                index: zone.next_withdrawal_index,
            };
            if overlay.get(&StateKey::Withdrawal(withdrawal)).is_some() {
                return Err(TransitionError::WithdrawalCollision);
            }
            zone.next_withdrawal_index = zone
                .next_withdrawal_index
                .checked_add(1)
                .ok_or(TransitionError::Overflow)?;
            overlay.set(
                StateKey::Withdrawal(withdrawal),
                Some(StateValue::Withdrawal(
                    WithdrawalOwner::PendingFailedDeposit {
                        deposit: id,
                        token: expected.token,
                        recipient: expected.tempo_refund_recipient,
                        amount: expected.amount,
                    },
                )),
            );
            effects.push(Effect::WithdrawalRequested {
                id: withdrawal,
                sender: Address::ZERO,
                token: expected.token,
                to: expected.tempo_refund_recipient,
                amount: expected.amount,
                fee: 0,
                memo: alloy_primitives::B256::ZERO,
                gas_limit: 0,
                fallback_nonce: 0,
                callback_data: Default::default(),
                reveal_to: Default::default(),
            });
            effects.push(Effect::DepositFailed {
                deposit_hash: hash,
                sender: expected.sender,
                token: expected.token,
                amount: expected.amount,
            });
        }
        (
            DepositOwner::BounceBack {
                withdrawal,
                token: owner_token,
                fallback_nonce,
                amount,
            },
            DepositOutcome::BounceBackMinted { recipient },
        ) => {
            let fallback = require_bounceback_owner(
                overlay,
                withdrawal,
                owner_token,
                fallback_nonce,
                amount,
                id,
            )?;
            if recipient.is_zero() {
                return Err(TransitionError::OwnerMismatch);
            }
            token.accounting.supply = token
                .accounting
                .supply
                .checked_add(U256::from(amount))
                .ok_or(TransitionError::Overflow)?;
            token.accounting.withdrawals = token
                .accounting
                .withdrawals
                .checked_sub(U256::from(amount))
                .ok_or(TransitionError::Underflow)?;
            overlay.set(StateKey::Fallback(fallback), None);
            effects.push(Effect::BounceBackMinted {
                token: owner_token,
                amount,
            });
        }
        (
            DepositOwner::BounceBack {
                withdrawal,
                token: owner_token,
                fallback_nonce,
                amount,
            },
            DepositOutcome::BounceBackPending { recipient },
        ) => {
            let fallback = require_bounceback_owner(
                overlay,
                withdrawal,
                owner_token,
                fallback_nonce,
                amount,
                id,
            )?;
            if recipient.is_zero() || amount == 0 {
                return Err(TransitionError::OwnerMismatch);
            }
            let refund = InboxRefundId {
                token: owner_token,
                recipient,
                withdrawal,
            };
            if overlay.get(&StateKey::InboxRefund(refund)).is_some() {
                return Err(TransitionError::OwnerMismatch);
            }
            overlay.set(
                StateKey::InboxRefund(refund),
                Some(StateValue::InboxRefund(RefundCredit { amount })),
            );
            overlay.set(StateKey::Fallback(fallback), None);
            effects.push(Effect::BounceBackPending {
                token: owner_token,
                amount,
            });
        }
        _ => return Err(TransitionError::DepositPrefixMismatch),
    }
    zone.processed_deposit = Cursor { hash, number };
    overlay.set(
        StateKey::Token(token_address),
        Some(StateValue::Token(token)),
    );
    overlay.set(StateKey::Deposit(id), None);
    overlay.set(StateKey::Zone, Some(StateValue::Zone(zone)));
    Ok(())
}

fn require_bounceback_owner(
    overlay: &Overlay<'_>,
    withdrawal: WithdrawalId,
    token: Address,
    fallback_nonce: NonZeroU64,
    amount: u128,
    deposit: DepositId,
) -> Result<FallbackId, TransitionError> {
    let key = FallbackId::new(withdrawal.zone_id, fallback_nonce.get())
        .ok_or(TransitionError::Overflow)?;
    match overlay.get(&StateKey::Fallback(key)) {
        Some(StateValue::Fallback(FallbackState::Queued {
            withdrawal: actual_withdrawal,
            token: actual_token,
            amount: actual_amount,
            deposit: actual_deposit,
        })) if *actual_withdrawal == withdrawal
            && *actual_token == token
            && *actual_amount == amount
            && *actual_deposit == deposit =>
        {
            Ok(key)
        }
        _ => Err(TransitionError::OwnerMismatch),
    }
}

/// Apply ordered Zone operations after deposit processing.
fn apply_zone_operations(
    overlay: &mut Overlay<'_>,
    operations: &[ZoneOperation],
    effects: &mut Vec<Effect>,
) -> Result<(), TransitionError> {
    let mut accepted = 0u32;
    for operation in operations {
        match operation {
            ZoneOperation::UpdateTempoGasRate(rate) => {
                let mut z = zone(overlay)?;
                z.tempo_gas_rate = *rate;
                overlay.set(StateKey::Zone, Some(StateValue::Zone(z)));
            }
            ZoneOperation::UpdateMaxWithdrawals(cap) => {
                let mut z = zone(overlay)?;
                z.max_withdrawals_per_block = *cap;
                overlay.set(StateKey::Zone, Some(StateValue::Zone(z)));
            }
            ZoneOperation::AcceptWithdrawal(input) => {
                let mut z = zone(overlay)?;
                let limited = z.max_withdrawals_per_block != 0;
                if limited && accepted >= z.max_withdrawals_per_block {
                    return Err(TransitionError::WithdrawalCap);
                }
                let mut t = token(overlay, input.token)?;
                if t.phase != TokenPhase::ZoneEnabled {
                    return Err(TransitionError::TokenNotZoneEnabled(input.token));
                }
                if input.amount == 0
                    || input.transaction_hash.is_zero()
                    || input.gas_limit > 10_000_000
                    || input.callback_data.len() > 1_024
                    || (!input.reveal_to.is_empty()
                        && (input.reveal_to.len() != 33
                            || !matches!(input.reveal_to[0], 0x02 | 0x03)))
                {
                    return Err(TransitionError::CommitmentMismatch);
                }
                let fee = withdrawal_fee(input.gas_limit, z.tempo_gas_rate)
                    .ok_or(TransitionError::Overflow)?;
                let burn = U256::from(input.amount)
                    .checked_add(U256::from(fee))
                    .ok_or(TransitionError::Overflow)?;
                t.accounting.supply = t
                    .accounting
                    .supply
                    .checked_sub(burn)
                    .ok_or(TransitionError::Underflow)?;
                t.accounting.withdrawals = t
                    .accounting
                    .withdrawals
                    .checked_add(U256::from(input.amount))
                    .ok_or(TransitionError::Overflow)?;
                let fallback_nonce = z
                    .last_fallback_nonce
                    .checked_add(1)
                    .ok_or(TransitionError::Overflow)?;
                let fallback = FallbackId::new(portal(overlay)?.identity().zone_id, fallback_nonce)
                    .ok_or(TransitionError::Overflow)?;
                let id = WithdrawalId {
                    zone_id: fallback.zone_id,
                    index: z.next_withdrawal_index,
                };
                if overlay.get(&StateKey::Withdrawal(id)).is_some()
                    || overlay.get(&StateKey::Fallback(fallback)).is_some()
                {
                    return Err(TransitionError::WithdrawalCollision);
                }
                let tag = sender_tag(input.sender, input.transaction_hash, fallback_nonce);
                let data = Withdrawal {
                    token: input.token,
                    sender_tag: tag,
                    to: input.to,
                    amount: input.amount,
                    memo: input.memo,
                    gas_limit: input.gas_limit,
                    fallback_nonce,
                    callback_data: input.callback_data.clone(),
                    encrypted_sender: input.reveal_to.clone(),
                };
                overlay.set(StateKey::Token(input.token), Some(StateValue::Token(t)));
                overlay.set(
                    StateKey::Withdrawal(id),
                    Some(StateValue::Withdrawal(WithdrawalOwner::PendingUser {
                        data,
                        fallback,
                    })),
                );
                overlay.set(
                    StateKey::Fallback(fallback),
                    Some(StateValue::Fallback(FallbackState::Held {
                        withdrawal: id,
                        token: input.token,
                        amount: input.amount,
                    })),
                );
                z.next_withdrawal_index = z
                    .next_withdrawal_index
                    .checked_add(1)
                    .ok_or(TransitionError::Overflow)?;
                z.last_fallback_nonce = fallback_nonce;
                overlay.set(StateKey::Zone, Some(StateValue::Zone(z)));
                if limited {
                    accepted = accepted.checked_add(1).ok_or(TransitionError::Overflow)?;
                }
                effects.push(Effect::WithdrawalRequested {
                    id,
                    sender: input.sender,
                    token: input.token,
                    to: input.to,
                    amount: input.amount,
                    fee,
                    memo: input.memo,
                    gas_limit: input.gas_limit,
                    fallback_nonce,
                    callback_data: input.callback_data.clone(),
                    reveal_to: input.reveal_to.clone(),
                });
            }
            ZoneOperation::ClaimInboxRefund(claim) => {
                claim_refund(overlay, *claim, RefundSide::Inbox, effects)?
            }
        }
    }
    Ok(())
}

/// Finalize the current Zone withdrawal suffix into its next batch.
fn finalize(
    overlay: &mut Overlay<'_>,
    facts: &ZoneFacts,
    imported_block_number: u64,
    input: &Finalization,
    effects: &mut Vec<Effect>,
) -> Result<(), TransitionError> {
    let mut z = zone(overlay)?;
    let count = z
        .next_withdrawal_index
        .checked_sub(z.batch_start.withdrawal_index)
        .ok_or(TransitionError::Underflow)?;
    if input.block_number != facts.block_number
        || input.declared_count != input.encrypted_senders.len()
        || usize::try_from(count).ok() != Some(input.declared_count)
    {
        return Err(TransitionError::CommitmentMismatch);
    }
    let zone_id = portal(overlay)?.identity().zone_id;
    let mut values = Vec::with_capacity(input.declared_count);
    for (offset, encrypted) in input.encrypted_senders.iter().enumerate() {
        let index = z
            .batch_start
            .withdrawal_index
            .checked_add(u64::try_from(offset).map_err(|_| TransitionError::Overflow)?)
            .ok_or(TransitionError::Overflow)?;
        let id = WithdrawalId { zone_id, index };
        let owner = overlay
            .get(&StateKey::Withdrawal(id))
            .cloned()
            .ok_or(TransitionError::OwnerMismatch)?;
        let (mut data, origin) = match owner {
            StateValue::Withdrawal(WithdrawalOwner::PendingUser { data, fallback }) => {
                let expected_len = if data.encrypted_sender.is_empty() {
                    0
                } else {
                    113
                };
                if encrypted.len() != expected_len {
                    return Err(TransitionError::CommitmentMismatch);
                }
                (data, WithdrawalOrigin::User { fallback })
            }
            StateValue::Withdrawal(WithdrawalOwner::PendingFailedDeposit {
                deposit,
                token,
                recipient,
                amount,
            }) => (
                Withdrawal {
                    token,
                    sender_tag: failed_deposit_sender_tag(),
                    to: recipient,
                    amount,
                    memo: alloy_primitives::B256::ZERO,
                    gas_limit: 0,
                    fallback_nonce: 0,
                    callback_data: Default::default(),
                    encrypted_sender: Default::default(),
                },
                WithdrawalOrigin::FailedDeposit { deposit },
            ),
            _ => return Err(TransitionError::OwnerMismatch),
        };
        if matches!(origin, WithdrawalOrigin::FailedDeposit { .. }) && !encrypted.is_empty() {
            return Err(TransitionError::CommitmentMismatch);
        }
        data.encrypted_sender = encrypted.clone();
        values.push(data.clone());
        overlay.set(
            StateKey::Withdrawal(id),
            Some(StateValue::Withdrawal(WithdrawalOwner::Finalized {
                data,
                origin,
            })),
        );
    }
    let index = z
        .withdrawal_batch_index
        .checked_add(1)
        .ok_or(TransitionError::Overflow)?;
    let id = BatchId::new(zone_id, index).ok_or(TransitionError::Overflow)?;
    if overlay.get(&StateKey::Batch(id)).is_some() {
        return Err(TransitionError::OwnerMismatch);
    }
    let queue_hash = withdrawal_queue_hash(&values);
    let boundary = BatchBoundary {
        first_parent: z.batch_start.parent_hash,
        final_block: facts.block_hash,
        first_deposit: z.batch_start.deposit,
        final_deposit: z.processed_deposit,
        tempo_block: imported_block_number,
        zone_height: facts.block_number,
    };
    overlay.set(
        StateKey::Batch(id),
        Some(StateValue::Batch(BatchState::Finalized {
            boundary,
            first_withdrawal: z.batch_start.withdrawal_index,
            count,
            queue_hash,
        })),
    );
    z.withdrawal_batch_index = index;
    z.withdrawal_queue_hash = queue_hash;
    z.batch_start = crate::kernel::state::BatchBoundaryStart {
        parent_hash: facts.block_hash,
        deposit: z.processed_deposit,
        withdrawal_index: z.next_withdrawal_index,
    };
    overlay.set(StateKey::Zone, Some(StateValue::Zone(z)));
    effects.push(Effect::BatchFinalized { id, queue_hash });
    Ok(())
}
