use std::num::{NonZeroU64, NonZeroU128};

use alloy_primitives::{Address, B256, Bytes, FixedBytes, U256};

use super::super::{ModelError, ModelTransition};
use crate::model::{
    accounting::TokenAccounting,
    encoding::{
        CompressedYParity, DepositPayload, DepositQueueMember, OrdinaryDeposit,
        UserWithdrawalRequest, Withdrawal, WithdrawalBounceBackDeposit, withdrawal_queue_hash,
    },
    input::{
        AuthenticatedDepositOutcome, AuthenticatedWithdrawalOutcome, BatchBlockTransitionInput,
        BatchDepositTransitionInput, BatchFinalizationInput, BatchSubmissionInput,
        ImportedTempoBlockInput, ImportedTempoOperation, PortalCreationInput, TokenEnable,
        UserWithdrawalInput, WithdrawalProcessingInput, ZoneBlockContext, ZoneBlockInput,
        ZoneDepositPrefixInput, ZoneOperation,
    },
    output::ExpectedOutputs,
    ownership::{
        BatchId, BatchOwner, DepositId, FallbackId, FallbackOwner, FinalizedBatchState,
        WithdrawalId, WithdrawalIdentity, WithdrawalOwner,
    },
    state::{ModelState, PortalIdentity, portal_address_for_zone},
};

pub(super) const ZONE_ID: u32 = 7;

pub(super) fn token(byte: u8) -> Address {
    Address::repeat_byte(byte)
}

pub(super) fn portal() -> Address {
    portal_address_for_zone(ZONE_ID)
}

pub(super) fn identity(initial_token: Address) -> PortalIdentity {
    PortalIdentity::new(portal(), ZONE_ID, initial_token)
}

pub(super) fn enable(token: Address, label: &str) -> TokenEnable {
    TokenEnable::new(token, format!("{label} Token"), label, "USD")
}

pub(super) fn ordinary(token: Address, seed: u8, amount: u128) -> OrdinaryDeposit {
    ordinary_with_refund_recipient(
        token,
        seed,
        amount,
        Address::repeat_byte(seed.wrapping_add(1)),
    )
}

pub(super) fn ordinary_with_refund_recipient(
    token: Address,
    seed: u8,
    amount: u128,
    tempo_refund_recipient: Address,
) -> OrdinaryDeposit {
    OrdinaryDeposit::new(
        token,
        Address::repeat_byte(seed),
        amount,
        tempo_refund_recipient,
        U256::from(seed),
        DepositPayload::new(
            B256::repeat_byte(seed.wrapping_add(2)),
            if seed.is_multiple_of(2) {
                CompressedYParity::Even
            } else {
                CompressedYParity::Odd
            },
            FixedBytes::repeat_byte(seed.wrapping_add(3)),
            FixedBytes::repeat_byte(seed.wrapping_add(4)),
            FixedBytes::repeat_byte(seed.wrapping_add(5)),
        ),
    )
}

pub(super) fn bounce(token: Address, nonce: u64, amount: u128) -> WithdrawalBounceBackDeposit {
    WithdrawalBounceBackDeposit::new(
        token,
        NonZeroU64::new(nonce).unwrap(),
        NonZeroU128::new(amount).unwrap(),
    )
}

pub(super) fn creation_operation(initial_token: Address) -> ImportedTempoOperation {
    ImportedTempoOperation::Create(PortalCreationInput::new(
        identity(initial_token),
        enable(initial_token, "INIT"),
    ))
}

pub(super) fn created_state(initial_token: Address) -> ModelState {
    let mut state = ModelState::awaiting_creation(identity(initial_token));
    let imported = ImportedTempoBlockInput::new(
        0,
        alloy_primitives::U256::ZERO,
        vec![creation_operation(initial_token)],
    );
    let zone =
        ZoneDepositPrefixInput::new(vec![enable(initial_token, "INIT")], Vec::new(), Vec::new());
    commit(&mut state, &imported, &zone).unwrap();
    state
}

pub(super) fn ordinary_members(deposits: &[OrdinaryDeposit]) -> Vec<DepositQueueMember> {
    deposits
        .iter()
        .cloned()
        .map(DepositQueueMember::Ordinary)
        .collect()
}

pub(super) fn ordinary_append_operations(
    deposits: &[OrdinaryDeposit],
) -> Vec<ImportedTempoOperation> {
    deposits
        .iter()
        .cloned()
        .map(ImportedTempoOperation::OrdinaryDepositAppended)
        .collect()
}

