use std::collections::{BTreeMap, btree_map::Entry};

use alloy_primitives::{Address, B256, FixedBytes, U256, b256, bytes, fixed_bytes};

use super::*;
use crate::model::{
    constants::{AUTHENTICATED_WITHDRAWAL_SIZE, MAX_CALLBACK_DATA_SIZE, MAX_WITHDRAWAL_GAS_LIMIT},
    encoding::{CompressedYParity, DepositPayload, Withdrawal, withdrawal_queue_hash},
};

fn deposit_id() -> DepositId {
    DepositId {
        portal: Address::repeat_byte(0x11),
        deposit_number: NonZeroU64::new(9).unwrap(),
    }
}

fn withdrawal_id() -> WithdrawalId {
    WithdrawalId {
        zone_id: 7,
        withdrawal_index: 12,
    }
}

fn batch_id() -> BatchId {
    BatchId {
        zone_id: 7,
        withdrawal_batch_index: NonZeroU64::new(4).unwrap(),
    }
}

fn ordinary_preimage() -> OrdinaryDeposit {
    ordinary_preimage_for(Address::repeat_byte(0x77), Address::repeat_byte(0x99), 900)
}

fn ordinary_preimage_for(
    token: Address,
    tempo_refund_recipient: Address,
    amount: u128,
) -> OrdinaryDeposit {
    OrdinaryDeposit::new(
        token,
        Address::repeat_byte(0x88),
        amount,
        tempo_refund_recipient,
        U256::from(4),
        DepositPayload::new(
            b256!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            CompressedYParity::Even,
            FixedBytes::repeat_byte(0xbb),
            fixed_bytes!("cccccccccccccccccccccccc"),
            fixed_bytes!("dddddddddddddddddddddddddddddddd"),
        ),
    )
}

fn boundary() -> BatchBoundary {
    BatchBoundary {
        first_zone_parent_hash: B256::repeat_byte(0x10),
        final_zone_block_hash: B256::repeat_byte(0x11),
        first_processed_deposit: DepositCursor {
            hash: B256::repeat_byte(0x12),
            number: 3,
        },
        final_processed_deposit: DepositCursor {
            hash: B256::repeat_byte(0x13),
            number: 5,
        },
        final_imported_tempo_block_number: 90,
        final_zone_height: 44,
    }
}

fn batch_withdrawals() -> [Withdrawal; 2] {
    [
        Withdrawal::for_failed_deposit(Address::repeat_byte(0x40), Address::repeat_byte(0x41), 10),
        Withdrawal::for_failed_deposit(Address::repeat_byte(0x42), Address::repeat_byte(0x43), 20),
    ]
}

#[test]
fn model_failed_deposit_shape_hardcodes_literal_zero_public_fields() {
    let pending =
        FailedDepositPendingWithdrawal::from_failed_deposit(deposit_id(), ordinary_preimage());
    assert_eq!(
        PendingWithdrawal::FailedDeposit(pending.clone()).identity(),
        WithdrawalIdentity::FailedDeposit {
            deposit: deposit_id()
        }
    );

    let finalized = pending.finalize();
    assert_eq!(
        finalized.identity(),
        WithdrawalIdentity::FailedDeposit {
            deposit: deposit_id()
        }
    );
    assert_eq!(
        finalized.preimage().sender_tag(),
        crate::model::encoding::sender_tag(Address::ZERO, B256::ZERO)
    );
    assert_eq!(finalized.preimage().memo(), B256::ZERO);
    assert_eq!(finalized.preimage().gas_limit(), 0);
    assert_eq!(finalized.preimage().fallback_nonce(), 0);
    assert!(finalized.preimage().callback_data().is_empty());
    assert!(finalized.preimage().encrypted_sender().is_empty());
}

