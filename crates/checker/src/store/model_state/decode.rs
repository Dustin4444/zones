use std::{
    collections::BTreeMap,
    num::{NonZeroU64, NonZeroU128},
};

use alloy_primitives::{Bytes, FixedBytes};

use crate::store::{schema::ModelKey, value::*};

use super::{ModelPersistenceError, ModelRows};
use crate::model::{
    accounting::TokenAccounting,
    encoding::{
        CompressedYParity, DepositPayload, OrdinaryDeposit, SenderReveal, UserWithdrawalIdentity,
        UserWithdrawalRequest, WithdrawalBounceBackDeposit,
    },
    ownership::{
        BatchBoundary, BatchId, BatchMembers, BatchOwner, DepositCursor, DepositId, DepositOwner,
        FailedDepositPendingWithdrawal, FallbackId, FallbackOwner, FinalizedBatchState,
        InboxRefundId, InboxRefundOwner, PendingWithdrawal, PortalQueueId, PortalRefundId,
        PortalRefundOwner, SubmittedBatchState, UserPendingWithdrawal, WithdrawalId,
        WithdrawalOwner,
    },
    state::{
        BatchStart, CreatedPortalState, ModelState, ModelStateParts, PortalConfig,
        PortalDepositCursor, PortalIdentity, PortalLifecycle, PortalSettlementState, TokenPhase,
        TokenState, ZoneConfig, ZoneLastBatch, ZoneProcessedDepositCursor, ZoneState,
    },
};

pub(crate) fn assemble_model(
    identity: PortalIdentity,
    mut rows: ModelRows,
) -> Result<ModelState, ModelPersistenceError> {
    for (key, value) in &rows {
        if !value.matches_key(*key) {
            return Err(ModelPersistenceError::KeyValueMismatch {
                key: *key,
                value: Box::new(value.clone()),
            });
        }
    }

    let portal = take_portal(identity, &mut rows)?;
    let zone = take_zone(&mut rows)?;
    let mut parts = ModelStateParts {
        portal,
        zone,
        tokens: BTreeMap::new(),
        pending_deposits: BTreeMap::new(),
        withdrawals: BTreeMap::new(),
        batches: BTreeMap::new(),
        fallback_owners: BTreeMap::new(),
        portal_refunds: BTreeMap::new(),
        inbox_refunds: BTreeMap::new(),
    };

    for (key, value) in rows {
        insert_dynamic(identity, &mut parts, key, value)?;
    }
    Ok(ModelState::from_parts(parts)?)
}

fn take_portal(
    identity: PortalIdentity,
    rows: &mut ModelRows,
) -> Result<PortalLifecycle, ModelPersistenceError> {
    let config = rows.remove(&ModelKey::PortalConfig);
    let cursor = rows.remove(&ModelKey::PortalDepositCursor);
    let settlement = rows.remove(&ModelKey::PortalSettlement);
    match (config, cursor, settlement) {
        (None, None, None) => Ok(PortalLifecycle::AwaitingCreation { expected: identity }),
        (
            Some(ModelValue::PortalConfig { bounceback_gas }),
            Some(ModelValue::PortalDepositCursor(cursor)),
            Some(ModelValue::PortalSettlement(settlement)),
        ) => Ok(PortalLifecycle::Created(Box::new(CreatedPortalState::new(
            identity,
            PortalConfig::new(bounceback_gas),
            PortalDepositCursor::new(cursor.hash, cursor.number),
            PortalSettlementState::new(
                settlement.withdrawal_batch_index,
                settlement.block_hash,
                settlement.last_synced_tempo_block_number,
                DepositCursor {
                    hash: settlement.last_submitted_deposit_cursor.hash,
                    number: settlement.last_submitted_deposit_cursor.number,
                },
                settlement.zone_height,
                settlement.withdrawal_queue_head,
                settlement.withdrawal_queue_tail,
            ),
        )))),
        (Some(value), _, _) if !value.matches_key(ModelKey::PortalConfig) => {
            Err(ModelPersistenceError::KeyValueMismatch {
                key: ModelKey::PortalConfig,
                value: Box::new(value),
            })
        }
        _ => Err(ModelPersistenceError::Partial("created Portal state")),
    }
}

