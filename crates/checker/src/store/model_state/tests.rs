use std::num::NonZeroU64;

use alloy_primitives::{Address, B256, U256};

use super::*;
use crate::{
    model::{
        accounting::TokenAccounting,
        state::{ModelState, PortalDepositCursor, PortalIdentity, ZoneProcessedDepositCursor},
        transition::{
            ModelTransition,
            test_inputs::{ImportedTempoOperation, imported_block},
        },
        validation::{AuthoritativeStateError, OwnerKind},
    },
    store::{schema::ModelKey, value::ModelValue},
};

fn identity() -> PortalIdentity {
    PortalIdentity::new(Address::repeat_byte(0x11), 7, Address::repeat_byte(0x22))
}

#[test]
fn awaiting_creation_round_trip_is_exact() {
    let state = ModelState::awaiting_creation(identity());
    let rows = flatten_model(&state).unwrap();
    assert_eq!(assemble_model(identity(), rows).unwrap(), state);
    validate_round_trip(&state).unwrap();
}

#[test]
fn created_token_round_trip_is_byte_exact() {
    let state = ModelState::created_with_zone_token_for_test(
        identity(),
        TokenAccounting {
            supply: U256::from(1),
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::ZERO,
        },
    );
    let rows = flatten_model(&state).unwrap();
    let expected_bytes = model_bytes(&rows);
    let decoded = assemble_model(identity(), rows).unwrap();
    let decoded_rows = flatten_model(&decoded).unwrap();

    assert_eq!(decoded, state);
    assert_eq!(model_bytes(&decoded_rows), expected_bytes);
}

#[test]
fn key_value_family_mismatch_is_rejected() {
    let mut rows = flatten_model(&ModelState::awaiting_creation(identity())).unwrap();
    rows.insert(
        ModelKey::ZoneNextWithdrawalIndex,
        ModelValue::ZoneLastFallbackNonce(1),
    );

    assert!(matches!(
        assemble_model(identity(), rows),
        Err(ModelPersistenceError::KeyValueMismatch { .. })
    ));
}

#[test]
fn partial_created_portal_is_rejected() {
    let mut rows = flatten_model(&ModelState::awaiting_creation(identity())).unwrap();
    rows.insert(
        ModelKey::PortalConfig,
        ModelValue::PortalConfig { bounceback_gas: 1 },
    );

    assert!(matches!(
        assemble_model(identity(), rows),
        Err(ModelPersistenceError::Partial("created Portal state"))
    ));
}

#[test]
fn portal_refund_totals_are_derived_from_credit_rows() {
    let mut state = ModelState::created_with_zone_token_for_test(
        identity(),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(9),
            withdrawal_liability: U256::ZERO,
        },
    );
    let cursor_hash = B256::repeat_byte(0x32);
    state.set_portal_deposit_cursor_for_test(PortalDepositCursor::new(cursor_hash, 1));
    state.set_zone_processed_deposit_cursor_for_test(ZoneProcessedDepositCursor::new(
        cursor_hash,
        1,
    ));
    state.seed_portal_refund_for_test(
        crate::model::ownership::PortalRefundId {
            token: identity().initial_token(),
            recipient: Address::repeat_byte(0x44),
            failed_deposit: crate::model::ownership::DepositId {
                portal: identity().portal(),
                deposit_number: NonZeroU64::new(1).unwrap(),
            },
        },
        crate::model::ownership::PortalRefundOwner::Pending { amount: 9 },
    );

    let rows = flatten_model(&state).unwrap();
    assert!(rows.contains_key(&ModelKey::PortalRefundCredit {
        token: identity().initial_token(),
        recipient: Address::repeat_byte(0x44),
        origin: 1,
    }));
    assert_eq!(assemble_model(identity(), rows).unwrap(), state);
}

#[test]
fn pre_creation_open_record_is_rejected_in_both_directions() {
    let mut state = ModelState::awaiting_creation(identity());
    state.seed_token_for_test(
        Address::repeat_byte(0x55),
        crate::model::state::TokenState::for_test(
            crate::model::state::TokenPhase::PendingZoneEnable,
            TokenAccounting::ZERO,
        ),
    );
    assert!(flatten_model(&state).is_err());

    let mut rows = flatten_model(&ModelState::awaiting_creation(identity())).unwrap();
    rows.insert(
        ModelKey::Token(Address::repeat_byte(0x55)),
        ModelValue::Token(crate::store::value::TokenValue {
            phase: crate::store::value::StoredTokenPhase::PendingZoneEnable,
            supply: U256::ZERO,
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::ZERO,
        }),
    );
    assert!(assemble_model(identity(), rows).is_err());
}

