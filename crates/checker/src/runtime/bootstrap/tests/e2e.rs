use alloy_eips::BlockNumHash;
use alloy_primitives::{B256, U256};
use tempfile::TempDir;

use self::support::*;
use super::super::{create_fresh, open_existing, resume_l1_replay};
use crate::{
    model::state::TokenPhase,
    runtime::{
        L1Client, RuntimeError, bootstrap::error::BootstrapError,
        exex::promote_zone_replay_if_ready, state::ReadyToAcknowledge,
    },
    store::{
        db::{CheckerStore, StoreSnapshot},
        model_state::model_bytes,
        value::BootstrapState,
    },
};

mod equivalence;
mod genesis_identity;
mod genesis_supply;
mod support;

#[tokio::test]
async fn first_lazy_l1_use_validates_the_stored_chain_id() {
    let matching = RpcScript::new();
    matching.push_chain_id();
    let mut client = L1Client::with_provider_for_chain(matching.provider(), L1_CHAIN_ID);
    client.provider().await.unwrap();
    client.provider().await.unwrap();
    matching.assert_consumed();

    let mismatched = RpcScript::new();
    mismatched.push_chain_id_value(L1_CHAIN_ID + 1);
    mismatched.push_chain_id();
    let mut client = L1Client::with_provider_for_chain(mismatched.provider(), L1_CHAIN_ID);
    assert!(matches!(
        client.provider().await,
        Err(RuntimeError::Check(
            crate::check::finding::CheckError::Acquisition(
                crate::observe::AcquisitionError::Inconsistent {
                    kind: crate::observe::AcquisitionSource::L1Rpc,
                    ..
                }
            )
        ))
    ));
    client.provider().await.unwrap();
    mismatched.assert_consumed();
}

#[tokio::test]
async fn fresh_bootstrap_replays_authenticated_pre_genesis_config_and_deposit_then_restarts_in_place()
 {
    let fixture = PreGenesisFixture::new();
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("archive-rebuild");
    let zone_provider = zone_provider(fixture.zone_genesis, fixture.anchor.tip());
    let script = RpcScript::new();
    script.push_full_fresh_replay(&fixture);
    let provider = script.provider();

    let mut checker = create_fresh(
        &fixture.config(None),
        fixture.zone_chain_id,
        &path,
        &zone_provider,
        &provider,
    )
    .await
    .unwrap();
    script.assert_consumed();

    assert_pre_genesis_snapshot(&checker, &fixture);
    let genesis = checker.store.load_progress().unwrap().verified_zone_tip;
    assert!(
        promote_zone_replay_if_ready(&mut checker, ReadyToAcknowledge::verified(genesis), genesis,)
            .unwrap(),
        "caught-up pre-genesis replay must hand off to live mode"
    );

    apply_first_post_genesis_deposit(&mut checker, &fixture).await;

    let expected = checker.current_snapshot_for_test();
    assert_eq!(expected.bootstrap, BootstrapState::live());
    assert_eq!(expected.verified_zone_tip.number, 1);
    let token = expected.model.token(fixture.initial_token).unwrap();
    assert_eq!(token.phase(), TokenPhase::ZoneEnabled);
    assert_eq!(token.accounting().supply, U256::from(DEPOSIT_AMOUNT));
    assert!(expected.model.pending_deposits().is_empty());
    drop(checker);

    let restart_script = RpcScript::new();
    let restarted = open_existing(
        &fixture.config(None),
        fixture.zone_chain_id,
        &path,
        fixture.identity(),
        &zone_provider,
    )
    .unwrap();
    restart_script.assert_consumed();
    let actual = restarted.current_snapshot_for_test();
    assert_eq!(
        actual, expected,
        "restart must load the durable cut directly"
    );
    assert_eq!(
        model_bytes(&actual.model_rows),
        model_bytes(&expected.model_rows)
    );
}

