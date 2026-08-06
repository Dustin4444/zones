use std::num::{NonZeroU64, NonZeroU128};

use alloy_primitives::{Address, B256, Bytes, FixedBytes, U256};

use super::*;
use crate::model::{
    accounting::TokenAccounting,
    encoding::{
        CompressedYParity, DepositPayload, OrdinaryDeposit, UserWithdrawalIdentity,
        UserWithdrawalRequest, Withdrawal, WithdrawalBounceBackDeposit, withdrawal_queue_hash,
    },
    ownership::{
        BatchBoundary, BatchId, BatchMembers, BatchOwner, DepositCursor, DepositId, DepositOwner,
        FailedDepositPendingWithdrawal, FallbackId, FallbackOwner, FinalizedBatchState,
        InboxRefundId, InboxRefundOwner, PendingWithdrawal, PortalQueueId, PortalRefundId,
        PortalRefundOwner, RefundAccount, SubmittedBatchState, UserPendingWithdrawal, WithdrawalId,
        WithdrawalOwner,
    },
    state::{
        BatchStart, ModelState, PortalDepositCursor, PortalIdentity, PortalLifecycle,
        PortalSettlementState, ZoneLastBatch, ZoneProcessedDepositCursor, portal_address_for_zone,
    },
};

mod accounting_refunds;
mod batches_origins;
mod counters_owners;
mod submitted_batches;

pub(super) const ZONE_ID: u32 = 7;

pub(super) fn token() -> Address {
    Address::repeat_byte(0x11)
}

pub(super) fn portal() -> Address {
    portal_address_for_zone(ZONE_ID)
}

pub(super) fn created() -> ModelState {
    ModelState::created_with_zone_token_for_test(
        PortalIdentity::new(portal(), ZONE_ID, token()),
        TokenAccounting::ZERO,
    )
}

pub(super) fn deposit_id(number: u64) -> DepositId {
    DepositId {
        portal: portal(),
        deposit_number: NonZeroU64::new(number).unwrap(),
    }
}

pub(super) fn withdrawal_id(index: u64) -> WithdrawalId {
    WithdrawalId {
        zone_id: ZONE_ID,
        withdrawal_index: index,
    }
}

pub(super) fn fallback_id(nonce: u64) -> FallbackId {
    FallbackId {
        zone_id: ZONE_ID,
        fallback_nonce: NonZeroU64::new(nonce).unwrap(),
    }
}

pub(super) fn batch_id(index: u64) -> BatchId {
    BatchId {
        zone_id: ZONE_ID,
        withdrawal_batch_index: NonZeroU64::new(index).unwrap(),
    }
}

pub(super) fn set_portal_cursor(state: &mut ModelState, hash: B256, number: u64) {
    let PortalLifecycle::Created(portal) = &mut state.portal else {
        unreachable!()
    };
    portal.deposit_cursor = PortalDepositCursor::new(hash, number);
}

pub(super) fn set_processed_cursor(state: &mut ModelState, hash: B256, number: u64) {
    set_portal_cursor(state, hash, number);
    state.zone.processed_deposit_cursor = ZoneProcessedDepositCursor::new(hash, number);
}

pub(super) fn set_terminal_batch(state: &mut ModelState, first_withdrawal_index: u64) {
    let block_hash = B256::repeat_byte(0x74);
    let processed = state.zone.processed_deposit_cursor;
    state.zone.last_batch = ZoneLastBatch::for_test(B256::repeat_byte(0x75), 1);
    state.zone.batch_start = BatchStart::new(block_hash, processed, first_withdrawal_index);
    let PortalLifecycle::Created(portal) = &mut state.portal else {
        unreachable!()
    };
    portal.settlement = PortalSettlementState::new(
        1,
        block_hash,
        0,
        DepositCursor {
            hash: processed.hash(),
            number: processed.number(),
        },
        U256::ONE,
        U256::ONE,
        U256::ONE,
    );
}

pub(super) fn ordinary(owner_token: Address, refund_recipient: Address) -> OrdinaryDeposit {
    OrdinaryDeposit::new(
        owner_token,
        Address::repeat_byte(0x21),
        9,
        refund_recipient,
        U256::from(3),
        DepositPayload::new(
            B256::repeat_byte(0x31),
            CompressedYParity::Even,
            FixedBytes::repeat_byte(0x32),
            FixedBytes::repeat_byte(0x33),
            FixedBytes::repeat_byte(0x34),
        ),
    )
}

pub(super) fn pending_user_at(
    withdrawal_index: u64,
    fallback_nonce: u64,
    amount: u128,
) -> (WithdrawalOwner, FallbackOwner) {
    let seed = u8::try_from(withdrawal_index).unwrap();
    let identity = UserWithdrawalIdentity::new(
        Address::repeat_byte(0x41_u8.wrapping_add(seed)),
        B256::repeat_byte(0x42_u8.wrapping_add(seed)),
        NonZeroU64::new(fallback_nonce).unwrap(),
    )
    .unwrap();
    let request = UserWithdrawalRequest::new(
        token(),
        Address::repeat_byte(0x43),
        amount,
        B256::ZERO,
        0,
        Bytes::new(),
    )
    .unwrap();
    (
        WithdrawalOwner::Pending(PendingWithdrawal::User(
            UserPendingWithdrawal::new(identity, request, Bytes::new()).unwrap(),
        )),
        FallbackOwner::Held {
            withdrawal: withdrawal_id(withdrawal_index),
            token: token(),
            amount: NonZeroU128::new(amount).unwrap(),
        },
    )
}