#[test]
fn model_user_pending_shape_validates_bounds_before_it_becomes_an_owner() {
    assert_eq!(
        UserWithdrawalIdentity::new(
            Address::repeat_byte(0x21),
            B256::ZERO,
            NonZeroU64::new(7).unwrap(),
        ),
        Err(WithdrawalDataError::ZeroTransactionHash)
    );
    let identity = UserWithdrawalIdentity::new(
        Address::repeat_byte(0x21),
        B256::repeat_byte(0x22),
        NonZeroU64::new(7).unwrap(),
    )
    .unwrap();
    assert_eq!(
        UserWithdrawalRequest::new(
            Address::repeat_byte(0x23),
            Address::repeat_byte(0x24),
            0,
            B256::repeat_byte(0x25),
            0,
            Bytes::new(),
        ),
        Err(WithdrawalDataError::ZeroAmount)
    );
    assert_eq!(
        UserWithdrawalRequest::new(
            Address::repeat_byte(0x23),
            Address::repeat_byte(0x24),
            500,
            B256::repeat_byte(0x25),
            MAX_WITHDRAWAL_GAS_LIMIT + 1,
            Bytes::new(),
        ),
        Err(WithdrawalDataError::GasLimitTooHigh {
            actual: MAX_WITHDRAWAL_GAS_LIMIT + 1,
            maximum: MAX_WITHDRAWAL_GAS_LIMIT,
        })
    );
    assert!(matches!(
        UserWithdrawalRequest::new(
            Address::repeat_byte(0x23),
            Address::repeat_byte(0x24),
            500,
            B256::repeat_byte(0x25),
            0,
            Bytes::from(vec![0; MAX_CALLBACK_DATA_SIZE + 1]),
        ),
        Err(WithdrawalDataError::CallbackDataTooLong { .. })
    ));
    let request = UserWithdrawalRequest::new(
        Address::repeat_byte(0x23),
        Address::repeat_byte(0x24),
        500,
        B256::repeat_byte(0x25),
        0,
        Bytes::new(),
    )
    .unwrap();
    assert!(matches!(
        UserPendingWithdrawal::new(
            identity,
            request,
            bytes!("04aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        ),
        Err(WithdrawalDataError::InvalidRevealToPrefix { actual: 4 })
    ));
}

#[test]
fn model_user_finalization_requires_reveal_matched_encrypted_sender() {
    let identity = UserWithdrawalIdentity::new(
        Address::repeat_byte(0x31),
        B256::repeat_byte(0x32),
        NonZeroU64::new(7).unwrap(),
    )
    .unwrap();
    let make_pending = || {
        UserPendingWithdrawal::new(
            identity,
            UserWithdrawalRequest::new(
                Address::repeat_byte(0x33),
                Address::repeat_byte(0x34),
                500,
                B256::repeat_byte(0x35),
                12_345,
                bytes!("deadbeef"),
            )
            .unwrap(),
            bytes!("020102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20"),
        )
        .unwrap()
    };

    assert_eq!(
        make_pending().finalize(Bytes::new()),
        Err(WithdrawalDataError::InvalidEncryptedSenderLength {
            actual: 0,
            expected: AUTHENTICATED_WITHDRAWAL_SIZE,
        })
    );
    let finalized = make_pending()
        .finalize(Bytes::from(vec![0x66; AUTHENTICATED_WITHDRAWAL_SIZE]))
        .unwrap();
    assert_eq!(finalized.identity(), WithdrawalIdentity::User(identity));
    assert_eq!(finalized.preimage().fallback_nonce(), 7);
    assert_eq!(finalized.preimage().callback_data(), &bytes!("deadbeef"));
    assert_eq!(
        finalized.preimage().encrypted_sender().len(),
        AUTHENTICATED_WITHDRAWAL_SIZE
    );
}

#[test]
fn model_same_deposit_identity_cannot_hold_two_actual_owner_variants() {
    let id = deposit_id();
    let ordinary = DepositOwner::PendingOrdinary {
        preimage: ordinary_preimage(),
    };
    let bounce = DepositOwner::PendingWithdrawalBounceBack {
        withdrawal: withdrawal_id(),
        preimage: WithdrawalBounceBackDeposit::new(
            Address::repeat_byte(0x43),
            NonZeroU64::new(7).unwrap(),
            NonZeroU128::new(500).unwrap(),
        ),
    };

    let mut owners = BTreeMap::<DepositId, DepositOwner>::new();
    assert!(matches!(owners.entry(id), Entry::Vacant(_)));
    owners.insert(id, ordinary.clone());
    assert!(matches!(owners.entry(id), Entry::Occupied(_)));
    assert_eq!(owners.get(&id), Some(&ordinary));
    assert_ne!(owners.get(&id), Some(&bounce));
}

#[test]
fn model_batch_phases_exclude_empty_submission_and_exhausted_open_cursor() {
    let portal = Address::repeat_byte(0x51);
    assert_eq!(
        PortalQueueId::new(portal, NO_WITHDRAWAL_QUEUE_INDEX),
        Err(PortalQueueIdError::EmptyBatchHasNoQueueIndex)
    );
    let queue = PortalQueueId::new(portal, U256::from(3)).unwrap();
    let empty =
        FinalizedBatchState::new(boundary(), BatchMembers::from_withdrawals(8, &[]).unwrap());
    assert_eq!(
        SubmittedBatchState::new(empty, queue),
        Err(BatchStateError::EmptyBatchCannotBeSubmitted)
    );
    assert_eq!(
        BatchMembers::from_parts(8, 0, B256::repeat_byte(0x19)),
        Err(BatchStateError::EmptyBatchHasQueueCommitment)
    );
    assert_eq!(
        BatchMembers::from_parts(8, 1, B256::ZERO),
        Err(BatchStateError::NonEmptyBatchHasNoQueueCommitment)
    );

    let withdrawals = batch_withdrawals();
    let finalized = FinalizedBatchState::new(
        boundary(),
        BatchMembers::from_withdrawals(8, &withdrawals).unwrap(),
    );
    let submitted = SubmittedBatchState::new(finalized, queue).unwrap();
    assert_eq!(submitted.next_processing_ordinal(), 0);
    assert_eq!(
        submitted.remaining_queue_hash(),
        withdrawal_queue_hash(&withdrawals)
    );
    assert_eq!(
        submitted
            .clone()
            .advance_partial(0, B256::repeat_byte(0x20)),
        Err(BatchStateError::ProcessingOrdinalDidNotAdvance {
            current: 0,
            next: 0,
        })
    );
    assert_eq!(
        submitted
            .clone()
            .advance_partial(2, B256::repeat_byte(0x21)),
        Err(BatchStateError::ProcessingOrdinalOutOfRange {
            ordinal: 2,
            member_count: 2,
        })
    );
    assert_eq!(
        submitted.clone().advance_partial(1, B256::ZERO),
        Err(BatchStateError::SubmittedBatchHasNoRemainingQueue)
    );
    let partial_queue = B256::repeat_byte(0x22);
    let partial = submitted.advance_partial(1, partial_queue).unwrap();
    assert_eq!(partial.next_processing_ordinal(), 1);
    assert_eq!(partial.remaining_queue_hash(), partial_queue);
    assert_eq!(partial.batch().members().member_count(), 2);
    assert_eq!(partial.batch().members().first_withdrawal_index(), 8);
    assert_eq!(partial.batch().members().member_index(1), Some(9));
    assert_eq!(partial.batch().members().member_index(2), None);
    assert_eq!(
        partial.batch().members().withdrawal_queue_hash(),
        withdrawal_queue_hash(&withdrawals)
    );
    assert_eq!(
        partial.advance_partial(0, B256::repeat_byte(0x23)),
        Err(BatchStateError::ProcessingOrdinalDidNotAdvance {
            current: 1,
            next: 0,
        })
    );
    assert_eq!(
        BatchMembers::from_withdrawals(u64::MAX, &withdrawals),
        Err(BatchStateError::WithdrawalRangeOverflow {
            first_withdrawal_index: u64::MAX,
            member_count: 2,
        })
    );
    assert_eq!(
        BatchMembers::from_parts(u64::MAX, 1, B256::repeat_byte(0x24)),
        Err(BatchStateError::WithdrawalRangeOverflow {
            first_withdrawal_index: u64::MAX,
            member_count: 1,
        })
    );
}

#[test]
fn model_concrete_owner_snapshots_cover_every_lifecycle_family() {
    #[derive(Debug, Default)]
    struct ConcreteOwners {
        deposit: Option<(DepositId, DepositOwner)>,
        withdrawal: Option<(WithdrawalId, WithdrawalOwner)>,
        batch: Option<(BatchId, BatchOwner)>,
        fallback: Option<(FallbackId, FallbackOwner)>,
        portal_refund: Option<(PortalRefundId, PortalRefundOwner)>,
        inbox_refund: Option<(InboxRefundId, InboxRefundOwner)>,
    }

    let token = Address::repeat_byte(0x61);
    let recipient = Address::repeat_byte(0x62);
    let deposit = deposit_id();
    let withdrawal = withdrawal_id();
    let batch = batch_id();
    let nonce = NonZeroU64::new(7).unwrap();

    // Ordinary append -> mint terminal, or failed withdrawal -> finalized
    // -> direct refund terminal / Portal refund credit -> claim terminal.
    let ordinary = ordinary_preimage_for(token, recipient, 900);
    let ordinary_open = ConcreteOwners {
        deposit: Some((
            deposit,
            DepositOwner::PendingOrdinary {
                preimage: ordinary.clone(),
            },
        )),
        ..Default::default()
    };
    assert!(matches!(
        ordinary_open.deposit,
        Some((_, DepositOwner::PendingOrdinary { .. }))
    ));
    let failed_pending = FailedDepositPendingWithdrawal::from_failed_deposit(deposit, ordinary);
    let failed_open = ConcreteOwners {
        withdrawal: Some((
            withdrawal,
            WithdrawalOwner::Pending(PendingWithdrawal::FailedDeposit(failed_pending.clone())),
        )),
        ..Default::default()
    };
    assert!(ordinary_open.withdrawal.is_none());
    assert!(failed_open.deposit.is_none());
    let failed_finalized = failed_pending.finalize();
    let failed_finalized_open = ConcreteOwners {
        withdrawal: Some((withdrawal, WithdrawalOwner::Finalized(failed_finalized))),
        ..Default::default()
    };
    assert!(matches!(
        failed_finalized_open.withdrawal,
        Some((_, WithdrawalOwner::Finalized(_)))
    ));
    let portal_refund_id = PortalRefundId {
        token,
        recipient,
        failed_deposit: deposit,
    };
    let portal_credit = ConcreteOwners {
        portal_refund: Some((portal_refund_id, PortalRefundOwner::Pending { amount: 895 })),
        ..Default::default()
    };
    assert!(failed_finalized_open.portal_refund.is_none());
    assert!(portal_credit.withdrawal.is_none());
    assert!(ConcreteOwners::default().portal_refund.is_none());

    // User acceptance creates the withdrawal plus fallback; delivery
    // deletes both, while failure replaces only the withdrawal with one
    // bounce-back deposit and retains the same fallback nonce.
    let user_identity =
        UserWithdrawalIdentity::new(Address::repeat_byte(0x64), B256::repeat_byte(0x65), nonce)
            .unwrap();
    let user_pending = UserPendingWithdrawal::new(
        user_identity,
        UserWithdrawalRequest::new(token, recipient, 500, B256::ZERO, 0, Bytes::new()).unwrap(),
        Bytes::new(),
    )
    .unwrap();
    let fallback_id = FallbackId {
        zone_id: withdrawal.zone_id,
        fallback_nonce: nonce,
    };
    let fallback_owner = FallbackOwner::Held {
        withdrawal,
        token,
        amount: NonZeroU128::new(500).unwrap(),
    };
    let user_open = ConcreteOwners {
        withdrawal: Some((
            withdrawal,
            WithdrawalOwner::Pending(PendingWithdrawal::User(user_pending.clone())),
        )),
        fallback: Some((fallback_id, fallback_owner.clone())),
        ..Default::default()
    };
    assert!(user_open.withdrawal.is_some() && user_open.fallback.is_some());
    let user_finalized = user_pending.finalize(Bytes::new()).unwrap();
    let finalized_user_open = ConcreteOwners {
        withdrawal: Some((withdrawal, WithdrawalOwner::Finalized(user_finalized))),
        fallback: Some((fallback_id, fallback_owner)),
        ..Default::default()
    };
    assert!(finalized_user_open.fallback.is_some());
    let bounce_preimage =
        WithdrawalBounceBackDeposit::new(token, nonce, NonZeroU128::new(500).unwrap());
    let bounce_deposit = DepositId {
        portal: deposit.portal,
        deposit_number: NonZeroU64::new(deposit.deposit_number.get() + 1).unwrap(),
    };
    let bounced_open = ConcreteOwners {
        deposit: Some((
            bounce_deposit,
            DepositOwner::PendingWithdrawalBounceBack {
                withdrawal,
                preimage: bounce_preimage,
            },
        )),
        fallback: Some((
            fallback_id,
            FallbackOwner::BounceBackQueued {
                withdrawal,
                token,
                amount: NonZeroU128::new(500).unwrap(),
                deposit: bounce_deposit,
            },
        )),
        ..Default::default()
    };
    assert!(bounced_open.withdrawal.is_none() && bounced_open.fallback.is_some());
    let (_, DepositOwner::PendingWithdrawalBounceBack { preimage, .. }) =
        bounced_open.deposit.as_ref().unwrap()
    else {
        unreachable!()
    };
    assert_eq!(preimage.fallback_nonce(), fallback_id.fallback_nonce);
    let (_, FallbackOwner::BounceBackQueued { deposit, .. }) =
        bounced_open.fallback.as_ref().unwrap()
    else {
        unreachable!()
    };
    assert_eq!(*deposit, bounce_deposit);

    // Bounce-back mint deletes both remaining owners; pending bounce-back
    // instead replaces them with one per-origin Inbox credit, then claim
    // deletes that credit.
    let inbox_refund_id = InboxRefundId {
        token,
        recipient,
        user_withdrawal: withdrawal,
    };
    let inbox_credit = ConcreteOwners {
        inbox_refund: Some((
            inbox_refund_id,
            InboxRefundOwner::Pending {
                amount: NonZeroU128::new(500).unwrap(),
            },
        )),
        ..Default::default()
    };
    assert!(inbox_credit.deposit.is_none() && inbox_credit.fallback.is_none());
    assert!(ConcreteOwners::default().inbox_refund.is_none());

    // Finalized empty batches terminate without a Portal queue; non-empty
    // batches alone become submitted and remain open through partial work.
    let finalized_empty = ConcreteOwners {
        batch: Some((
            batch,
            BatchOwner::Finalized(FinalizedBatchState::new(
                boundary(),
                BatchMembers::from_withdrawals(20, &[]).unwrap(),
            )),
        )),
        ..Default::default()
    };
    assert!(finalized_empty.batch.is_some());
    let withdrawals = batch_withdrawals();
    let finalized_nonempty = FinalizedBatchState::new(
        boundary(),
        BatchMembers::from_withdrawals(20, &withdrawals).unwrap(),
    );
    let queue = PortalQueueId::new(deposit.portal, U256::from(3)).unwrap();
    let submitted = ConcreteOwners {
        batch: Some((
            batch,
            BatchOwner::Submitted(
                SubmittedBatchState::new(finalized_nonempty, queue)
                    .unwrap()
                    .advance_partial(1, B256::repeat_byte(0x70))
                    .unwrap(),
            ),
        )),
        ..Default::default()
    };
    assert!(matches!(
        submitted.batch,
        Some((_, BatchOwner::Submitted(_)))
    ));
    assert!(ConcreteOwners::default().batch.is_none());
}