#[tokio::test]
async fn every_durable_l1_cursor_resumes_at_the_next_unapplied_block_and_matches_full_replay() {
    let fixture = PreGenesisFixture::new();
    let zone_provider = zone_provider(fixture.zone_genesis, fixture.anchor.tip());

    let uninterrupted_directory = TempDir::new().unwrap();
    let uninterrupted_path = uninterrupted_directory.path().join("uninterrupted");
    let uninterrupted_script = RpcScript::new();
    uninterrupted_script.push_full_fresh_replay(&fixture);
    let uninterrupted = create_fresh(
        &fixture.config(None),
        fixture.zone_chain_id,
        &uninterrupted_path,
        &zone_provider,
        &uninterrupted_script.provider(),
    )
    .await
    .unwrap();
    uninterrupted_script.assert_consumed();
    let uninterrupted_snapshot = uninterrupted.current_snapshot_for_test();

    let initialized_directory = TempDir::new().unwrap();
    let initialized_path = initialized_directory.path().join("initialized");
    persist_l1_prefix(&fixture, &initialized_path, L1ReplayCheckpoint::Initialized).await;
    let initialized_cursor =
        CheckerStore::inspect_existing_at(&initialized_path, fixture.identity()).unwrap();
    assert_eq!(
        initialized_cursor.bootstrap,
        BootstrapState::l1_replay(None)
    );
    let initialized_script = RpcScript::new();
    initialized_script.push_full_resume(&fixture);
    let mut initialized = open_existing(
        &fixture.config(None),
        fixture.zone_chain_id,
        &initialized_path,
        fixture.identity(),
        &zone_provider,
    )
    .unwrap();
    resume_l1_replay(
        &fixture.config(None),
        &zone_provider,
        &initialized_script.provider(),
        &mut initialized,
    )
    .await
    .unwrap();
    initialized_script.assert_consumed();
    assert_replay_matches(
        &initialized.current_snapshot_for_test(),
        &uninterrupted_snapshot,
    );

    let resumed_directory = TempDir::new().unwrap();
    let resumed_path = resumed_directory.path().join("resumed");
    persist_l1_prefix(&fixture, &resumed_path, L1ReplayCheckpoint::Creation).await;
    let cursor_snapshot =
        CheckerStore::inspect_existing_at(&resumed_path, fixture.identity()).unwrap();
    assert_eq!(
        cursor_snapshot.bootstrap,
        BootstrapState::l1_replay(Some(fixture.creation.tip()))
    );

    let resume_script = RpcScript::new();
    resume_script.push_resume_after_creation(&fixture);
    let mut resumed = open_existing(
        &fixture.config(None),
        fixture.zone_chain_id,
        &resumed_path,
        fixture.identity(),
        &zone_provider,
    )
    .unwrap();
    resume_l1_replay(
        &fixture.config(None),
        &zone_provider,
        &resume_script.provider(),
        &mut resumed,
    )
    .await
    .unwrap();
    resume_script.assert_consumed();
    let resumed_snapshot = resumed.current_snapshot_for_test();

    assert_replay_matches(&resumed_snapshot, &uninterrupted_snapshot);

    let completed_directory = TempDir::new().unwrap();
    let completed_path = completed_directory.path().join("completed-l1-cursor");
    persist_l1_prefix(&fixture, &completed_path, L1ReplayCheckpoint::Anchor).await;
    let completed_cursor =
        CheckerStore::inspect_existing_at(&completed_path, fixture.identity()).unwrap();
    assert_eq!(
        completed_cursor.bootstrap,
        BootstrapState::l1_replay(Some(fixture.anchor.tip()))
    );
    let completed_script = RpcScript::new();
    let mut completed = open_existing(
        &fixture.config(None),
        fixture.zone_chain_id,
        &completed_path,
        fixture.identity(),
        &zone_provider,
    )
    .unwrap();
    resume_l1_replay(
        &fixture.config(None),
        &zone_provider,
        &completed_script.provider(),
        &mut completed,
    )
    .await
    .unwrap();
    completed_script.assert_consumed();
    let completed_snapshot = completed.current_snapshot_for_test();
    assert_replay_matches(&completed_snapshot, &uninterrupted_snapshot);
}

fn assert_replay_matches(actual: &StoreSnapshot, expected: &StoreSnapshot) {
    assert_eq!(actual.verified_zone_tip, expected.verified_zone_tip);
    assert_eq!(actual.imported_tempo_tip, expected.imported_tempo_tip);
    assert_eq!(actual.bootstrap, expected.bootstrap);
    assert_eq!(
        model_bytes(&actual.model_rows),
        model_bytes(&expected.model_rows),
        "resumed and uninterrupted archive replay must persist identical authoritative bytes"
    );
}

