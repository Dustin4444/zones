use alloy_primitives::{B256, U256};

use super::support::*;
use crate::model::{
    accounting::TokenAccounting,
    constants::{BOUNCE_BACK_BASE_FEE_SCALE, EMPTY_WITHDRAWAL_QUEUE_SENTINEL},
    encoding::{DepositQueueMember, WithdrawalQueueError, withdrawal_queue_hash},
    input::{AuthenticatedWithdrawalOutcome, ImportedTempoOperation},
    output::{
        ExpectedImportedTempoOperation, ExpectedOutputs, ExpectedProcessedWithdrawal,
        ExpectedWithdrawalProcessing,
    },
    ownership::{BatchOwner, DepositOwner, FallbackOwner, PortalRefundId, PortalRefundOwner},
    state::ModelState,
    transition::{ModelError, WithdrawalOriginKind, WithdrawalProcessingOutcomeKind},
};

fn only_withdrawal_processing(output: &ExpectedOutputs) -> &ExpectedWithdrawalProcessing {
    let [ExpectedImportedTempoOperation::WithdrawalsProcessed(processing)] =
        output.imported_tempo_block().operations()
    else {
        panic!("one withdrawal-processing output expected")
    };
    processing
}

fn assert_bad_queue_rejected(state: &mut ModelState, operation: ImportedTempoOperation) {
    let error = reject_imported_atomically(state, U256::ZERO, vec![operation]);
    assert!(matches!(
        error,
        ModelError::WithdrawalQueue(WithdrawalQueueError::CommitmentMismatch { .. })
    ));
}

#[test]
fn three_member_batch_advances_by_exact_prefix_then_exhausts() {
    let token = token(0xa3);
    let mut state = funded_state(token, U256::from(1_000_000));
    let batch = finalize_initial_token_users(
        &mut state,
        21,
        &[(0x31, 10, 0), (0x32, 20, 0), (0x33, 30, 0)],
    );
    submit_finalized_batch(&mut state, batch);
    let withdrawals = [
        finalized_preimage(&state, 0),
        finalized_preimage(&state, 1),
        finalized_preimage(&state, 2),
    ];
    let suffix = withdrawal_queue_hash(&withdrawals[2..]);

    let output = commit_imported(
        &mut state,
        22,
        U256::ZERO,
        vec![withdrawals_processed(
            withdrawals[..2].to_vec(),
            suffix,
            user_delivered_outcomes(2),
        )],
    )
    .unwrap();
    let processed = only_withdrawal_processing(&output);
    assert_eq!(processed.members().len(), 2);
    let Some(BatchOwner::Submitted(submitted)) = state.batch(batch) else {
        panic!("partial batch must remain submitted")
    };
    assert_eq!(submitted.next_processing_ordinal(), 2);
    assert_eq!(submitted.remaining_queue_hash(), suffix);
    assert!(state.withdrawal(withdrawal_id(0)).is_none());
    assert!(state.withdrawal(withdrawal_id(1)).is_none());
    assert!(state.withdrawal(withdrawal_id(2)).is_some());
    assert_eq!(
        state
            .portal()
            .created()
            .unwrap()
            .settlement()
            .withdrawal_queue_head(),
        U256::ZERO
    );

    commit_imported(
        &mut state,
        23,
        U256::ZERO,
        vec![withdrawals_processed(
            vec![withdrawals[2].clone()],
            B256::ZERO,
            user_delivered_outcomes(1),
        )],
    )
    .unwrap();
    assert!(state.batch(batch).is_none());
    assert!(state.withdrawals().is_empty());
    let settlement = state.portal().created().unwrap().settlement();
    assert_eq!(settlement.withdrawal_queue_head(), U256::ONE);
    assert_eq!(settlement.withdrawal_queue_tail(), U256::ONE);
    assert_eq!(
        state
            .token(token)
            .unwrap()
            .accounting()
            .withdrawal_liability,
        U256::ZERO
    );
}

