//! Milestone-2 parity fixtures. The two sides receive separately constructed
//! values so a shared transition or expected-value constructor cannot make the
//! comparison pass vacuously.

use alloy_primitives::{Address, B256, U256};
use zone_checker_kernel as compact;

use super::support::*;
use crate::model::{
    accounting::TokenAccounting,
    encoding::{DepositQueueMember, OrdinaryDeposit},
    input::{AuthenticatedDepositOutcome, ImportedTempoBlockInput, ZoneDepositPrefixInput},
    output::{ExpectedDepositOutcome, ExpectedImportedTempoOperation},
    state::{ModelState, TokenPhase},
};

fn compact_identity(token: Address) -> compact::PortalIdentity {
    compact::PortalIdentity {
        portal: portal(),
        zone_id: ZONE_ID,
        initial_token: token,
    }
}

fn compact_enable(token: Address, label: &str) -> compact::TokenEnable {
    compact::TokenEnable {
        token,
        name: format!("{label} Token"),
        symbol: label.into(),
        currency: "USD".into(),
    }
}

fn compact_deposit(value: &OrdinaryDeposit) -> compact::OrdinaryDeposit {
    compact::OrdinaryDeposit {
        token: value.token(),
        sender: value.sender(),
        amount: value.amount(),
        tempo_refund_recipient: value.tempo_refund_recipient(),
        key_index: value.key_index(),
        encrypted: compact::DepositPayload {
            ephemeral_pubkey_x: value.encrypted().ephemeral_pubkey_x(),
            ephemeral_pubkey_y_parity: value.encrypted().ephemeral_pubkey_y_parity(),
            ciphertext: value.encrypted().ciphertext(),
            nonce: value.encrypted().nonce(),
            tag: value.encrypted().tag(),
        },
    }
}

fn compact_token(state: &compact::State, token: Address) -> compact::TokenState {
    match state.rows().get(&compact::StateKey::Token(token)).unwrap() {
        compact::StateValue::Token(value) => *value,
        _ => unreachable!(),
    }
}

fn assert_snapshot_eq(old: &ModelState, new: &compact::State, token: Address) {
    let old_portal = old.portal().created().unwrap();
    let new_portal = match new.rows().get(&compact::StateKey::Portal).unwrap() {
        compact::StateValue::Portal(compact::PortalState::Created {
            identity, deposit, ..
        }) => (*identity, *deposit),
        _ => panic!("compact Portal was not created"),
    };
    assert_eq!(old_portal.identity().portal(), new_portal.0.portal);
    assert_eq!(old_portal.identity().zone_id(), new_portal.0.zone_id);
    assert_eq!(old_portal.deposit_cursor().hash(), new_portal.1.hash);
    assert_eq!(old_portal.deposit_cursor().number(), new_portal.1.number);

    let old_token = old.token(token).unwrap();
    let new_token = compact_token(new, token);
    assert_eq!(
        old_token.phase() == TokenPhase::ZoneEnabled,
        new_token.phase == compact::TokenPhase::ZoneEnabled
    );
    assert_eq!(old_token.accounting().supply, new_token.accounting.supply);
    assert_eq!(
        old_token.accounting().deposit_liability,
        new_token.accounting.deposits
    );
    assert_eq!(
        old_token.accounting().withdrawal_liability,
        new_token.accounting.withdrawals
    );

    assert_eq!(
        old.zone().processed_deposit_cursor().hash(),
        match new.rows().get(&compact::StateKey::Zone).unwrap() {
            compact::StateValue::Zone(zone) => zone.processed_deposit.hash,
            _ => unreachable!(),
        }
    );
    assert_eq!(
        old.pending_deposits().len(),
        new.rows()
            .keys()
            .filter(|key| matches!(key, compact::StateKey::Deposit(_)))
            .count()
    );
    assert_eq!(
        old.withdrawals().len(),
        new.rows()
            .keys()
            .filter(|key| matches!(key, compact::StateKey::Withdrawal(_)))
            .count()
    );
}