fn take_zone(rows: &mut ModelRows) -> Result<ZoneState, ModelPersistenceError> {
    let config = required(rows, ModelKey::ZoneConfig, "Zone config")?;
    let cursor = required(
        rows,
        ModelKey::ZoneProcessedDepositCursor,
        "Zone processed-deposit cursor",
    )?;
    let accumulator = required(
        rows,
        ModelKey::ZoneBatchAccumulator,
        "Zone batch accumulator",
    )?;
    let next_withdrawal = required(
        rows,
        ModelKey::ZoneNextWithdrawalIndex,
        "Zone next withdrawal index",
    )?;
    let last_fallback = required(
        rows,
        ModelKey::ZoneLastFallbackNonce,
        "Zone last fallback nonce",
    )?;

    let ModelValue::ZoneConfig {
        tempo_gas_rate,
        max_withdrawals_per_block,
    } = config
    else {
        return mismatch(ModelKey::ZoneConfig, config);
    };
    let ModelValue::ZoneProcessedDepositCursor(cursor) = cursor else {
        return mismatch(ModelKey::ZoneProcessedDepositCursor, cursor);
    };
    let ModelValue::ZoneBatchAccumulator(accumulator) = accumulator else {
        return mismatch(ModelKey::ZoneBatchAccumulator, accumulator);
    };
    let ModelValue::ZoneNextWithdrawalIndex(next_withdrawal_index) = next_withdrawal else {
        return mismatch(ModelKey::ZoneNextWithdrawalIndex, next_withdrawal);
    };
    let ModelValue::ZoneLastFallbackNonce(last_fallback_nonce) = last_fallback else {
        return mismatch(ModelKey::ZoneLastFallbackNonce, last_fallback);
    };

    Ok(ZoneState::new(
        ZoneConfig::new(tempo_gas_rate, max_withdrawals_per_block),
        ZoneProcessedDepositCursor::new(cursor.hash, cursor.number),
        next_withdrawal_index,
        last_fallback_nonce,
        ZoneLastBatch::new(
            accumulator.last_withdrawal_queue_hash,
            accumulator.last_withdrawal_batch_index,
        ),
        BatchStart::new(
            accumulator.first_zone_parent_hash,
            ZoneProcessedDepositCursor::new(
                accumulator.first_processed_deposit.hash,
                accumulator.first_processed_deposit.number,
            ),
            accumulator.first_withdrawal_index,
        ),
    ))
}