#[test]
fn bad_suffix_preimage_order_and_late_outcome_fail_atomically() {
    let token = token(0xa4);
    let mut state = funded_state(token, U256::from(1_000_000));
    let batch = finalize_initial_token_users(
        &mut state,
        31,
        &[(0x41, 10, 0), (0x42, 20, 0), (0x43, 30, 0)],
    );
    submit_finalized_batch(&mut state, batch);
    let withdrawals = [
        finalized_preimage(&state, 0),
        finalized_preimage(&state, 1),
        finalized_preimage(&state, 2),
    ];

    assert_bad_queue_rejected(
        &mut state,
        withdrawals_processed(
            withdrawals[..2].to_vec(),
            B256::repeat_byte(0x51),
            user_delivered_outcomes(2),
        ),
    );

    assert_bad_queue_rejected(
        &mut state,
        withdrawals_processed(
            vec![withdrawals[1].clone()],
            withdrawal_queue_hash(&withdrawals[1..]),
            user_delivered_outcomes(1),
        ),
    );

    assert_bad_queue_rejected(
        &mut state,
        withdrawals_processed(
            vec![
                withdrawals[1].clone(),
                withdrawals[0].clone(),
                withdrawals[2].clone(),
            ],
            B256::ZERO,
            user_delivered_outcomes(3),
        ),
    );

    let suffix = withdrawal_queue_hash(&withdrawals[2..]);
    let error = reject_imported_atomically(
        &mut state,
        U256::ZERO,
        vec![withdrawals_processed(
            withdrawals[..2].to_vec(),
            suffix,
            vec![
                AuthenticatedWithdrawalOutcome::user_delivered(Vec::new()),
                AuthenticatedWithdrawalOutcome::FailedDepositPaid,
            ],
        )],
    );
    assert_eq!(
        error,
        ModelError::WithdrawalProcessingOutcomeMismatch {
            withdrawal_index: 1,
            expected: WithdrawalOriginKind::User,
            actual: WithdrawalProcessingOutcomeKind::FailedDepositPaid,
        }
    );
}

#[test]
fn empty_processing_with_random_or_sentinel_suffix_is_an_exact_noop() {
    let token = token(0xa5);
    let mut initial = funded_state(token, U256::from(1_000_000));
    let (batch, _) = prepare_submitted_users(&mut initial, 40, &[(0x61, 25, 0)]);
    commit_imported(
        &mut initial,
        20_000,
        U256::ZERO,
        vec![ImportedTempoOperation::BouncebackGasUpdated(77)],
    )
    .unwrap();
    assert!(matches!(
        initial.batch(batch),
        Some(BatchOwner::Submitted(_))
    ));
    assert_eq!(
        initial
            .portal()
            .created()
            .unwrap()
            .settlement()
            .withdrawal_queue_tail(),
        U256::ONE
    );
    assert_eq!(
        initial
            .portal()
            .created()
            .unwrap()
            .config()
            .bounceback_gas(),
        77
    );

    for remaining_queue in [B256::repeat_byte(0x61), EMPTY_WITHDRAWAL_QUEUE_SENTINEL] {
        let mut state = initial.clone();
        let output = commit_imported(
            &mut state,
            40,
            U256::ZERO,
            vec![withdrawals_processed(
                Vec::new(),
                remaining_queue,
                Vec::new(),
            )],
        )
        .unwrap();
        assert_eq!(state, initial);
        let processed = only_withdrawal_processing(&output);
        assert!(processed.members().is_empty());
    }
}

#[test]
fn processing_requires_exactly_one_outcome_per_supplied_withdrawal() {
    let token = token(0xac);
    let mut state = funded_state(token, U256::from(1_000_000));
    let (_, withdrawals) = prepare_submitted_users(&mut state, 41, &[(0xc1, 25, 0)]);

    for outcomes in [Vec::new(), user_delivered_outcomes(2)] {
        let actual = outcomes.len();
        assert_eq!(
            reject_imported_atomically(
                &mut state,
                U256::ZERO,
                vec![withdrawals_processed(
                    vec![withdrawals[0].1.clone()],
                    B256::ZERO,
                    outcomes,
                )],
            ),
            ModelError::WithdrawalProcessingOutcomeCountMismatch {
                withdrawals: 1,
                outcomes: actual,
            }
        );
    }
}

