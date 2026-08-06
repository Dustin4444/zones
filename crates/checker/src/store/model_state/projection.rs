//! Canonical projection of dynamic model families into physical rows.
//!
//! Both whole-model snapshots and sparse transition updates pass through these
//! functions. This keeps each family's physical key and value shape in one
//! place, including deletion keys for sparse updates.

use alloy_primitives::Address;

use crate::{
    model::{
        encoding::{OrdinaryDeposit, UserWithdrawalIdentity},
        ownership::{
            BatchId, BatchOwner, DepositId, DepositOwner, FallbackId, FallbackOwner, InboxRefundId,
            InboxRefundOwner, PendingWithdrawal, PortalRefundId, PortalRefundOwner, WithdrawalId,
            WithdrawalIdentity, WithdrawalOwner,
        },
        state::{PortalIdentity, TokenPhase, TokenState},
    },
    store::{schema::ModelKey, value::*},
};

use super::{ModelPersistenceError, cursor};

pub(super) type ProjectedRow = (ModelKey, Option<ModelValue>);

pub(super) fn token(token: Address, value: &TokenState) -> ProjectedRow {
    (
        ModelKey::Token(token),
        Some(ModelValue::Token(TokenValue {
            phase: match value.phase() {
                TokenPhase::PendingZoneEnable => StoredTokenPhase::PendingZoneEnable,
                TokenPhase::ZoneEnabled => StoredTokenPhase::ZoneEnabled,
            },
            supply: value.accounting().supply,
            deposit_liability: value.accounting().deposit_liability,
            withdrawal_liability: value.accounting().withdrawal_liability,
        })),
    )
}

pub(super) fn pending_deposit(
    identity: PortalIdentity,
    id: DepositId,
    owner: Option<&DepositOwner>,
) -> Result<ProjectedRow, ModelPersistenceError> {
    require_address(identity.portal(), id.portal, "deposit Portal")?;
    Ok((
        ModelKey::PendingDeposit(id.deposit_number.get()),
        owner.map(|owner| ModelValue::PendingDeposit(pending_deposit_value(owner))),
    ))
}

pub(super) fn withdrawal(
    identity: PortalIdentity,
    id: WithdrawalId,
    owner: Option<&WithdrawalOwner>,
) -> Result<ProjectedRow, ModelPersistenceError> {
    require_zone(identity.zone_id(), id.zone_id, "withdrawal Zone ID")?;
    Ok((
        ModelKey::Withdrawal(id.withdrawal_index),
        owner.map(|owner| ModelValue::Withdrawal(withdrawal_value(owner))),
    ))
}

pub(super) fn batch(
    identity: PortalIdentity,
    id: BatchId,
    owner: Option<&BatchOwner>,
) -> Result<ProjectedRow, ModelPersistenceError> {
    require_zone(identity.zone_id(), id.zone_id, "batch Zone ID")?;
    Ok((
        ModelKey::Batch(id.withdrawal_batch_index.get()),
        owner.map(|owner| ModelValue::Batch(batch_value(owner))),
    ))
}

pub(super) fn fallback_owner(
    identity: PortalIdentity,
    id: FallbackId,
    owner: Option<&FallbackOwner>,
) -> Result<ProjectedRow, ModelPersistenceError> {
    require_zone(identity.zone_id(), id.zone_id, "fallback Zone ID")?;
    Ok((
        ModelKey::FallbackOwner(id.fallback_nonce.get()),
        owner.map(|owner| ModelValue::FallbackOwner(fallback_value(owner))),
    ))
}

pub(super) fn portal_refund(
    identity: PortalIdentity,
    id: PortalRefundId,
    owner: Option<&PortalRefundOwner>,
) -> Result<ProjectedRow, ModelPersistenceError> {
    require_address(
        identity.portal(),
        id.failed_deposit.portal,
        "refund deposit Portal",
    )?;
    Ok((
        ModelKey::PortalRefundCredit {
            token: id.token,
            recipient: id.recipient,
            origin: id.failed_deposit.deposit_number.get(),
        },
        owner.map(|owner| {
            let PortalRefundOwner::Pending { amount } = owner;
            ModelValue::PortalRefundCredit(*amount)
        }),
    ))
}