#[test]
fn encoded_rows_are_ordered_by_physical_key() {
    let state = ModelState::created_with_zone_token_for_test(identity(), TokenAccounting::ZERO);
    let rows = flatten_model(&state).unwrap();
    let bytes = model_bytes(&rows);

    assert!(bytes.windows(2).all(|pair| pair[0].0 < pair[1].0));
    assert!(
        bytes
            .iter()
            .all(|(key, value)| !key.is_empty() && !value.is_empty())
    );
}

#[test]
fn imported_bootstrap_update_is_portal_only_and_byte_exact() {
    let parent = ModelState::created_with_zone_token_for_test(identity(), TokenAccounting::ZERO);
    let input = imported_block(
        1,
        U256::ZERO,
        vec![ImportedTempoOperation::BouncebackGasUpdated(77)],
    );
    let update = ModelTransition::new(&parent)
        .apply_imported_tempo_block(&input)
        .unwrap()
        .into_bootstrap_state_update();
    let changes = update::lower_imported_update(identity(), &update).unwrap();
    assert!(changes.keys().all(|key| {
        matches!(
            key,
            ModelKey::PortalConfig | ModelKey::PortalDepositCursor | ModelKey::PortalSettlement
        )
    }));

    let mut persisted = flatten_model(&parent).unwrap();
    for (key, value) in changes {
        match value {
            Some(value) => {
                persisted.insert(key, value);
            }
            None => {
                persisted.remove(&key);
            }
        }
    }
    let mut child = parent;
    update.apply_to_current_parent(&mut child);

    assert_eq!(
        model_bytes(&persisted),
        model_bytes(&flatten_model(&child).unwrap())
    );
}

#[test]
fn finalized_user_shape_is_revalidated_during_assembly() {
    let state = ModelState::created_with_zone_token_for_test(identity(), TokenAccounting::ZERO);
    let mut rows = flatten_model(&state).unwrap();
    rows.insert(
        ModelKey::Withdrawal(0),
        ModelValue::Withdrawal(crate::store::value::WithdrawalValue::FinalizedUser {
            identity: crate::store::value::UserWithdrawalIdentityValue {
                sender: Address::repeat_byte(0x66),
                transaction_hash: B256::repeat_byte(0x77),
                fallback_nonce: 1,
            },
            request: crate::store::value::UserWithdrawalRequestValue {
                token: Address::repeat_byte(0x22),
                recipient: Address::repeat_byte(0x88),
                amount: 0,
                memo: B256::ZERO,
                gas_limit: 0,
                callback_data: Vec::new(),
            },
            encrypted_sender: Vec::new(),
        }),
    );

    assert!(assemble_model(identity(), rows).is_err());
}

#[test]
fn finalized_failed_deposit_must_belong_to_the_checker_portal() {
    let state = ModelState::created_with_zone_token_for_test(identity(), TokenAccounting::ZERO);
    let mut rows = flatten_model(&state).unwrap();
    let wrong_portal = Address::repeat_byte(0xff);
    rows.insert(
        ModelKey::Withdrawal(0),
        ModelValue::Withdrawal(
            crate::store::value::WithdrawalValue::FinalizedFailedDeposit {
                deposit_portal: wrong_portal,
                deposit_number: 1,
                token: Address::repeat_byte(0x22),
                recipient: Address::repeat_byte(0x88),
                amount: 1,
            },
        ),
    );

    assert!(matches!(
        assemble_model(identity(), rows),
        Err(ModelPersistenceError::AddressIdentityMismatch {
            kind: "finalized failed deposit",
            expected,
            actual,
        }) if expected == identity().portal() && actual == wrong_portal
    ));
}

#[test]
fn assembly_rejects_an_open_owner_for_a_missing_token() {
    let missing_token = Address::repeat_byte(0x99);
    let mut state = ModelState::created_with_zone_token_for_test(identity(), TokenAccounting::ZERO);
    let cursor_hash = B256::repeat_byte(0x98);
    state.set_portal_deposit_cursor_for_test(PortalDepositCursor::new(cursor_hash, 1));
    state.set_zone_processed_deposit_cursor_for_test(ZoneProcessedDepositCursor::new(
        cursor_hash,
        1,
    ));
    let mut rows = flatten_model(&state).unwrap();
    rows.insert(
        ModelKey::PortalRefundCredit {
            token: missing_token,
            recipient: Address::repeat_byte(0x97),
            origin: 1,
        },
        ModelValue::PortalRefundCredit(1),
    );

    assert!(matches!(
        assemble_model(identity(), rows),
        Err(ModelPersistenceError::Authoritative(
            AuthoritativeStateError::MissingOwnerToken {
                owner: OwnerKind::PortalRefund,
                token,
            }
        )) if token == missing_token
    ));
}