pub(super) fn commit(
    state: &mut ModelState,
    imported: &ImportedTempoBlockInput,
    zone: &ZoneDepositPrefixInput,
) -> Result<ExpectedOutputs, ModelError> {
    let completed = ModelTransition::new(state)
        .apply_imported_tempo_block(imported)?
        .apply_zone_block(&advance_only_block(zone))?;
    let (next, expected) = completed.materialize_for_test();
    *state = next;
    Ok(expected)
}

pub(super) fn empty_import() -> ImportedTempoBlockInput {
    ImportedTempoBlockInput::new(0, alloy_primitives::U256::ZERO, Vec::new())
}

pub(super) fn empty_zone() -> ZoneDepositPrefixInput {
    ZoneDepositPrefixInput::default()
}

pub(super) fn advance_only_block(advance: &ZoneDepositPrefixInput) -> ZoneBlockInput {
    ZoneBlockInput::new(
        ZoneBlockContext::new(B256::ZERO, 0),
        advance.clone(),
        Vec::new(),
        None,
    )
}

pub(super) fn user_withdrawal(
    token: Address,
    seed: u8,
    amount: u128,
    gas_limit: u64,
    reveal_to: Bytes,
) -> UserWithdrawalInput {
    UserWithdrawalInput::new(
        Address::repeat_byte(seed),
        B256::repeat_byte(seed.wrapping_add(1)),
        UserWithdrawalRequest::new(
            token,
            Address::repeat_byte(seed.wrapping_add(2)),
            amount,
            B256::repeat_byte(seed.wrapping_add(3)),
            gas_limit,
            Bytes::from(vec![seed; usize::from(seed % 3)]),
        )
        .unwrap(),
        reveal_to,
    )
}

pub(super) fn zone_block(
    block_number: u64,
    operations: Vec<ZoneOperation>,
    finalization: Option<BatchFinalizationInput>,
) -> ZoneBlockInput {
    ZoneBlockInput::new(
        ZoneBlockContext::new(B256::repeat_byte(block_number as u8), block_number),
        ZoneDepositPrefixInput::default(),
        operations,
        finalization,
    )
}

pub(super) fn commit_block(
    state: &mut ModelState,
    block_number: u64,
    operations: Vec<ZoneOperation>,
    finalization: Option<BatchFinalizationInput>,
) -> Result<ExpectedOutputs, ModelError> {
    let imported =
        ImportedTempoBlockInput::new(block_number, alloy_primitives::U256::ZERO, Vec::new());
    commit_full_block(
        state,
        &imported,
        &zone_block(block_number, operations, finalization),
    )
}

pub(super) fn apply_full_block(
    state: &ModelState,
    imported: &ImportedTempoBlockInput,
    zone: &ZoneBlockInput,
) -> Result<(ModelState, ExpectedOutputs), ModelError> {
    let completed = ModelTransition::new(state)
        .apply_imported_tempo_block(imported)?
        .apply_zone_block(zone)?;
    Ok(completed.materialize_for_test())
}

pub(super) fn commit_full_block(
    state: &mut ModelState,
    imported: &ImportedTempoBlockInput,
    zone: &ZoneBlockInput,
) -> Result<ExpectedOutputs, ModelError> {
    let (next, expected) = apply_full_block(state, imported, zone)?;
    *state = next;
    Ok(expected)
}

pub(super) fn seed_fallback(
    state: &mut ModelState,
    withdrawal_index: u64,
    nonce: u64,
    token: Address,
    amount: u128,
) -> WithdrawalId {
    let withdrawal = WithdrawalId {
        zone_id: ZONE_ID,
        withdrawal_index,
    };
    state.seed_fallback_owner_for_test(
        FallbackId {
            zone_id: ZONE_ID,
            fallback_nonce: NonZeroU64::new(nonce).unwrap(),
        },
        FallbackOwner::Held {
            withdrawal,
            token,
            amount: NonZeroU128::new(amount).unwrap(),
        },
    );
    withdrawal
}