pub(super) fn inbox_refund(
    identity: PortalIdentity,
    id: InboxRefundId,
    owner: Option<&InboxRefundOwner>,
) -> Result<ProjectedRow, ModelPersistenceError> {
    require_zone(
        identity.zone_id(),
        id.user_withdrawal.zone_id,
        "refund withdrawal Zone ID",
    )?;
    Ok((
        ModelKey::InboxRefundCredit {
            token: id.token,
            recipient: id.recipient,
            origin: id.user_withdrawal.withdrawal_index,
        },
        owner.map(|owner| {
            let InboxRefundOwner::Pending { amount } = owner;
            ModelValue::InboxRefundCredit(amount.get())
        }),
    ))
}

fn pending_deposit_value(owner: &DepositOwner) -> PendingDepositValue {
    match owner {
        DepositOwner::PendingOrdinary { preimage } => {
            PendingDepositValue::Ordinary(ordinary_deposit_value(preimage))
        }
        DepositOwner::PendingWithdrawalBounceBack {
            withdrawal,
            preimage,
        } => PendingDepositValue::WithdrawalBounceBack {
            withdrawal_zone_id: withdrawal.zone_id,
            withdrawal_index: withdrawal.withdrawal_index,
            preimage: BounceBackDepositValue {
                token: preimage.token(),
                fallback_nonce: preimage.fallback_nonce().get(),
                amount: preimage.amount().get(),
            },
        },
    }
}

fn ordinary_deposit_value(value: &OrdinaryDeposit) -> OrdinaryDepositValue {
    let encrypted = value.encrypted();
    let mut nonce = [0_u8; 12];
    nonce.copy_from_slice(encrypted.nonce().as_slice());
    let mut tag = [0_u8; 16];
    tag.copy_from_slice(encrypted.tag().as_slice());
    OrdinaryDepositValue {
        token: value.token(),
        sender: value.sender(),
        amount: value.amount(),
        tempo_refund_recipient: value.tempo_refund_recipient(),
        key_index: value.key_index(),
        ephemeral_pubkey_x: encrypted.ephemeral_pubkey_x(),
        ephemeral_pubkey_y_parity: encrypted.ephemeral_pubkey_y_parity(),
        ciphertext: encrypted.ciphertext().as_slice().to_vec(),
        nonce,
        tag,
    }
}

fn withdrawal_value(owner: &WithdrawalOwner) -> WithdrawalValue {
    match owner {
        WithdrawalOwner::Pending(PendingWithdrawal::User(pending)) => {
            let (identity, request, sender_reveal) = pending.parts();
            WithdrawalValue::Pending(PendingWithdrawalValue::User {
                identity: user_identity_value(identity),
                request: UserWithdrawalRequestValue {
                    token: request.token(),
                    recipient: request.to(),
                    amount: request.principal().get(),
                    memo: request.memo(),
                    gas_limit: request.gas_limit().get(),
                    callback_data: request.callback_data().to_vec(),
                },
                sender_reveal: if sender_reveal.is_enabled() {
                    StoredSenderReveal::Encrypted
                } else {
                    StoredSenderReveal::None
                },
            })
        }
        WithdrawalOwner::Pending(PendingWithdrawal::FailedDeposit(pending)) => {
            let (deposit, token, recipient, amount) = pending.parts();
            WithdrawalValue::Pending(PendingWithdrawalValue::FailedDeposit {
                deposit_portal: deposit.portal,
                deposit_number: deposit.deposit_number.get(),
                token,
                recipient,
                amount,
            })
        }
        WithdrawalOwner::Finalized(finalized) => match finalized.identity() {
            WithdrawalIdentity::User(identity) => {
                let preimage = finalized.preimage();
                WithdrawalValue::FinalizedUser {
                    identity: user_identity_value(identity),
                    request: UserWithdrawalRequestValue {
                        token: preimage.token(),
                        recipient: preimage.to(),
                        amount: preimage.amount(),
                        memo: preimage.memo(),
                        gas_limit: preimage.gas_limit(),
                        callback_data: preimage.callback_data().to_vec(),
                    },
                    encrypted_sender: preimage.encrypted_sender().to_vec(),
                }
            }
            WithdrawalIdentity::FailedDeposit { deposit } => {
                let preimage = finalized.preimage();
                WithdrawalValue::FinalizedFailedDeposit {
                    deposit_portal: deposit.portal,
                    deposit_number: deposit.deposit_number.get(),
                    token: preimage.token(),
                    recipient: preimage.to(),
                    amount: preimage.amount(),
                }
            }
        },
    }
}