#[test]
fn creation_append_and_deposit_prefix_match_legacy_semantics() {
    let token = token(0x31);
    let deposits = [ordinary(token, 0x40, 10), ordinary(token, 0x41, 20)];

    let mut old = ModelState::awaiting_creation(identity(token));
    let old_creation = ImportedTempoBlockInput::new(0, U256::ZERO, vec![creation_operation(token)]);
    let old_initial_zone = ZoneDepositPrefixInput::new(vec![enable(token, "INIT")], vec![], vec![]);
    commit(&mut old, &old_creation, &old_initial_zone).unwrap();

    let mut new = compact::State::awaiting(compact_identity(token));
    let candidate = compact::apply_zone(
        compact::apply_imported(
            &new,
            &compact::ImportedFacts {
                operations: vec![compact::ImportedOperation::Create {
                    identity: compact_identity(token),
                    initial_token: compact_enable(token, "INIT"),
                }],
            },
        )
        .unwrap(),
        &compact::ZoneFacts {
            enabled_tokens: vec![compact_enable(token, "INIT")],
            ..compact::ZoneFacts::default()
        },
    )
    .unwrap();
    new.apply(&candidate.delta).unwrap();
    assert_snapshot_eq(&old, &new, token);

    let old_imported =
        ImportedTempoBlockInput::new(1, U256::ZERO, ordinary_append_operations(&deposits));
    let old_expected = commit(&mut old, &old_imported, &empty_zone()).unwrap();
    let compact_deposits = deposits.iter().map(compact_deposit).collect::<Vec<_>>();
    let candidate = compact::apply_zone(
        compact::apply_imported(
            &new,
            &compact::ImportedFacts {
                operations: compact_deposits
                    .iter()
                    .cloned()
                    .map(compact::ImportedOperation::AppendDeposit)
                    .collect(),
            },
        )
        .unwrap(),
        &compact::ZoneFacts::default(),
    )
    .unwrap();
    let old_appends = old_expected.imported_tempo_block().operations();
    let new_appends = &candidate.expected_effects;
    assert_eq!(old_appends.len(), new_appends.len());
    for (old, new) in old_appends.iter().zip(new_appends) {
        let ExpectedImportedTempoOperation::DepositAppended(old) = old else {
            panic!()
        };
        let compact::ExpectedEffect::DepositAppended { id, queue_hash } = new else {
            panic!()
        };
        assert_eq!(old.id().portal, id.portal);
        assert_eq!(old.id().deposit_number, id.number);
        assert_eq!(old.queue_hash(), *queue_hash);
    }
    new.apply(&candidate.delta).unwrap();
    assert_snapshot_eq(&old, &new, token);
    assert_eq!(
        old.token(token).unwrap().accounting(),
        TokenAccounting {
            supply: U256::ZERO,
            deposit_liability: U256::from(30),
            withdrawal_liability: U256::ZERO,
        }
    );

    let members = deposits
        .iter()
        .cloned()
        .map(DepositQueueMember::Ordinary)
        .collect();
    let old_zone = ZoneDepositPrefixInput::new(
        vec![],
        members,
        vec![
            AuthenticatedDepositOutcome::OrdinaryMinted {
                recipient: Address::repeat_byte(0x50),
                memo: B256::ZERO,
            },
            AuthenticatedDepositOutcome::OrdinaryFailed,
        ],
    );
    let old_expected = commit(&mut old, &empty_import(), &old_zone).unwrap();
    let candidate = compact::apply_zone(
        compact::apply_imported(&new, &compact::ImportedFacts::default()).unwrap(),
        &compact::ZoneFacts {
            deposits: compact_deposits,
            outcomes: vec![
                compact::DepositOutcome::Minted,
                compact::DepositOutcome::Failed,
            ],
            ..compact::ZoneFacts::default()
        },
    )
    .unwrap();
    assert!(matches!(
        old_expected.zone_deposit_prefix().deposit_outcomes(),
        [
            ExpectedDepositOutcome::OrdinaryMinted(_),
            ExpectedDepositOutcome::OrdinaryFailed(_)
        ]
    ));
    assert!(matches!(
        candidate.expected_effects.as_slice(),
        [
            compact::ExpectedEffect::DepositProcessed { .. },
            compact::ExpectedEffect::DepositFailed { .. }
        ]
    ));
    assert_eq!(
        old_expected.zone_deposit_prefix().processed_cursor().hash(),
        candidate.expected_state.processed_deposit_hash
    );
    new.apply(&candidate.delta).unwrap();
    assert_snapshot_eq(&old, &new, token);
}