pub(super) fn pending_user() -> (WithdrawalOwner, FallbackOwner) {
    pending_user_at(0, 1, 5)
}

pub(super) fn valid_user_state() -> ModelState {
    let mut state = ModelState::created_with_zone_token_for_test(
        PortalIdentity::new(portal(), ZONE_ID, token()),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::from(5),
        },
    );
    state.zone.next_withdrawal_index = 1;
    state.zone.last_fallback_nonce = 1;
    let (withdrawal, fallback) = pending_user();
    state.withdrawals.insert(withdrawal_id(0), withdrawal);
    state.fallback_owners.insert(fallback_id(1), fallback);
    state
}

pub(super) fn valid_bounce_back_state() -> ModelState {
    let mut state = ModelState::created_with_zone_token_for_test(
        PortalIdentity::new(portal(), ZONE_ID, token()),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::from(5),
        },
    );
    let deposit = deposit_id(1);
    let withdrawal = withdrawal_id(0);
    let preimage = WithdrawalBounceBackDeposit::new(
        token(),
        NonZeroU64::new(1).unwrap(),
        NonZeroU128::new(5).unwrap(),
    );
    let cursor = DepositOwner::PendingWithdrawalBounceBack {
        withdrawal,
        preimage,
    }
    .queue_member()
    .hash_after(B256::ZERO);
    set_portal_cursor(&mut state, cursor, 1);
    state.zone.next_withdrawal_index = 1;
    state.zone.last_fallback_nonce = 1;
    set_terminal_batch(&mut state, 1);
    state.pending_deposits.insert(
        deposit,
        DepositOwner::PendingWithdrawalBounceBack {
            withdrawal,
            preimage,
        },
    );
    state.fallback_owners.insert(
        fallback_id(1),
        FallbackOwner::BounceBackQueued {
            withdrawal,
            token: token(),
            amount: NonZeroU128::new(5).unwrap(),
            deposit,
        },
    );
    state
}

pub(super) fn failed_withdrawal(origin: u64, amount: u128) -> WithdrawalOwner {
    WithdrawalOwner::Finalized(
        FailedDepositPendingWithdrawal::from_parts(
            deposit_id(origin),
            token(),
            Address::repeat_byte(0x61),
            amount,
        )
        .finalize(),
    )
}

pub(super) fn submitted_batch_state() -> ModelState {
    let mut state = ModelState::created_with_zone_token_for_test(
        PortalIdentity::new(portal(), ZONE_ID, token()),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(4),
            withdrawal_liability: U256::ZERO,
        },
    );
    let cursor_hash = B256::repeat_byte(0x71);
    set_portal_cursor(&mut state, cursor_hash, 2);
    state.zone.processed_deposit_cursor = ZoneProcessedDepositCursor::new(cursor_hash, 2);
    state.zone.next_withdrawal_index = 2;

    let first = failed_withdrawal(1, 3);
    let second = failed_withdrawal(2, 4);
    let withdrawals = [
        finalized_preimage(&first).clone(),
        finalized_preimage(&second).clone(),
    ];
    let members = BatchMembers::from_withdrawals(0, &withdrawals).unwrap();
    let boundary = BatchBoundary {
        first_zone_parent_hash: B256::ZERO,
        final_zone_block_hash: B256::repeat_byte(0x72),
        first_processed_deposit: DepositCursor {
            hash: B256::ZERO,
            number: 0,
        },
        final_processed_deposit: DepositCursor {
            hash: cursor_hash,
            number: 2,
        },
        final_imported_tempo_block_number: 9,
        final_zone_height: 10,
    };
    let finalized = FinalizedBatchState::new(boundary, members);
    let remaining = withdrawal_queue_hash(&withdrawals[1..]);
    let submitted =
        SubmittedBatchState::new(finalized, PortalQueueId::new(portal(), U256::ZERO).unwrap())
            .unwrap()
            .advance_partial(1, remaining)
            .unwrap();

    state.withdrawals.insert(withdrawal_id(1), second);
    state.batches.insert(
        BatchId {
            zone_id: ZONE_ID,
            withdrawal_batch_index: NonZeroU64::new(1).unwrap(),
        },
        BatchOwner::Submitted(submitted),
    );
    state.zone.last_batch = ZoneLastBatch::for_test(members.withdrawal_queue_hash(), 1);
    state.zone.batch_start = BatchStart {
        first_zone_parent_hash: boundary.final_zone_block_hash,
        first_processed_deposit: ZoneProcessedDepositCursor::new(cursor_hash, 2),
        first_withdrawal_index: 2,
    };
    let PortalLifecycle::Created(portal_state) = &mut state.portal else {
        unreachable!()
    };
    portal_state.settlement = PortalSettlementState {
        withdrawal_batch_index: 1,
        block_hash: boundary.final_zone_block_hash,
        last_synced_tempo_block_number: boundary.final_imported_tempo_block_number,
        last_submitted_deposit_cursor: boundary.final_processed_deposit,
        zone_height: U256::from(boundary.final_zone_height),
        withdrawal_queue_head: U256::ZERO,
        withdrawal_queue_tail: U256::ONE,
    };
    state
}

pub(super) fn finalized_preimage(owner: &WithdrawalOwner) -> &Withdrawal {
    let WithdrawalOwner::Finalized(finalized) = owner else {
        unreachable!()
    };
    finalized.preimage()
}