fn user_identity_value(identity: UserWithdrawalIdentity) -> UserWithdrawalIdentityValue {
    UserWithdrawalIdentityValue {
        sender: identity.sender(),
        transaction_hash: identity.tx_hash(),
        fallback_nonce: identity.fallback_nonce().get(),
    }
}

fn fallback_value(owner: &FallbackOwner) -> FallbackOwnerValue {
    match owner {
        FallbackOwner::Held {
            withdrawal,
            token,
            amount,
        } => FallbackOwnerValue::Held {
            withdrawal_zone_id: withdrawal.zone_id,
            withdrawal_index: withdrawal.withdrawal_index,
            token: *token,
            amount: amount.get(),
        },
        FallbackOwner::BounceBackQueued {
            withdrawal,
            token,
            amount,
            deposit,
        } => FallbackOwnerValue::BounceBackQueued {
            withdrawal_zone_id: withdrawal.zone_id,
            withdrawal_index: withdrawal.withdrawal_index,
            token: *token,
            amount: amount.get(),
            deposit_portal: deposit.portal,
            deposit_number: deposit.deposit_number.get(),
        },
    }
}

fn batch_value(owner: &BatchOwner) -> BatchValue {
    match owner {
        BatchOwner::Finalized(batch) => BatchValue::Finalized(finalized_batch_value(batch)),
        BatchOwner::Submitted(submitted) => BatchValue::Submitted {
            batch: finalized_batch_value(submitted.batch()),
            portal: submitted.portal_queue().portal(),
            logical_queue_index: submitted.portal_queue().logical_queue_index(),
            next_processing_ordinal: submitted.next_processing_ordinal(),
            remaining_queue_hash: submitted.remaining_queue_hash(),
        },
    }
}

fn finalized_batch_value(
    batch: &crate::model::ownership::FinalizedBatchState,
) -> FinalizedBatchValue {
    let boundary = batch.boundary();
    let members = batch.members();
    FinalizedBatchValue {
        boundary: BatchBoundaryValue {
            first_zone_parent_hash: boundary.first_zone_parent_hash,
            final_zone_block_hash: boundary.final_zone_block_hash,
            first_processed_deposit: cursor(
                boundary.first_processed_deposit.hash,
                boundary.first_processed_deposit.number,
            ),
            final_processed_deposit: cursor(
                boundary.final_processed_deposit.hash,
                boundary.final_processed_deposit.number,
            ),
            final_imported_tempo_block_number: boundary.final_imported_tempo_block_number,
            final_zone_height: boundary.final_zone_height,
        },
        members: BatchMembersValue {
            first_withdrawal_index: members.first_withdrawal_index(),
            member_count: members.member_count(),
            withdrawal_queue_hash: members.withdrawal_queue_hash(),
        },
    }
}

fn require_address(
    expected: Address,
    actual: Address,
    kind: &'static str,
) -> Result<(), ModelPersistenceError> {
    if expected == actual {
        Ok(())
    } else {
        Err(ModelPersistenceError::AddressIdentityMismatch {
            kind,
            expected,
            actual,
        })
    }
}

fn require_zone(
    expected: u32,
    actual: u32,
    kind: &'static str,
) -> Result<(), ModelPersistenceError> {
    if expected == actual {
        Ok(())
    } else {
        Err(ModelPersistenceError::ZoneIdentityMismatch {
            kind,
            expected,
            actual,
        })
    }
}
