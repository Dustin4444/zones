use std::{collections::BTreeMap, path::Path, sync::Arc};

use alloy_consensus::BlockHeader as _;
use alloy_eips::BlockNumHash;
use alloy_primitives::U256;
use reth_execution_types::{Chain, ExecutionOutcome};
use reth_exex::ExExNotification;
use reth_primitives_traits::RecoveredBlock;
use reth_provider::test_utils::MockEthProvider;
use tempfile::TempDir;
use tempo_primitives::{Block, TempoPrimitives, TempoReceipt};

use super::support::{DevelopmentFixture, RpcScript, zone_provider};
use crate::{
    model::state::ModelState,
    runtime::{
        L1Client, PersistentChecker,
        bootstrap::{create_fresh, open_existing},
        exex::promote_zone_replay_if_ready,
        state::ReadyToAcknowledge,
    },
    store::{
        db::{CheckerStore, Initialization},
        model_state::model_bytes,
        value::BootstrapState,
    },
};

type ZoneProvider = MockEthProvider<TempoPrimitives>;

#[tokio::test]
async fn live_processing_and_creation_after_anchor_rebuild_converge_exactly() {
    let fixture = DevelopmentFixture::new();
    let zone_provider = zone_provider(fixture.zone_genesis, fixture.anchor.tip());
    let archive_directory = TempDir::new().unwrap();
    let archive_path = archive_directory.path().join("archive-rebuilt");
    let live_directory = TempDir::new().unwrap();
    let live_path = live_directory.path().join("live-built");
    let mut archive_rebuilt = fresh_archive_checker(&fixture, &zone_provider, &archive_path).await;
    let mut live_built = live_checker_at_genesis(&fixture, &live_path);

    let (block, receipts, exact_state) = fixture.creation_zone_block();
    let canonical_head = BlockNumHash::new(block.header().number(), block.hash());
    let notification = committed_notification(block, receipts);
    let live_ready = process_creation(&mut live_built, &notification, &exact_state, &fixture).await;
    assert_eq!(live_ready.tip(), canonical_head);
    let archive_ready =
        process_creation(&mut archive_rebuilt, &notification, &exact_state, &fixture).await;
    assert_eq!(archive_ready.tip(), canonical_head);
    assert!(
        promote_zone_replay_if_ready(&mut archive_rebuilt, archive_ready, canonical_head).unwrap(),
        "archive replay at the exact canonical head must hand off to live mode"
    );

    let live_snapshot = live_built.current_snapshot_for_test();
    let rebuilt_snapshot = archive_rebuilt.current_snapshot_for_test();
    assert_eq!(
        rebuilt_snapshot, live_snapshot,
        "archive replay and uninterrupted live processing must persist the same authoritative cut"
    );
    assert_eq!(
        model_bytes(&rebuilt_snapshot.model_rows),
        model_bytes(&live_snapshot.model_rows),
        "authoritative model rows must have identical encoded bytes"
    );

    let before_duplicate = rebuilt_snapshot.clone();
    let mut disconnected_l1 = L1Client::new("duplicate replay must not acquire L1".into());
    let duplicate_ready = archive_rebuilt
        .process_notification_once(&notification, &exact_state, &mut disconnected_l1)
        .await
        .unwrap();
    assert_eq!(duplicate_ready.tip(), canonical_head);
    assert_eq!(
        archive_rebuilt.current_snapshot_for_test(),
        before_duplicate,
        "duplicate delivery after live handoff must not alter authoritative bytes"
    );

    drop(live_built);
    drop(archive_rebuilt);
    let reopened_live = reopen_without_remote_bootstrap(&fixture, &zone_provider, &live_path);
    let reopened_rebuilt = reopen_without_remote_bootstrap(&fixture, &zone_provider, &archive_path);

    let reopened_live_snapshot = reopened_live.current_snapshot_for_test();
    let reopened_rebuilt_snapshot = reopened_rebuilt.current_snapshot_for_test();
    assert_eq!(reopened_live_snapshot, live_snapshot);
    assert_eq!(reopened_rebuilt_snapshot, before_duplicate);
    assert_eq!(reopened_rebuilt_snapshot, reopened_live_snapshot);
    assert_eq!(
        model_bytes(&reopened_rebuilt_snapshot.model_rows),
        model_bytes(&reopened_live_snapshot.model_rows)
    );
}

async fn fresh_archive_checker(
    fixture: &DevelopmentFixture,
    zone_provider: &ZoneProvider,
    path: &Path,
) -> PersistentChecker {
    let bootstrap_rpc = RpcScript::new();
    bootstrap_rpc.push_development_fresh(fixture);
    let checker = create_fresh(
        &fixture.config(),
        fixture.zone_chain_id,
        path,
        zone_provider,
        &bootstrap_rpc.provider(),
    )
    .await
    .unwrap();
    bootstrap_rpc.assert_consumed();

    let snapshot = checker.current_snapshot_for_test();
    assert_eq!(
        snapshot.bootstrap,
        BootstrapState::zone_replay(fixture.anchor.tip())
    );
    assert!(
        snapshot.model.portal().created().is_none(),
        "creation after the genesis anchor must remain pending until ordinary Zone replay"
    );
    checker
}

fn live_checker_at_genesis(fixture: &DevelopmentFixture, path: &Path) -> PersistentChecker {
    let initialization = Initialization::new(
        fixture.identity(),
        BootstrapState::live(),
        BlockNumHash::new(0, fixture.zone_genesis),
        fixture.anchor.tip(),
        ModelState::awaiting_creation(fixture.portal_identity()),
    );
    let store = CheckerStore::create_fresh_at(path, initialization).unwrap();
    PersistentChecker::from_store(store).unwrap()
}

async fn process_creation(
    checker: &mut PersistentChecker,
    notification: &ExExNotification<TempoPrimitives>,
    exact_state: &ZoneProvider,
    fixture: &DevelopmentFixture,
) -> ReadyToAcknowledge {
    let rpc = RpcScript::new();
    rpc.push_observation(&fixture.creation);
    rpc.push_balance(U256::ZERO);
    let mut l1 = L1Client::with_provider(rpc.provider());
    let ready = checker
        .process_notification_once(notification, exact_state, &mut l1)
        .await
        .unwrap();
    rpc.assert_consumed();
    ready
}

fn reopen_without_remote_bootstrap(
    fixture: &DevelopmentFixture,
    zone_provider: &ZoneProvider,
    path: &Path,
) -> PersistentChecker {
    open_existing(
        &fixture.config(),
        fixture.zone_chain_id,
        path,
        fixture.identity(),
        zone_provider,
    )
    .unwrap()
}

fn committed_notification(
    block: RecoveredBlock<Block>,
    receipts: Vec<TempoReceipt>,
) -> ExExNotification<TempoPrimitives> {
    let first_block = block.header().number();
    let outcome = ExecutionOutcome::new(
        Default::default(),
        vec![receipts],
        first_block,
        Default::default(),
    );
    ExExNotification::ChainCommitted {
        new: Arc::new(Chain::new(vec![block], outcome, BTreeMap::new())),
    }
}
