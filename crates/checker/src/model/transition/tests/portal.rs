use alloy_primitives::{Address, B256, U256, address};

use super::support::*;
use crate::model::{
    accounting::{AccountingError, Component, TokenAccounting},
    input::{
        ImportedTempoBlockInput, ImportedTempoOperation, PortalCreationInput,
        ZoneDepositPrefixInput,
    },
    state::{
        ModelState, PortalDepositCursor, PortalIdentity, PortalLifecycle, TokenPhase,
        portal_address_for_zone,
    },
    transition::{ModelError, ModelTransition},
};

#[test]
fn portal_address_derivation_and_creation_install_literal_zero_state() {
    assert_eq!(
        portal_address_for_zone(1),
        address!("5AD0000000000000000000000000000000000001")
    );
    assert_eq!(
        portal_address_for_zone(0x0102_0304),
        address!("5AD0000000000000000000000000000001020304")
    );

    let initial = token(0x11);
    let mut state = ModelState::awaiting_creation(identity(initial));
    let imported = ImportedTempoBlockInput::new(
        0,
        alloy_primitives::U256::ZERO,
        vec![creation_operation(initial)],
    );
    let zone = ZoneDepositPrefixInput::new(vec![enable(initial, "INIT")], Vec::new(), Vec::new());
    let expected = commit(&mut state, &imported, &zone).unwrap();

    let PortalLifecycle::Created(portal) = state.portal() else {
        panic!("creation must move the Portal phase")
    };
    assert_eq!(portal.identity(), identity(initial));
    assert_eq!(portal.config().bounceback_gas(), 0);
    assert_eq!(portal.deposit_cursor(), PortalDepositCursor::ZERO);
    assert_eq!(state.zone().config().tempo_gas_rate(), 0);
    assert_eq!(state.zone().config().max_withdrawals_per_block(), 0);
    assert_eq!(state.zone().last_fallback_nonce(), 0);
    assert_eq!(
        state.zone().last_batch().withdrawal_queue_hash(),
        B256::ZERO
    );
    assert_eq!(state.zone().last_batch().withdrawal_batch_index(), 0);
    assert_eq!(
        state.zone().batch_start().first_zone_parent_hash(),
        B256::ZERO
    );
    assert_eq!(
        state.zone().batch_start().first_processed_deposit(),
        crate::model::state::ZoneProcessedDepositCursor::ZERO
    );
    assert_eq!(state.zone().batch_start().first_withdrawal_index(), 0);
    let token = state.token(initial).unwrap();
    assert_eq!(token.accounting(), TokenAccounting::ZERO);
    assert_eq!(token.phase(), TokenPhase::ZoneEnabled);
    let [enabled] = expected.zone_deposit_prefix().token_enables() else {
        panic!("creation must expect exactly one Zone token enable")
    };
    assert_eq!(enabled.token(), initial);
    assert_eq!(enabled.name(), "INIT Token");
    assert_eq!(enabled.symbol(), "INIT");
    assert_eq!(enabled.currency(), "USD");
}

#[test]
fn creation_is_atomic_and_checks_configured_identity_and_initial_token() {
    let initial = token(0x12);
    let base = ModelState::awaiting_creation(identity(initial));

    let wrong_identity = PortalIdentity::new(portal(), ZONE_ID + 1, initial);
    let input = ImportedTempoBlockInput::new(
        0,
        alloy_primitives::U256::ZERO,
        vec![ImportedTempoOperation::Create(PortalCreationInput::new(
            wrong_identity,
            enable(initial, "INIT"),
        ))],
    );
    let before = base.clone();
    assert_eq!(
        ModelTransition::new(&base)
            .apply_imported_tempo_block(&input)
            .err(),
        Some(ModelError::PortalIdentityMismatch {
            expected: identity(initial),
            actual: wrong_identity,
        })
    );
    assert_eq!(base, before);

    let bad_config = PortalIdentity::new(Address::repeat_byte(0x99), ZONE_ID, initial);
    let bad_state = ModelState::awaiting_creation(bad_config);
    let input = ImportedTempoBlockInput::new(
        0,
        alloy_primitives::U256::ZERO,
        vec![ImportedTempoOperation::Create(PortalCreationInput::new(
            bad_config,
            enable(initial, "INIT"),
        ))],
    );
    assert_eq!(
        ModelTransition::new(&bad_state)
            .apply_imported_tempo_block(&input)
            .err(),
        Some(ModelError::PortalAddressMismatch {
            expected: portal(),
            actual: Address::repeat_byte(0x99),
        })
    );

    let input = ImportedTempoBlockInput::new(
        0,
        alloy_primitives::U256::ZERO,
        vec![ImportedTempoOperation::Create(PortalCreationInput::new(
            identity(initial),
            enable(token(0x13), "WRONG"),
        ))],
    );
    assert_eq!(
        ModelTransition::new(&base)
            .apply_imported_tempo_block(&input)
            .err(),
        Some(ModelError::InitialTokenMismatch {
            expected: initial,
            actual: token(0x13),
        })
    );
}

