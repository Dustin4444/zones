use alloy_primitives::{Address, B256, U256, b256};

use super::support::*;
use crate::model::{
    accounting::TokenAccounting,
    encoding::DepositQueueMember,
    input::{
        AuthenticatedDepositOutcome, AuthenticatedWithdrawalOutcome, ImportedTempoBlockInput,
        ImportedTempoOperation, RefundClaimInput, ZoneDepositPrefixInput, ZoneOperation,
    },
    ownership::{
        BatchOwner, DepositOwner, FallbackOwner, InboxRefundOwner, PendingWithdrawal,
        PortalRefundOwner, WithdrawalOwner,
    },
};

const AMOUNT: u128 = 40;

#[derive(Debug, Clone, Copy)]
enum FailedDepositTerminal {
    DirectRefund,
    PortalRefundClaim,
}

#[derive(Debug, Clone, Copy)]
enum UserBounceTerminal {
    Mint,
    InboxRefundClaim,
}

#[test]
fn release_one_lifecycle_matrix_reaches_all_six_terminals_without_lost_owners() {
    ordinary_mint_terminal();
    for terminal in [
        FailedDepositTerminal::DirectRefund,
        FailedDepositTerminal::PortalRefundClaim,
    ] {
        failed_deposit_terminal(terminal);
    }
    user_delivery_terminal();
    for terminal in [
        UserBounceTerminal::Mint,
        UserBounceTerminal::InboxRefundClaim,
    ] {
        user_bounce_back_terminal(terminal);
    }
}

fn ordinary_mint_terminal() {
    let token = token(0xd1);
    let deposit = ordinary(token, 0xd2, AMOUNT);
    let member = DepositQueueMember::Ordinary(deposit.clone());
    let mut state = created_state(token);

    commit(
        &mut state,
        &ImportedTempoBlockInput::new(
            1,
            U256::ZERO,
            vec![ImportedTempoOperation::OrdinaryDepositAppended(
                deposit.clone(),
            )],
        ),
        &empty_zone(),
    )
    .unwrap();
    assert!(matches!(
        state.pending_deposit(deposit_id(1)),
        Some(DepositOwner::PendingOrdinary { preimage }) if preimage == &deposit
    ));
    assert_accounting(&state, token, 0, AMOUNT, 0);

    commit(
        &mut state,
        &empty_import(),
        &ZoneDepositPrefixInput::new(
            Vec::new(),
            vec![member],
            vec![AuthenticatedDepositOutcome::OrdinaryMinted {
                recipient: Address::repeat_byte(0xd3),
                memo: B256::repeat_byte(0xd4),
            }],
        ),
    )
    .unwrap();

    assert!(state.pending_deposit(deposit_id(1)).is_none());
    assert_accounting(&state, token, AMOUNT, 0, 0);
    crate::model::validation::validate_authoritative(&state).unwrap();
}

