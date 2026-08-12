use alloy_primitives::{Address, B256, Bytes, U256, address, b256, fixed_bytes};

use std::{collections::BTreeMap, num::NonZeroU64};

use crate::kernel::{
    BatchId, BatchSubmission, Deposit, DepositId, DepositOutcome, Effect, Finalization,
    ImportedFacts, ImportedOperation, OrdinaryDeposit, PortalIdentity, PortalState, RefundClaim,
    State, StateKey, StateValue, TokenEnable, TokenPhase, TransitionError, UserWithdrawal,
    WithdrawalId, WithdrawalOutcome, WithdrawalProcessing, ZoneFacts, ZoneOperation,
    apply_genesis_handoff, apply_imported, apply_zone,
    derivation::{WITHDRAWAL_TERMINATOR, ordinary_deposit_hash, portal_address},
    facts::DepositPayload,
    invariants::{InvariantCode, validate},
    state::{
        BatchState, FallbackId, InboxRefundId, Overlay, PortalRefundId, TokenState, WithdrawalOwner,
    },
};

const ZONE_ID: u32 = 7;

fn identity() -> PortalIdentity {
    PortalIdentity {
        zone_id: ZONE_ID,
        portal: portal_address(ZONE_ID),
        initial_token: address!("1111111111111111111111111111111111111111"),
    }
}

fn enable(token: Address) -> TokenEnable {
    TokenEnable {
        token,
        name: "Token".into(),
        symbol: "TOK".into(),
        currency: "USD".into(),
    }
}

fn deposit() -> OrdinaryDeposit {
    OrdinaryDeposit {
        token: identity().initial_token,
        sender: address!("2222222222222222222222222222222222222222"),
        amount: 1_000,
        tempo_refund_recipient: address!("3333333333333333333333333333333333333333"),
        key_index: U256::from(7),
        encrypted: DepositPayload {
            ephemeral_pubkey_x: b256!(
                "4444444444444444444444444444444444444444444444444444444444444444"
            ),
            ephemeral_pubkey_y_parity: 2,
            ciphertext: fixed_bytes!(
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f"
            ),
            nonce: fixed_bytes!("555555555555555555555555"),
            tag: fixed_bytes!("66666666666666666666666666666666"),
        },
    }
}

fn created_with_deposit() -> (State, OrdinaryDeposit) {
    let parent = State::awaiting(identity());
    let deposit = deposit();
    let imported = apply_imported(
        &parent,
        &ImportedFacts {
            operations: vec![
                ImportedOperation::Create {
                    identity: identity(),
                    initial_token: enable(identity().initial_token),
                },
                ImportedOperation::AppendDeposit(deposit.clone()),
            ],
            ..ImportedFacts::default()
        },
    )
    .unwrap();
    let mut state = imported.into_state();
    state
        .apply(&apply_genesis_handoff(&state).unwrap())
        .unwrap();
    (state, deposit)
}

fn commit(state: &mut State, imported: ImportedFacts, zone: ZoneFacts) -> Vec<Effect> {
    let candidate = apply_zone(apply_imported(state, &imported).unwrap(), &zone).unwrap();
    state.apply(&candidate.delta).unwrap();
    candidate.expected_effects
}

fn funded_state() -> State {
    let mut state = State::awaiting(identity());
    commit(
        &mut state,
        ImportedFacts {
            operations: vec![ImportedOperation::Create {
                identity: identity(),
                initial_token: enable(identity().initial_token),
            }],
            ..ImportedFacts::default()
        },
        ZoneFacts {
            enabled_tokens: vec![enable(identity().initial_token)],
            ..ZoneFacts::default()
        },
    );
    let mut rows = state.rows().clone();
    let StateValue::Token(mut token) = rows[&StateKey::Token(identity().initial_token)].clone()
    else {
        unreachable!()
    };
    token.accounting.supply = U256::from(100);
    rows.insert(
        StateKey::Token(identity().initial_token),
        StateValue::Token(token),
    );
    State::from_rows(rows).unwrap()
}

fn user_withdrawal(seed: u8, amount: u128) -> UserWithdrawal {
    UserWithdrawal {
        sender: Address::repeat_byte(seed),
        transaction_hash: B256::repeat_byte(seed.wrapping_add(1)),
        token: identity().initial_token,
        to: Address::repeat_byte(seed.wrapping_add(2)),
        amount,
        memo: B256::repeat_byte(seed.wrapping_add(3)),
        gas_limit: 0,
        callback_data: Default::default(),
        reveal_to: Default::default(),
    }
}

fn finalized_user_state() -> (State, crate::kernel::Withdrawal) {
    let mut state = funded_state();
    commit(
        &mut state,
        ImportedFacts::default(),
        ZoneFacts {
            block_hash: B256::repeat_byte(1),
            block_number: 1,
            operations: vec![ZoneOperation::AcceptWithdrawal(user_withdrawal(0x40, 40))],
            ..ZoneFacts::default()
        },
    );
    commit(
        &mut state,
        ImportedFacts {
            block_number: 2,
            ..ImportedFacts::default()
        },
        ZoneFacts {
            block_hash: B256::repeat_byte(2),
            block_number: 2,
            finalization: Some(Finalization {
                block_number: 2,
                declared_count: 1,
                encrypted_senders: vec![Default::default()],
            }),
            ..ZoneFacts::default()
        },
    );
    let id = WithdrawalId {
        zone_id: ZONE_ID,
        index: 0,
    };
    let StateValue::Withdrawal(WithdrawalOwner::Finalized { data, .. }) =
        state.rows()[&StateKey::Withdrawal(id)].clone()
    else {
        unreachable!()
    };
    (state, data)
}

fn submit_first_batch(state: &mut State) {
    submit_batch(state, NonZeroU64::MIN);
}

fn submit_batch(state: &mut State, index: NonZeroU64) {
    let id = BatchId {
        zone_id: ZONE_ID,
        index,
    };
    let StateValue::Batch(BatchState::Finalized {
        boundary,
        queue_hash,
        ..
    }) = state.rows()[&StateKey::Batch(id)].clone()
    else {
        unreachable!()
    };
    commit(
        state,
        ImportedFacts {
            operations: vec![ImportedOperation::SubmitBatch(BatchSubmission {
                tempo_block: boundary.tempo_block,
                previous_block: boundary.first_parent,
                next_block: boundary.final_block,
                previous_deposit: boundary.first_deposit,
                next_deposit: boundary.final_deposit,
                withdrawal_queue_hash: queue_hash,
                next_zone_height: U256::from(boundary.zone_height),
            })],
            ..ImportedFacts::default()
        },
        ZoneFacts::default(),
    );
}

mod derivation;
mod invariants;
mod state;
mod tempo;
mod zone;