#[test]
fn creation_cannot_repeat_and_all_pre_creation_operations_fail_on_lifecycle_first() {
    let initial = token(0x14);
    let state = created_state(initial);
    let duplicate = ImportedTempoBlockInput::new(
        0,
        alloy_primitives::U256::ZERO,
        vec![creation_operation(initial)],
    );
    assert_eq!(
        ModelTransition::new(&state)
            .apply_imported_tempo_block(&duplicate)
            .err(),
        Some(ModelError::PortalAlreadyCreated)
    );

    let premature_cases = [
        ImportedTempoOperation::TokenEnabled(enable(token(0x15), "B")),
        ImportedTempoOperation::BouncebackGasUpdated(7),
        ImportedTempoOperation::OrdinaryDepositAppended(ordinary(initial, 0xff, 1)),
    ];
    for operation in premature_cases {
        let awaiting = ModelState::awaiting_creation(identity(initial));
        let before = awaiting.clone();
        let input = ImportedTempoBlockInput::new(0, alloy_primitives::U256::ZERO, vec![operation]);
        assert_eq!(
            ModelTransition::new(&awaiting)
                .apply_imported_tempo_block(&input)
                .err(),
            Some(ModelError::PortalNotCreated)
        );
        assert_eq!(awaiting, before);
    }
}

#[test]
fn token_enablement_is_exact_block_ordered_and_preserves_same_block_liability() {
    let initial = token(0x21);
    let second = token(0x22);
    let third = token(0x23);
    let mut state = created_state(initial);
    let deposit = ordinary(second, 0x31, 700);
    let mut operations = vec![
        ImportedTempoOperation::TokenEnabled(enable(second, "B")),
        ImportedTempoOperation::TokenEnabled(enable(third, "C")),
    ];
    operations.extend(ordinary_append_operations(std::slice::from_ref(&deposit)));
    let imported = ImportedTempoBlockInput::new(0, alloy_primitives::U256::ZERO, operations);
    let reversed = ZoneDepositPrefixInput::new(
        vec![enable(third, "C"), enable(second, "B")],
        vec![],
        vec![],
    );
    assert_eq!(
        ModelTransition::new(&state)
            .apply_imported_tempo_block(&imported)
            .unwrap()
            .apply_zone_block(&advance_only_block(&reversed))
            .err(),
        Some(ModelError::ZoneTokenEnableMismatch {
            index: 0,
            expected: Box::new(enable(second, "B")),
            actual: Box::new(enable(third, "C")),
        })
    );

    let zone = ZoneDepositPrefixInput::new(
        vec![enable(second, "B"), enable(third, "C")],
        vec![],
        vec![],
    );
    let expected = commit(&mut state, &imported, &zone).unwrap();

    let second_state = state.token(second).unwrap();
    assert_eq!(second_state.phase(), TokenPhase::ZoneEnabled);
    assert_eq!(second_state.accounting().supply, U256::ZERO);
    assert_eq!(second_state.accounting().deposit_liability, U256::from(700));
    assert_eq!(state.token(third).unwrap().phase(), TokenPhase::ZoneEnabled);
    let enabled = expected.zone_deposit_prefix().token_enables();
    assert_eq!(enabled.len(), 2);
    assert_eq!(
        enabled
            .iter()
            .map(|enable| (enable.token(), enable.symbol()))
            .collect::<Vec<_>>(),
        vec![(second, "B"), (third, "C")]
    );

    let duplicate = ImportedTempoBlockInput::new(
        0,
        alloy_primitives::U256::ZERO,
        vec![ImportedTempoOperation::TokenEnabled(enable(second, "B"))],
    );
    assert_eq!(
        ModelTransition::new(&state)
            .apply_imported_tempo_block(&duplicate)
            .err(),
        Some(ModelError::TokenAlreadyEnabled { token: second })
    );
}

#[test]
fn collateral_and_supply_token_views_merge_parent_replacements_and_new_tokens_in_order() {
    let low = token(0x10);
    let middle = token(0x20);
    let high = token(0x30);
    let mut state = created_state(low);
    commit(
        &mut state,
        &ImportedTempoBlockInput::new(
            0,
            alloy_primitives::U256::ZERO,
            vec![ImportedTempoOperation::TokenEnabled(enable(high, "HIGH"))],
        ),
        &ZoneDepositPrefixInput::new(vec![enable(high, "HIGH")], vec![], vec![]),
    )
    .unwrap();

    let deposit = ordinary(low, 0x41, 77);
    let mut operations = ordinary_append_operations(std::slice::from_ref(&deposit));
    operations.push(ImportedTempoOperation::TokenEnabled(enable(middle, "MID")));
    let imported = ImportedTempoBlockInput::new(0, alloy_primitives::U256::ZERO, operations);
    let post_l1 = ModelTransition::new(&state)
        .apply_imported_tempo_block(&imported)
        .unwrap();

    assert!(post_l1.created_portal().is_some());
    assert_eq!(
        post_l1
            .tokens()
            .map(|(address, state)| {
                (address, state.phase(), state.accounting().deposit_liability)
            })
            .collect::<Vec<_>>(),
        vec![
            (low, TokenPhase::ZoneEnabled, U256::from(77)),
            (middle, TokenPhase::PendingZoneEnable, U256::ZERO),
            (high, TokenPhase::ZoneEnabled, U256::ZERO),
        ]
    );
    assert_eq!(
        post_l1.token(middle).unwrap().phase(),
        TokenPhase::PendingZoneEnable
    );

    let completed = post_l1
        .apply_zone_block(&advance_only_block(&ZoneDepositPrefixInput::new(
            vec![enable(middle, "MID")],
            vec![],
            vec![],
        )))
        .unwrap();
    assert_eq!(
        completed
            .tokens()
            .map(|(address, state)| (address, state.phase()))
            .collect::<Vec<_>>(),
        vec![
            (low, TokenPhase::ZoneEnabled),
            (middle, TokenPhase::ZoneEnabled),
            (high, TokenPhase::ZoneEnabled),
        ]
    );
    assert_eq!(
        completed.token(low).unwrap().accounting().deposit_liability,
        U256::from(77)
    );
}