fn insert_dynamic(
    identity: PortalIdentity,
    state: &mut ModelStateParts,
    key: ModelKey,
    value: ModelValue,
) -> Result<(), ModelPersistenceError> {
    match (key, value) {
        (ModelKey::Token(token), ModelValue::Token(value)) => insert(
            &mut state.tokens,
            token,
            TokenState::new(
                match value.phase {
                    StoredTokenPhase::PendingZoneEnable => TokenPhase::PendingZoneEnable,
                    StoredTokenPhase::ZoneEnabled => TokenPhase::ZoneEnabled,
                },
                TokenAccounting {
                    supply: value.supply,
                    deposit_liability: value.deposit_liability,
                    withdrawal_liability: value.withdrawal_liability,
                },
            ),
            "token",
        ),
        (ModelKey::PendingDeposit(number), ModelValue::PendingDeposit(value)) => {
            let id = DepositId {
                portal: identity.portal(),
                deposit_number: nonzero_u64(number, "deposit")?,
            };
            insert(
                &mut state.pending_deposits,
                id,
                decode_pending_deposit(identity, value)?,
                "deposit",
            )
        }
        (ModelKey::Withdrawal(index), ModelValue::Withdrawal(value)) => insert(
            &mut state.withdrawals,
            WithdrawalId {
                zone_id: identity.zone_id(),
                withdrawal_index: index,
            },
            decode_withdrawal(identity, value)?,
            "withdrawal",
        ),
        (ModelKey::FallbackOwner(nonce), ModelValue::FallbackOwner(value)) => insert(
            &mut state.fallback_owners,
            FallbackId {
                zone_id: identity.zone_id(),
                fallback_nonce: nonzero_u64(nonce, "fallback")?,
            },
            decode_fallback(identity, value)?,
            "fallback",
        ),
        (ModelKey::Batch(index), ModelValue::Batch(value)) => insert(
            &mut state.batches,
            BatchId {
                zone_id: identity.zone_id(),
                withdrawal_batch_index: nonzero_u64(index, "batch")?,
            },
            decode_batch(identity, value)?,
            "batch",
        ),
        (
            ModelKey::PortalRefundCredit {
                token,
                recipient,
                origin,
            },
            ModelValue::PortalRefundCredit(amount),
        ) => insert(
            &mut state.portal_refunds,
            PortalRefundId {
                token,
                recipient,
                failed_deposit: DepositId {
                    portal: identity.portal(),
                    deposit_number: nonzero_u64(origin, "Portal refund origin")?,
                },
            },
            PortalRefundOwner::Pending { amount },
            "Portal refund credit",
        ),
        (
            ModelKey::InboxRefundCredit {
                token,
                recipient,
                origin,
            },
            ModelValue::InboxRefundCredit(amount),
        ) => insert(
            &mut state.inbox_refunds,
            InboxRefundId {
                token,
                recipient,
                user_withdrawal: WithdrawalId {
                    zone_id: identity.zone_id(),
                    withdrawal_index: origin,
                },
            },
            InboxRefundOwner::Pending {
                amount: nonzero_u128(amount, "Inbox refund credit")?,
            },
            "Inbox refund credit",
        ),
        (key, value) => Err(ModelPersistenceError::KeyValueMismatch {
            key,
            value: Box::new(value),
        }),
    }
}

fn decode_pending_deposit(
    identity: PortalIdentity,
    value: PendingDepositValue,
) -> Result<DepositOwner, ModelPersistenceError> {
    Ok(match value {
        PendingDepositValue::Ordinary(value) => DepositOwner::PendingOrdinary {
            preimage: ordinary(value)?,
        },
        PendingDepositValue::WithdrawalBounceBack {
            withdrawal_zone_id,
            withdrawal_index,
            preimage,
        } => {
            require_zone(identity, withdrawal_zone_id, "bounce-back withdrawal")?;
            DepositOwner::PendingWithdrawalBounceBack {
                withdrawal: WithdrawalId {
                    zone_id: withdrawal_zone_id,
                    withdrawal_index,
                },
                preimage: WithdrawalBounceBackDeposit::new(
                    preimage.token,
                    nonzero_u64(preimage.fallback_nonce, "bounce-back fallback nonce")?,
                    nonzero_u128(preimage.amount, "bounce-back amount")?,
                ),
            }
        }
    })
}

fn ordinary(value: OrdinaryDepositValue) -> Result<OrdinaryDeposit, ModelPersistenceError> {
    let ciphertext = FixedBytes::from_slice(&value.ciphertext);
    Ok(OrdinaryDeposit::new(
        value.token,
        value.sender,
        value.amount,
        value.tempo_refund_recipient,
        value.key_index,
        DepositPayload::new(
            value.ephemeral_pubkey_x,
            CompressedYParity::from_u8(value.ephemeral_pubkey_y_parity)?,
            ciphertext,
            FixedBytes::from(value.nonce),
            FixedBytes::from(value.tag),
        ),
    ))
}

