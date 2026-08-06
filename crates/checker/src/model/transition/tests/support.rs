use std::num::{NonZeroU64, NonZeroU128};

use alloy_primitives::{Address, B256, Bytes, FixedBytes, U256};

use super::super::{ModelError, ModelTransition};
use crate::model::{
    encoding::{
        CompressedYParity, DepositPayload, DepositQueueMember, OrdinaryDeposit,
        UserWithdrawalRequest, WithdrawalBounceBackDeposit,
    },
    input::{
        BatchFinalizationInput, ImportedTempoBlockInput, ImportedTempoOperation,
        PortalCreationInput, TokenEnable, UserWithdrawalInput, ZoneBlockContext, ZoneBlockInput,
        ZoneDepositPrefixInput, ZoneOperation,
    },
    output::ExpectedOutputs,
    ownership::{FallbackId, FallbackOwner, WithdrawalId},
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
    OrdinaryDeposit::new(
        token,
        Address::repeat_byte(seed),
        amount,
        Address::repeat_byte(seed.wrapping_add(1)),
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
    let imported = ImportedTempoBlockInput::new(0, vec![creation_operation(initial_token)]);
    let zone =
        ZoneDepositPrefixInput::new(vec![enable(initial_token, "INIT")], Vec::new(), Vec::new());
    commit(&mut state, &imported, &zone).unwrap();
    state
}

pub(super) fn append_operations(members: &[DepositQueueMember]) -> Vec<ImportedTempoOperation> {
    members
        .iter()
        .map(|member| match member {
            DepositQueueMember::Ordinary(input) => {
                ImportedTempoOperation::OrdinaryDepositAppended(input.clone())
            }
            DepositQueueMember::WithdrawalBounceBack(input) => {
                ImportedTempoOperation::WithdrawalBounceBackAppended(*input)
            }
        })
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
    ImportedTempoBlockInput::new(0, Vec::new())
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
    let imported = ImportedTempoBlockInput::new(block_number, Vec::new());
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