#[test]
fn user_delivery_and_bounce_close_the_right_owners_and_only_delivery_reduces_w() {
    let token = token(0xa6);
    let mut state = funded_state(token, U256::from(1_000_000));
    let batch = finalize_initial_token_users(&mut state, 41, &[(0x71, 30, 0), (0x72, 40, 0)]);
    submit_finalized_batch(&mut state, batch);
    let withdrawals = [finalized_preimage(&state, 0), finalized_preimage(&state, 1)];
    let accounting_before = state.token(token).unwrap().accounting();

    let output = commit_imported(
        &mut state,
        42,
        U256::ZERO,
        vec![withdrawals_processed(
            withdrawals.to_vec(),
            B256::ZERO,
            vec![
                AuthenticatedWithdrawalOutcome::user_delivered(Vec::new()),
                AuthenticatedWithdrawalOutcome::UserBounced,
            ],
        )],
    )
    .unwrap();
    let processed = only_withdrawal_processing(&output);
    let [
        ExpectedProcessedWithdrawal::UserDelivered(delivered),
        ExpectedProcessedWithdrawal::UserBounced(bounced),
    ] = processed.members()
    else {
        panic!("delivery then bounce outcomes required")
    };
    assert!(delivered.callback_deposit_appends().is_empty());
    let delivered_event = delivered.processed();
    assert_eq!(delivered_event.withdrawal(), withdrawal_id(0));
    assert_eq!(delivered_event.to(), withdrawals[0].to());
    assert_eq!(delivered_event.sender_tag(), withdrawals[0].sender_tag());
    assert_eq!(delivered_event.token(), token);
    assert_eq!(delivered_event.amount(), 30);
    assert!(delivered_event.callback_success());

    let bounced_event = bounced.second();
    assert_eq!(bounced_event.withdrawal(), withdrawal_id(1));
    assert_eq!(bounced_event.to(), withdrawals[1].to());
    assert_eq!(bounced_event.sender_tag(), withdrawals[1].sender_tag());
    assert_eq!(bounced_event.token(), token);
    assert_eq!(bounced_event.amount(), 40);
    assert!(!bounced_event.callback_success());

    let bounce = bounced.first();
    assert_eq!(bounce.deposit().fallback_nonce().get(), 2);
    assert_eq!(bounce.deposit().token(), token);
    assert_eq!(bounce.deposit().amount().get(), 40);

    assert!(state.withdrawals().is_empty());
    assert!(state.fallback_owner(fallback_id(1)).is_none());
    let bounced_deposit = bounce.append().id();
    assert_eq!(bounced_deposit, deposit_id(1));
    assert_eq!(
        bounce.append().queue_hash(),
        DepositQueueMember::WithdrawalBounceBack(bounce.deposit()).hash_after(B256::ZERO)
    );
    assert!(matches!(
        state.fallback_owner(fallback_id(2)),
        Some(FallbackOwner::BounceBackQueued {
            withdrawal,
            token: owned_token,
            amount,
            deposit,
        }) if *withdrawal == withdrawal_id(1)
            && *owned_token == token
            && amount.get() == 40
            && *deposit == bounced_deposit
    ));
    assert!(matches!(
        state.pending_deposit(bounced_deposit),
        Some(DepositOwner::PendingWithdrawalBounceBack { withdrawal, preimage })
            if *withdrawal == withdrawal_id(1)
                && preimage == &bounced.first().deposit()
    ));
    assert_eq!(
        state.token(token).unwrap().accounting(),
        TokenAccounting {
            withdrawal_liability: accounting_before.withdrawal_liability - U256::from(30),
            ..accounting_before
        }
    );
}

#[test]
fn failed_deposit_paid_and_pending_use_same_block_fee_and_distinct_d_rows() {
    let token = token(0xa7);
    let fixture = submitted_failed_deposit_batch(token, &[10, 11]);
    let mut state = fixture.state;
    let deposits = fixture.deposits;
    let withdrawals = fixture.withdrawals;

    let base_fee = U256::from(BOUNCE_BACK_BASE_FEE_SCALE / 2);
    let output = commit_imported(
        &mut state,
        52,
        base_fee,
        vec![
            ImportedTempoOperation::BouncebackGasUpdated(3),
            withdrawals_processed(
                withdrawals.to_vec(),
                B256::ZERO,
                vec![
                    AuthenticatedWithdrawalOutcome::FailedDepositPaid,
                    AuthenticatedWithdrawalOutcome::FailedDepositPending,
                ],
            ),
        ],
    )
    .unwrap();
    let processed = only_withdrawal_processing(&output);
    let [
        ExpectedProcessedWithdrawal::FailedDepositPaid(paid),
        ExpectedProcessedWithdrawal::FailedDepositPending(pending),
    ] = processed.members()
    else {
        panic!("direct then pending failed-deposit outcomes required")
    };
    assert_eq!(paid.failed_deposit(), deposit_id(1));
    assert_eq!(paid.recipient(), deposits[0].tempo_refund_recipient());
    assert_eq!(paid.token(), token);
    assert_eq!(paid.amount(), 8);
    assert_eq!(paid.bounceback_fee(), 2);
    assert_eq!(pending.failed_deposit(), deposit_id(2));
    assert_eq!(pending.recipient(), deposits[1].tempo_refund_recipient());
    assert_eq!(pending.token(), token);
    assert_eq!(pending.amount(), 9);
    assert_eq!(pending.bounceback_fee(), 2);

    let paid_refund = PortalRefundId {
        token,
        recipient: deposits[0].tempo_refund_recipient(),
        failed_deposit: deposit_id(1),
    };
    let pending_refund = PortalRefundId {
        token,
        recipient: deposits[1].tempo_refund_recipient(),
        failed_deposit: deposit_id(2),
    };
    assert!(state.portal_refund(paid_refund).is_none());
    assert_eq!(
        state.portal_refund(pending_refund),
        Some(&PortalRefundOwner::Pending { amount: 9 })
    );
    assert_eq!(
        state.token(token).unwrap().accounting(),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(9),
            withdrawal_liability: U256::ZERO,
        }
    );
    assert!(state.withdrawals().is_empty());
}