pub(super) fn batch_id(index: u64) -> BatchId {
    BatchId {
        zone_id: ZONE_ID,
        withdrawal_batch_index: NonZeroU64::new(index).unwrap(),
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

pub(super) fn deposit_id(number: u64) -> DepositId {
    DepositId {
        portal: portal(),
        deposit_number: NonZeroU64::new(number).unwrap(),
    }
}

pub(super) fn funded_state(token: Address, supply: U256) -> ModelState {
    let mut state = created_state(token);
    state.set_token_accounting_for_test(
        token,
        TokenAccounting {
            supply,
            ..TokenAccounting::ZERO
        },
    );
    state
}

pub(super) fn empty_sender_finalization(
    block_number: u64,
    member_count: usize,
) -> BatchFinalizationInput {
    BatchFinalizationInput::new(member_count, block_number, vec![Bytes::new(); member_count])
}

pub(super) fn commit_imported(
    state: &mut ModelState,
    tempo_block_number: u64,
    base_fee: U256,
    operations: Vec<ImportedTempoOperation>,
) -> Result<ExpectedOutputs, ModelError> {
    commit(
        state,
        &ImportedTempoBlockInput::new(tempo_block_number, base_fee, operations),
        &empty_zone(),
    )
}

pub(super) fn withdrawals_processed(
    withdrawals: Vec<Withdrawal>,
    remaining_queue: B256,
    outcomes: Vec<AuthenticatedWithdrawalOutcome>,
) -> ImportedTempoOperation {
    ImportedTempoOperation::WithdrawalsProcessed(Box::new(WithdrawalProcessingInput::new(
        withdrawals,
        remaining_queue,
        outcomes,
    )))
}

pub(super) fn user_delivered_outcomes(count: usize) -> Vec<AuthenticatedWithdrawalOutcome> {
    (0..count)
        .map(|_| AuthenticatedWithdrawalOutcome::user_delivered(Vec::new()))
        .collect()
}

pub(super) fn finalize_initial_token_users(
    state: &mut ModelState,
    block_number: u64,
    users: &[(u8, u128, u64)],
) -> BatchId {
    let token = state
        .portal()
        .created()
        .expect("fixture Portal must be created")
        .identity()
        .initial_token();
    let operations = users
        .iter()
        .map(|&(seed, amount, gas_limit)| {
            ZoneOperation::user_withdrawal_accepted(user_withdrawal(
                token,
                seed,
                amount,
                gas_limit,
                Bytes::new(),
            ))
        })
        .collect();
    let output = commit_block(
        state,
        block_number,
        operations,
        Some(empty_sender_finalization(block_number, users.len())),
    )
    .unwrap();
    output.zone_block().finalized_batch().unwrap().batch()
}

pub(super) fn finalized_batch(state: &ModelState, batch: BatchId) -> &FinalizedBatchState {
    let Some(BatchOwner::Finalized(batch)) = state.batch(batch) else {
        panic!("fixture batch must be finalized")
    };
    batch
}

pub(super) fn exact_submission(state: &ModelState, batch: BatchId) -> BatchSubmissionInput {
    let finalized = finalized_batch(state, batch);
    let boundary = finalized.boundary();
    BatchSubmissionInput::new(
        boundary.final_imported_tempo_block_number,
        BatchBlockTransitionInput::new(
            boundary.first_zone_parent_hash,
            boundary.final_zone_block_hash,
        ),
        BatchDepositTransitionInput::new(
            boundary.first_processed_deposit,
            boundary.final_processed_deposit,
        ),
        finalized.members().withdrawal_queue_hash(),
        U256::from(boundary.final_zone_height),
    )
}

pub(super) fn submit_finalized_batch(state: &mut ModelState, batch: BatchId) -> ExpectedOutputs {
    let submission = exact_submission(state, batch);
    commit_imported(
        state,
        10_000 + batch.withdrawal_batch_index.get(),
        U256::ZERO,
        vec![ImportedTempoOperation::BatchSubmitted(Box::new(submission))],
    )
    .unwrap()
}

/// Drive user requests through acceptance, finalization, and Portal submission.
/// The returned IDs/preimages are the exact submitted FIFO members, ready for
/// a production `UserBounced` processing operation.
pub(super) fn prepare_submitted_users(
    state: &mut ModelState,
    block_number: u64,
    users: &[(u8, u128, u64)],
) -> (BatchId, Vec<(WithdrawalId, Withdrawal)>) {
    let first_withdrawal_index = state.zone().next_withdrawal_index();
    let batch = finalize_initial_token_users(state, block_number, users);
    let withdrawals = (0..users.len())
        .map(|offset| {
            let offset = u64::try_from(offset).expect("fixture length must fit u64");
            let id = withdrawal_id(
                first_withdrawal_index
                    .checked_add(offset)
                    .expect("fixture withdrawal range must fit u64"),
            );
            let Some(WithdrawalOwner::Finalized(finalized)) = state.withdrawal(id) else {
                panic!("fixture withdrawal must be finalized")
            };
            (id, finalized.preimage().clone())
        })
        .collect();
    submit_finalized_batch(state, batch);
    (batch, withdrawals)
}

pub(super) fn finalized_preimage(state: &ModelState, index: u64) -> Withdrawal {
    let Some(WithdrawalOwner::Finalized(finalized)) = state.withdrawal(withdrawal_id(index)) else {
        panic!("fixture withdrawal must be finalized")
    };
    finalized.preimage().clone()
}

pub(super) struct SubmittedFailedDepositBatch {
    pub(super) state: ModelState,
    pub(super) batch: BatchId,
    pub(super) withdrawals: Vec<Withdrawal>,
    pub(super) origins: Vec<DepositId>,
    pub(super) deposits: Vec<OrdinaryDeposit>,
}

pub(super) fn submitted_failed_deposit_batch(
    token: Address,
    amounts: &[u128],
) -> SubmittedFailedDepositBatch {
    let deposits = amounts
        .iter()
        .enumerate()
        .map(|(index, &amount)| {
            let offset = u8::try_from(index).expect("fixture deposit count must fit u8");
            let seed = 0x80_u8
                .checked_add(offset)
                .expect("fixture supports at most 128 failed deposits");
            ordinary(token, seed, amount)
        })
        .collect::<Vec<_>>();
    submitted_failed_deposits(token, deposits)
}

pub(super) fn submitted_failed_deposits(
    token: Address,
    deposits: Vec<OrdinaryDeposit>,
) -> SubmittedFailedDepositBatch {
    assert!(deposits.iter().all(|deposit| deposit.token() == token));
    let mut state = created_state(token);
    let members = ordinary_members(&deposits);
    let imported =
        ImportedTempoBlockInput::new(51, U256::ZERO, ordinary_append_operations(&deposits));
    let zone = ZoneBlockInput::new(
        ZoneBlockContext::new(B256::repeat_byte(0x83), 51),
        ZoneDepositPrefixInput::new(
            Vec::new(),
            members,
            vec![AuthenticatedDepositOutcome::OrdinaryFailed; deposits.len()],
        ),
        Vec::new(),
        Some(empty_sender_finalization(51, deposits.len())),
    );
    let output = commit_full_block(&mut state, &imported, &zone).unwrap();
    let batch = output.zone_block().finalized_batch().unwrap().batch();

    let mut withdrawals = Vec::with_capacity(deposits.len());
    let mut origins = Vec::with_capacity(deposits.len());
    for index in 0..deposits.len() {
        let withdrawal_index = u64::try_from(index).expect("fixture withdrawal count must fit u64");
        let Some(WithdrawalOwner::Finalized(withdrawal)) =
            state.withdrawal(withdrawal_id(withdrawal_index))
        else {
            panic!("fixture withdrawal must be finalized")
        };
        let WithdrawalIdentity::FailedDeposit { deposit } = withdrawal.identity() else {
            panic!("fixture withdrawal must retain a failed-deposit origin")
        };
        withdrawals.push(withdrawal.preimage().clone());
        origins.push(deposit);
    }
    assert_eq!(
        withdrawal_queue_hash(&withdrawals),
        finalized_batch(&state, batch)
            .members()
            .withdrawal_queue_hash()
    );
    submit_finalized_batch(&mut state, batch);

    SubmittedFailedDepositBatch {
        state,
        batch,
        withdrawals,
        origins,
        deposits,
    }
}

pub(super) fn reject_imported_atomically(
    state: &mut ModelState,
    base_fee: U256,
    operations: Vec<ImportedTempoOperation>,
) -> ModelError {
    let before = state.clone();
    let error = commit_imported(state, 20_000, base_fee, operations).unwrap_err();
    assert_eq!(*state, before);
    error
}

pub(super) fn assert_queue_progress(state: &ModelState, head: U256, tail: U256) {
    let settlement = state.portal().created().unwrap().settlement();
    assert_eq!(settlement.withdrawal_queue_head(), head);
    assert_eq!(settlement.withdrawal_queue_tail(), tail);
}