fn failed_deposit_terminal(terminal: FailedDepositTerminal) {
    let pending_refund = matches!(terminal, FailedDepositTerminal::PortalRefundClaim);
    let token = token(if pending_refund { 0xd5 } else { 0xd6 });
    let deposit = ordinary(token, 0xde, AMOUNT);
    let member = DepositQueueMember::Ordinary(deposit.clone());
    let mut state = created_state(token);
    let origin = deposit_id(1);
    let withdrawal_id = withdrawal_id(0);
    let batch = batch_id(1);

    commit(
        &mut state,
        &ImportedTempoBlockInput::new(
            1,
            U256::ZERO,
            vec![ImportedTempoOperation::OrdinaryDepositAppended(
                deposit.clone(),
            )],
        ),
        &ZoneDepositPrefixInput::new(
            Vec::new(),
            vec![member],
            vec![AuthenticatedDepositOutcome::OrdinaryFailed],
        ),
    )
    .unwrap();
    assert!(state.pending_deposit(origin).is_none());
    assert!(matches!(
        state.withdrawal(withdrawal_id),
        Some(WithdrawalOwner::Pending(PendingWithdrawal::FailedDeposit(
            _
        )))
    ));
    assert_accounting(&state, token, 0, AMOUNT, 0);

    commit_block(
        &mut state,
        2,
        Vec::new(),
        Some(empty_sender_finalization(2, 1)),
    )
    .unwrap();
    assert!(matches!(
        state.withdrawal(withdrawal_id),
        Some(WithdrawalOwner::Finalized(_))
    ));
    assert!(matches!(state.batch(batch), Some(BatchOwner::Finalized(_))));

    submit_finalized_batch(&mut state, batch);
    assert!(matches!(state.batch(batch), Some(BatchOwner::Submitted(_))));
    let Some(WithdrawalOwner::Finalized(finalized)) = state.withdrawal(withdrawal_id) else {
        panic!("failed-deposit withdrawal must remain finalized while submitted")
    };
    let withdrawal = finalized.preimage().clone();
    assert_eq!(withdrawal.fallback_nonce(), 0);
    assert_eq!(
        withdrawal.sender_tag(),
        b256!("a86d54e9aab41ae5e520ff0062ff1b4cbd0b2192bb01080a058bb170d84e6457")
    );
    assert_accounting(&state, token, 0, AMOUNT, 0);

    let outcome = if pending_refund {
        AuthenticatedWithdrawalOutcome::FailedDepositPending
    } else {
        AuthenticatedWithdrawalOutcome::FailedDepositPaid
    };
    commit_imported(
        &mut state,
        20_001,
        U256::ZERO,
        vec![withdrawals_processed(
            vec![withdrawal],
            B256::ZERO,
            vec![outcome],
        )],
    )
    .unwrap();

    assert!(state.withdrawal(withdrawal_id).is_none());
    assert!(state.batch(batch).is_none());
    let refund = portal_refund_id(token, deposit.tempo_refund_recipient(), 1);
    if pending_refund {
        assert_eq!(
            state.portal_refund(refund),
            Some(&PortalRefundOwner::Pending { amount: AMOUNT })
        );
        assert_accounting(&state, token, 0, AMOUNT, 0);
        commit_imported(
            &mut state,
            20_002,
            U256::ZERO,
            vec![ImportedTempoOperation::PortalRefundClaimed(
                RefundClaimInput::new(deposit.tempo_refund_recipient(), token, AMOUNT),
            )],
        )
        .unwrap();
        assert!(state.portal_refund(refund).is_none());
    } else {
        assert!(state.portal_refund(refund).is_none());
    }
    assert_accounting(&state, token, 0, 0, 0);
    crate::model::validation::validate_authoritative(&state).unwrap();
}

fn user_delivery_terminal() {
    let token = token(0xd7);
    let (mut state, withdrawal) = submitted_user(token);
    let withdrawal_id = withdrawal_id(0);
    let fallback = fallback_id(1);

    commit_imported(
        &mut state,
        20_003,
        U256::ZERO,
        vec![withdrawals_processed(
            vec![withdrawal],
            B256::ZERO,
            vec![AuthenticatedWithdrawalOutcome::user_delivered(Vec::new())],
        )],
    )
    .unwrap();

    assert!(state.withdrawal(withdrawal_id).is_none());
    assert!(state.fallback_owner(fallback).is_none());
    assert!(state.batch(batch_id(1)).is_none());
    assert_accounting(&state, token, 60, 0, 0);
    crate::model::validation::validate_authoritative(&state).unwrap();
}