#[test]
fn zone_enablement_must_equal_enables_from_this_imported_block() {
    let initial = token(0x23);
    let second = token(0x24);
    let state = created_state(initial);
    let imported = ImportedTempoBlockInput::new(
        0,
        alloy_primitives::U256::ZERO,
        vec![ImportedTempoOperation::TokenEnabled(enable(second, "B"))],
    );

    let transition = ModelTransition::new(&state)
        .apply_imported_tempo_block(&imported)
        .unwrap();
    assert_eq!(
        transition
            .apply_zone_block(&advance_only_block(&ZoneDepositPrefixInput::default()))
            .err(),
        Some(ModelError::ZoneTokenEnableCountMismatch {
            expected: 1,
            actual: 0,
        })
    );

    let wrong = ZoneDepositPrefixInput::new(vec![enable(second, "WRONG")], Vec::new(), Vec::new());
    assert_eq!(
        ModelTransition::new(&state)
            .apply_imported_tempo_block(&imported)
            .unwrap()
            .apply_zone_block(&advance_only_block(&wrong))
            .err(),
        Some(ModelError::ZoneTokenEnableMismatch {
            index: 0,
            expected: Box::new(enable(second, "B")),
            actual: Box::new(enable(second, "WRONG")),
        })
    );
}

#[test]
fn same_block_config_updates_retain_event_order() {
    let initial = token(0x25);
    let base = created_state(initial);
    let apply = |updates: Vec<u64>| {
        let input = ImportedTempoBlockInput::new(
            0,
            alloy_primitives::U256::ZERO,
            updates
                .into_iter()
                .map(ImportedTempoOperation::BouncebackGasUpdated)
                .collect(),
        );
        let mut state = base.clone();
        commit(&mut state, &input, &empty_zone()).unwrap();
        state.portal().created().unwrap().config().bounceback_gas()
    };
    assert_eq!(apply(vec![11, 22, 33]), 33);
    assert_eq!(apply(vec![33, 22, 11]), 11);
}

#[test]
fn portal_append_rejects_invalid_value_and_cursor_overflow_without_mutating_parent() {
    let initial = token(0x26);
    let state = created_state(initial);
    let before = state.clone();

    let zero_refund = ordinary(initial, 0xff, 1);
    let input = ImportedTempoBlockInput::new(
        0,
        alloy_primitives::U256::ZERO,
        ordinary_append_operations(std::slice::from_ref(&zero_refund)),
    );
    assert_eq!(
        ModelTransition::new(&state)
            .apply_imported_tempo_block(&input)
            .err(),
        Some(ModelError::ZeroTempoRefundRecipient)
    );
    assert_eq!(state, before);

    let mut liability_overflow = state.clone();
    liability_overflow.set_token_accounting_for_test(
        initial,
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::MAX,
            withdrawal_liability: U256::ZERO,
        },
    );
    let deposit = ordinary(initial, 0x43, 1);
    let input = ImportedTempoBlockInput::new(
        0,
        alloy_primitives::U256::ZERO,
        ordinary_append_operations(std::slice::from_ref(&deposit)),
    );
    assert_eq!(
        ModelTransition::new(&liability_overflow)
            .apply_imported_tempo_block(&input)
            .err(),
        Some(ModelError::Accounting(AccountingError::Overflow(
            Component::DepositLiability
        )))
    );

    let mut overflow = state;
    overflow.set_portal_deposit_cursor_for_test(PortalDepositCursor::new(B256::ZERO, u64::MAX));
    let input = ImportedTempoBlockInput::new(
        0,
        alloy_primitives::U256::ZERO,
        vec![ImportedTempoOperation::OrdinaryDepositAppended(ordinary(
            initial, 0x42, 1,
        ))],
    );
    assert_eq!(
        ModelTransition::new(&overflow)
            .apply_imported_tempo_block(&input)
            .err(),
        Some(ModelError::PortalDepositNumberOverflow)
    );
}