fn decode_withdrawal(
    identity: PortalIdentity,
    value: WithdrawalValue,
) -> Result<WithdrawalOwner, ModelPersistenceError> {
    Ok(match value {
        WithdrawalValue::Pending(PendingWithdrawalValue::User {
            identity: user,
            request,
            sender_reveal,
        }) => WithdrawalOwner::Pending(PendingWithdrawal::User(UserPendingWithdrawal::from_parts(
            user_identity(user)?,
            user_request(request)?,
            match sender_reveal {
                StoredSenderReveal::None => SenderReveal::None,
                StoredSenderReveal::Encrypted => SenderReveal::Encrypted,
            },
        ))),
        WithdrawalValue::Pending(PendingWithdrawalValue::FailedDeposit {
            deposit_portal,
            deposit_number,
            token,
            recipient,
            amount,
        }) => {
            require_portal(identity, deposit_portal, "failed-deposit withdrawal")?;
            WithdrawalOwner::Pending(PendingWithdrawal::FailedDeposit(
                FailedDepositPendingWithdrawal::from_parts(
                    DepositId {
                        portal: deposit_portal,
                        deposit_number: nonzero_u64(deposit_number, "failed-deposit")?,
                    },
                    token,
                    recipient,
                    amount,
                ),
            ))
        }
        WithdrawalValue::FinalizedUser {
            identity: user,
            request,
            encrypted_sender,
        } => {
            let identity = user_identity(user)?;
            let request = user_request(request)?;
            let sender_reveal = if encrypted_sender.is_empty() {
                SenderReveal::None
            } else {
                SenderReveal::Encrypted
            };
            WithdrawalOwner::Finalized(
                UserPendingWithdrawal::from_parts(identity, request, sender_reveal)
                    .finalize(Bytes::from(encrypted_sender))?,
            )
        }
        WithdrawalValue::FinalizedFailedDeposit {
            deposit_portal,
            deposit_number,
            token,
            recipient,
            amount,
        } => {
            require_portal(identity, deposit_portal, "finalized failed deposit")?;
            WithdrawalOwner::Finalized(
                FailedDepositPendingWithdrawal::from_parts(
                    DepositId {
                        portal: deposit_portal,
                        deposit_number: nonzero_u64(deposit_number, "finalized failed deposit")?,
                    },
                    token,
                    recipient,
                    amount,
                )
                .finalize(),
            )
        }
    })
}

fn user_identity(
    value: UserWithdrawalIdentityValue,
) -> Result<UserWithdrawalIdentity, ModelPersistenceError> {
    Ok(UserWithdrawalIdentity::new(
        value.sender,
        value.transaction_hash,
        nonzero_u64(value.fallback_nonce, "withdrawal fallback nonce")?,
    )?)
}

fn user_request(
    value: UserWithdrawalRequestValue,
) -> Result<UserWithdrawalRequest, ModelPersistenceError> {
    Ok(UserWithdrawalRequest::new(
        value.token,
        value.recipient,
        value.amount,
        value.memo,
        value.gas_limit,
        Bytes::from(value.callback_data),
    )?)
}

fn decode_fallback(
    identity: PortalIdentity,
    value: FallbackOwnerValue,
) -> Result<FallbackOwner, ModelPersistenceError> {
    Ok(match value {
        FallbackOwnerValue::Held {
            withdrawal_zone_id,
            withdrawal_index,
            token,
            amount,
        } => {
            require_zone(identity, withdrawal_zone_id, "held fallback")?;
            FallbackOwner::Held {
                withdrawal: WithdrawalId {
                    zone_id: withdrawal_zone_id,
                    withdrawal_index,
                },
                token,
                amount: nonzero_u128(amount, "held fallback amount")?,
            }
        }
        FallbackOwnerValue::BounceBackQueued {
            withdrawal_zone_id,
            withdrawal_index,
            token,
            amount,
            deposit_portal,
            deposit_number,
        } => {
            require_zone(identity, withdrawal_zone_id, "queued fallback")?;
            require_portal(identity, deposit_portal, "queued fallback deposit")?;
            FallbackOwner::BounceBackQueued {
                withdrawal: WithdrawalId {
                    zone_id: withdrawal_zone_id,
                    withdrawal_index,
                },
                token,
                amount: nonzero_u128(amount, "queued fallback amount")?,
                deposit: DepositId {
                    portal: deposit_portal,
                    deposit_number: nonzero_u64(deposit_number, "queued fallback deposit")?,
                },
            }
        }
    })
}