#[test]
fn same_block_submit_process_preserves_callback_append_order() {
    let token = token(0xa8);
    let mut state = funded_state(token, U256::from(1_000_000));
    let batch = finalize_initial_token_users(&mut state, 61, &[(0x91, 25, 100)]);
    let submission = exact_submission(&state, batch);
    let withdrawal = finalized_preimage(&state, 0);
    let callbacks = [ordinary(token, 0x92, 5), ordinary(token, 0x93, 7)];

    let output = commit_imported(
        &mut state,
        62,
        U256::ZERO,
        vec![
            ImportedTempoOperation::BatchSubmitted(Box::new(submission)),
            withdrawals_processed(
                vec![withdrawal],
                B256::ZERO,
                vec![AuthenticatedWithdrawalOutcome::user_delivered(
                    callbacks.to_vec(),
                )],
            ),
        ],
    )
    .unwrap();
    let [
        ExpectedImportedTempoOperation::BatchSubmitted(_),
        ExpectedImportedTempoOperation::WithdrawalsProcessed(processed),
    ] = output.imported_tempo_block().operations()
    else {
        panic!("same-block submission must precede processing")
    };
    let [ExpectedProcessedWithdrawal::UserDelivered(delivery)] = processed.members() else {
        panic!("one delivered user expected")
    };
    let [first, second] = delivery.callback_deposit_appends() else {
        panic!("two ordered callback appends expected")
    };
    let first_hash = DepositQueueMember::Ordinary(callbacks[0].clone()).hash_after(B256::ZERO);
    let second_hash = DepositQueueMember::Ordinary(callbacks[1].clone()).hash_after(first_hash);
    assert_eq!(first.id(), deposit_id(1));
    assert_eq!(first.queue_hash(), first_hash);
    assert_eq!(second.id(), deposit_id(2));
    assert_eq!(second.queue_hash(), second_hash);
    assert!(delivery.processed().callback_success());
    assert!(matches!(
        state.pending_deposit(deposit_id(1)),
        Some(DepositOwner::PendingOrdinary { preimage }) if preimage == &callbacks[0]
    ));
    assert!(matches!(
        state.pending_deposit(deposit_id(2)),
        Some(DepositOwner::PendingOrdinary { preimage }) if preimage == &callbacks[1]
    ));
    let portal_state = state.portal().created().unwrap();
    assert_eq!(portal_state.deposit_cursor().number(), 2);
    assert_eq!(portal_state.deposit_cursor().hash(), second_hash);
    assert!(state.batch(batch).is_none());
    assert!(state.withdrawal(withdrawal_id(0)).is_none());
    assert_eq!(portal_state.settlement().withdrawal_queue_head(), U256::ONE);
    assert_eq!(portal_state.settlement().withdrawal_queue_tail(), U256::ONE);
}

#[test]
fn plain_transfer_delivery_cannot_report_callback_deposits() {
    let token = token(0xaa);
    let mut state = funded_state(token, U256::from(1_000_000));
    let (_, withdrawals) = prepare_submitted_users(&mut state, 70, &[(0xa1, 25, 0)]);
    let preimage = withdrawals[0].1.clone();
    let callback = ordinary(token, 0xa2, 5);

    let error = reject_imported_atomically(
        &mut state,
        U256::ZERO,
        vec![withdrawals_processed(
            vec![preimage],
            B256::ZERO,
            vec![AuthenticatedWithdrawalOutcome::user_delivered(vec![
                callback,
            ])],
        )],
    );
    assert_eq!(
        error,
        ModelError::CallbackDepositsWithoutCallback {
            withdrawal_index: 0,
        }
    );
}