#[tokio::test]
async fn missing_archive_history_fails_explicitly_without_creating_a_database() {
    let fixture = PreGenesisFixture::new();
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("must-stay-absent");
    let zone_provider = zone_provider(fixture.zone_genesis, fixture.anchor.tip());
    let script = RpcScript::new();
    script.push_chain_id();
    script.push_missing_block();

    assert!(matches!(
        create_fresh(
            &fixture.config(None),
            fixture.zone_chain_id,
            &path,
            &zone_provider,
            &script.provider(),
        )
        .await,
        Err(RuntimeError::Check(
            crate::check::finding::CheckError::Acquisition(
                crate::observe::AcquisitionError::Missing {
                    kind: crate::observe::AcquisitionSource::L1Block,
                    ..
                }
            )
        ))
    ));
    script.assert_consumed();
    assert!(
        !path.exists(),
        "failed acquisition must not create default rows"
    );
}

#[tokio::test]
async fn zero_genesis_checkpoint_fails_before_creating_a_database() {
    let fixture = PreGenesisFixture::new();
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("zero-checkpoint");
    let zone_provider = zone_provider(fixture.zone_genesis, BlockNumHash::new(0, B256::ZERO));
    let script = RpcScript::new();
    script.push_chain_id();

    assert!(matches!(
        create_fresh(
            &fixture.config(None),
            fixture.zone_chain_id,
            &path,
            &zone_provider,
            &script.provider(),
        )
        .await,
        Err(RuntimeError::Bootstrap(error))
            if matches!(
                error.as_ref(),
                crate::runtime::bootstrap::error::BootstrapError::UnsupportedBootstrapStyle
            )
    ));
    script.assert_consumed();
    assert!(!path.exists());
}

#[tokio::test]
async fn nonzero_genesis_protocol_progress_fails_before_creating_a_database() {
    let fixture = PreGenesisFixture::new();
    let cases = [
        (
            "processed-deposit-hash",
            B256::repeat_byte(0x11),
            0,
            B256::ZERO,
            0,
        ),
        ("processed-deposit-number", B256::ZERO, 1, B256::ZERO, 0),
        (
            "withdrawal-queue-hash",
            B256::ZERO,
            0,
            B256::repeat_byte(0x22),
            0,
        ),
        ("withdrawal-batch-index", B256::ZERO, 0, B256::ZERO, 1),
    ];

    for (
        name,
        processed_deposit_queue_hash,
        processed_deposit_number,
        withdrawal_queue_hash,
        withdrawal_batch_index,
    ) in cases
    {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join(name);
        let zone_provider = zone_provider_with_genesis_progress(
            fixture.zone_genesis,
            fixture.anchor.tip(),
            processed_deposit_queue_hash,
            processed_deposit_number,
            withdrawal_queue_hash,
            withdrawal_batch_index,
        );
        let script = RpcScript::new();
        script.push_chain_id();

        let result = create_fresh(
            &fixture.config(None),
            fixture.zone_chain_id,
            &path,
            &zone_provider,
            &script.provider(),
        )
        .await;
        let Err(error) = result else {
            panic!("{name} must reject nonzero genesis protocol progress");
        };
        assert!(matches!(
            error,
            RuntimeError::Bootstrap(error)
                if matches!(
                    error.as_ref(),
                    BootstrapError::NonzeroZoneGenesisProgress {
                        processed_deposit_queue_hash: actual_processed_deposit_queue_hash,
                        processed_deposit_number: actual_processed_deposit_number,
                        withdrawal_queue_hash: actual_withdrawal_queue_hash,
                        withdrawal_batch_index: actual_withdrawal_batch_index,
                    } if *actual_processed_deposit_queue_hash == processed_deposit_queue_hash
                        && *actual_processed_deposit_number == processed_deposit_number
                        && *actual_withdrawal_queue_hash == withdrawal_queue_hash
                        && *actual_withdrawal_batch_index == withdrawal_batch_index
                )
        ));
        script.assert_consumed();
        assert!(!path.exists(), "{name} must fail before DB creation");
    }
}