fn decode_batch(
    identity: PortalIdentity,
    value: BatchValue,
) -> Result<BatchOwner, ModelPersistenceError> {
    Ok(match value {
        BatchValue::Finalized(batch) => BatchOwner::Finalized(finalized_batch(batch)?),
        BatchValue::Submitted {
            batch,
            portal,
            logical_queue_index,
            next_processing_ordinal,
            remaining_queue_hash,
        } => {
            require_portal(identity, portal, "submitted batch")?;
            let finalized = finalized_batch(batch)?;
            let mut submitted = SubmittedBatchState::new(
                finalized,
                PortalQueueId::new(portal, logical_queue_index)?,
            )?;
            if next_processing_ordinal != 0 {
                submitted =
                    submitted.advance_partial(next_processing_ordinal, remaining_queue_hash)?;
            } else if submitted.remaining_queue_hash() != remaining_queue_hash {
                return Err(ModelPersistenceError::Partial(
                    "submitted batch initial queue commitment",
                ));
            }
            BatchOwner::Submitted(submitted)
        }
    })
}

fn finalized_batch(
    value: FinalizedBatchValue,
) -> Result<FinalizedBatchState, ModelPersistenceError> {
    Ok(FinalizedBatchState::new(
        BatchBoundary {
            first_zone_parent_hash: value.boundary.first_zone_parent_hash,
            final_zone_block_hash: value.boundary.final_zone_block_hash,
            first_processed_deposit: DepositCursor {
                hash: value.boundary.first_processed_deposit.hash,
                number: value.boundary.first_processed_deposit.number,
            },
            final_processed_deposit: DepositCursor {
                hash: value.boundary.final_processed_deposit.hash,
                number: value.boundary.final_processed_deposit.number,
            },
            final_imported_tempo_block_number: value.boundary.final_imported_tempo_block_number,
            final_zone_height: value.boundary.final_zone_height,
        },
        BatchMembers::from_parts(
            value.members.first_withdrawal_index,
            value.members.member_count,
            value.members.withdrawal_queue_hash,
        )?,
    ))
}

fn required(
    rows: &mut ModelRows,
    key: ModelKey,
    name: &'static str,
) -> Result<ModelValue, ModelPersistenceError> {
    rows.remove(&key)
        .ok_or(ModelPersistenceError::Missing(name))
}

fn mismatch<T>(key: ModelKey, value: ModelValue) -> Result<T, ModelPersistenceError> {
    Err(ModelPersistenceError::KeyValueMismatch {
        key,
        value: Box::new(value),
    })
}

fn insert<K: Ord, V>(
    map: &mut BTreeMap<K, V>,
    key: K,
    value: V,
    kind: &'static str,
) -> Result<(), ModelPersistenceError> {
    if map.insert(key, value).is_some() {
        Err(ModelPersistenceError::Duplicate { kind })
    } else {
        Ok(())
    }
}

fn nonzero_u64(value: u64, kind: &'static str) -> Result<NonZeroU64, ModelPersistenceError> {
    NonZeroU64::new(value).ok_or(ModelPersistenceError::ZeroIdentifier { kind })
}

fn nonzero_u128(value: u128, kind: &'static str) -> Result<NonZeroU128, ModelPersistenceError> {
    NonZeroU128::new(value).ok_or(ModelPersistenceError::ZeroIdentifier { kind })
}

fn require_zone(
    identity: PortalIdentity,
    actual: u32,
    kind: &'static str,
) -> Result<(), ModelPersistenceError> {
    if identity.zone_id() == actual {
        Ok(())
    } else {
        Err(ModelPersistenceError::ZoneIdentityMismatch {
            kind,
            expected: identity.zone_id(),
            actual,
        })
    }
}

fn require_portal(
    identity: PortalIdentity,
    actual: alloy_primitives::Address,
    kind: &'static str,
) -> Result<(), ModelPersistenceError> {
    if identity.portal() == actual {
        Ok(())
    } else {
        Err(ModelPersistenceError::AddressIdentityMismatch {
            kind,
            expected: identity.portal(),
            actual,
        })
    }
}