fn user_bounce_back_terminal(terminal: UserBounceTerminal) {
    let pending_refund = matches!(terminal, UserBounceTerminal::InboxRefundClaim);
    let token = token(if pending_refund { 0xd9 } else { 0xda });
    let recipient = Address::repeat_byte(if pending_refund { 0xdb } else { 0xdc });
    let (mut state, withdrawal) = submitted_user(token);
    let withdrawal_id = withdrawal_id(0);
    let fallback = fallback_id(1);

    commit_imported(
        &mut state,
        20_004,
        U256::ZERO,
        vec![withdrawals_processed(
            vec![withdrawal],
            B256::ZERO,
            vec![AuthenticatedWithdrawalOutcome::UserBounced],
        )],
    )
    .unwrap();
    assert!(state.withdrawal(withdrawal_id).is_none());
    assert!(matches!(
        state.pending_deposit(deposit_id(1)),
        Some(DepositOwner::PendingWithdrawalBounceBack { withdrawal, .. })
            if *withdrawal == withdrawal_id
    ));
    assert!(matches!(
        state.fallback_owner(fallback),
        Some(FallbackOwner::BounceBackQueued { withdrawal, deposit, .. })
            if *withdrawal == withdrawal_id && *deposit == deposit_id(1)
    ));
    assert_accounting(&state, token, 60, 0, AMOUNT);

    let outcome = if pending_refund {
        AuthenticatedDepositOutcome::WithdrawalBounceBackPending { recipient }
    } else {
        AuthenticatedDepositOutcome::WithdrawalBounceBackMinted { recipient }
    };
    commit(
        &mut state,
        &empty_import(),
        &ZoneDepositPrefixInput::new(
            Vec::new(),
            vec![DepositQueueMember::WithdrawalBounceBack(bounce(
                token, 1, AMOUNT,
            ))],
            vec![outcome],
        ),
    )
    .unwrap();

    assert!(state.pending_deposit(deposit_id(1)).is_none());
    assert!(state.fallback_owner(fallback).is_none());
    let refund = inbox_refund_id(token, recipient, 0);
    if pending_refund {
        assert!(matches!(
            state.inbox_refund(refund),
            Some(InboxRefundOwner::Pending { amount }) if amount.get() == AMOUNT
        ));
        assert_accounting(&state, token, 60, 0, AMOUNT);
        commit_block(
            &mut state,
            2,
            vec![ZoneOperation::InboxRefundClaimed(RefundClaimInput::new(
                recipient, token, AMOUNT,
            ))],
            None,
        )
        .unwrap();
        assert!(state.inbox_refund(refund).is_none());
    } else {
        assert!(state.inbox_refund(refund).is_none());
    }
    assert_accounting(&state, token, 100, 0, 0);
    crate::model::validation::validate_authoritative(&state).unwrap();
}

fn submitted_user(
    token: Address,
) -> (
    crate::model::state::ModelState,
    crate::model::encoding::Withdrawal,
) {
    let mut state = funded_state(token, U256::from(100));
    let withdrawal_id = withdrawal_id(0);
    let fallback = fallback_id(1);

    commit_block(
        &mut state,
        1,
        vec![ZoneOperation::user_withdrawal_accepted(user_withdrawal(
            token,
            0xdf,
            AMOUNT,
            0,
            Default::default(),
        ))],
        None,
    )
    .unwrap();
    assert!(matches!(
        state.withdrawal(withdrawal_id),
        Some(WithdrawalOwner::Pending(PendingWithdrawal::User(_)))
    ));
    assert!(matches!(
        state.fallback_owner(fallback),
        Some(FallbackOwner::Held { withdrawal, token: owner_token, amount })
            if *withdrawal == withdrawal_id && *owner_token == token && amount.get() == AMOUNT
    ));
    assert_accounting(&state, token, 60, 0, AMOUNT);

    commit_block(
        &mut state,
        2,
        Vec::new(),
        Some(empty_sender_finalization(2, 1)),
    )
    .unwrap();
    assert!(matches!(
        state.withdrawal(withdrawal_id),
        Some(WithdrawalOwner::Finalized(_))
    ));
    assert!(matches!(
        state.batch(batch_id(1)),
        Some(BatchOwner::Finalized(_))
    ));

    submit_finalized_batch(&mut state, batch_id(1));
    assert!(matches!(
        state.batch(batch_id(1)),
        Some(BatchOwner::Submitted(_))
    ));
    let Some(WithdrawalOwner::Finalized(finalized)) = state.withdrawal(withdrawal_id) else {
        panic!("submitted user withdrawal must remain finalized")
    };
    let withdrawal = finalized.preimage().clone();
    assert_eq!(withdrawal.fallback_nonce(), 1);
    (state, withdrawal)
}

fn assert_accounting(
    state: &crate::model::state::ModelState,
    token: Address,
    supply: u128,
    deposit: u128,
    withdrawal: u128,
) {
    assert_eq!(
        state.token(token).unwrap().accounting(),
        TokenAccounting {
            supply: U256::from(supply),
            deposit_liability: U256::from(deposit),
            withdrawal_liability: U256::from(withdrawal),
        }
    );
}
