use alloy_consensus::Sealable as _;
use reth_storage_api::{
    StateProviderBox,
    errors::provider::{ProviderError, ProviderResult},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use super::*;
use crate::{
    check::finding::CheckError,
    observe::{AcquisitionError, AcquisitionSource, ExactStateLookup},
};

#[tokio::test]
async fn prepared_candidate_is_not_authoritative_before_commit() {
    let fixture = LiveFixture::new();
    let parent = fixture.checker.current_snapshot_for_test();

    let candidate = fixture.prepare().await;

    assert_eq!(fixture.checker.current_snapshot_for_test(), parent);
    assert_eq!(
        fixture.checker.mirror_tip(),
        fixture.initialization.verified_zone_tip
    );
    drop(candidate);
    assert_eq!(fixture.checker.current_snapshot_for_test(), parent);
}

#[tokio::test]
async fn injected_commit_abort_preserves_parent_and_retry_applies_once() {
    let mut fixture = LiveFixture::new();
    let parent = fixture.checker.current_snapshot_for_test();
    let candidate = fixture.prepare().await;

    assert!(matches!(
        fixture.checker.commit_block_aborting_after(candidate, 1),
        Err(RuntimeError::Store(StoreError::InjectedWriteFailure))
    ));
    assert_eq!(fixture.checker.current_snapshot_for_test(), parent);

    drop(fixture.checker);
    fixture.checker = LiveChecker::from_store(
        CheckerStore::open_existing(fixture.directory.path(), fixture.initialization.identity)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(fixture.checker.current_snapshot_for_test(), parent);

    let candidate = fixture.prepare().await;
    let durable = fixture.checker.commit_block(candidate).unwrap();
    let ready = fixture.checker.adopt_block(durable);
    let child = fixture.checker.current_snapshot_for_test();
    assert_eq!(ready.tip(), BlockNumHash::new(1, fixture.block.hash()));
    assert_eq!(child.verified_zone_tip, ready.tip());
    assert_eq!(
        child.model.token(fixture.token).unwrap().accounting(),
        TokenAccounting {
            supply: U256::from(POST_WITHDRAWAL_SUPPLY),
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::from(10),
        }
    );
}

#[tokio::test]
async fn stale_prepared_candidate_cannot_overwrite_a_competing_child() {
    let mut fixture = LiveFixture::new();
    let stale = fixture.prepare().await;
    let sender = Address::repeat_byte(0x53);
    let winner_block = zone_block_with_user_withdrawal_marker(
        1,
        fixture.initialization.verified_zone_tip.hash,
        &fixture.imported,
        sender,
        1,
    );
    assert_ne!(winner_block.hash(), fixture.block.hash());
    let provider = l1_provider_with_collateral(&fixture.imported, U256::from(INITIAL_SUPPLY));
    let zone_state = exact_zone_state_with_supply(
        &fixture.imported,
        fixture.token,
        U256::from(POST_WITHDRAWAL_SUPPLY),
    );
    let winner = fixture
        .checker
        .prepare_block(&provider, &zone_state, &winner_block, &fixture.receipts)
        .await
        .unwrap();
    let winner = fixture.checker.commit_block(winner).unwrap();
    fixture.checker.adopt_block(winner);
    let winner = fixture.checker.current_snapshot_for_test();

    assert!(matches!(
        fixture.checker.commit_block(stale),
        Err(RuntimeError::Store(StoreError::ParentChanged { .. }))
    ));
    assert_eq!(fixture.checker.current_snapshot_for_test(), winner);
}

#[tokio::test]
async fn synchronous_commit_cannot_reenter_exact_state_acquisition() {
    let mut fixture = LiveFixture::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let zone_state = CountingState {
        inner: exact_zone_state_with_supply(
            &fixture.imported,
            fixture.token,
            U256::from(POST_WITHDRAWAL_SUPPLY),
        ),
        calls: Arc::clone(&calls),
    };
    let provider = l1_provider_with_collateral(&fixture.imported, U256::from(INITIAL_SUPPLY));
    let candidate = fixture
        .checker
        .prepare_block(&provider, &zone_state, &fixture.block, &fixture.receipts)
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let durable = fixture.checker.commit_block(candidate).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    fixture.checker.adopt_block(durable);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn commit_before_mirror_restart_reloads_and_acknowledges_without_provider_work() {
    let fixture = LiveFixture::new();
    let candidate = fixture.prepare().await;
    let durable = fixture.checker.commit_block(candidate).unwrap();
    let child_tip = BlockNumHash::new(1, fixture.block.hash());

    assert_eq!(
        fixture
            .checker
            .current_snapshot_for_test()
            .verified_zone_tip,
        child_tip
    );
    assert_eq!(
        fixture.checker.mirror_tip(),
        fixture.initialization.verified_zone_tip,
        "the crash seam intentionally leaves the mirror at its parent"
    );
    drop(durable);
    let notification = fixture.notification();
    let identity = fixture.initialization.identity;
    let directory = fixture.directory;
    drop(fixture.checker);
    let mut restarted =
        LiveChecker::from_store(CheckerStore::open_existing(directory.path(), identity).unwrap())
            .unwrap();
    let mut disconnected = L1Client::new("not a URL".to_owned());

    let ready = restarted
        .process_notification_once(&notification, &UnavailableZoneState, &mut disconnected)
        .await
        .unwrap();

    assert_eq!(restarted.mirror_tip(), child_tip);
    assert_eq!(ready.tip(), child_tip);
}

#[tokio::test]
async fn next_preflight_reloads_the_durable_model_after_an_unadopted_commit() {
    let mut fixture = LiveFixture::new();
    let candidate = fixture.prepare().await;
    let durable = fixture.checker.commit_block(candidate).unwrap();
    drop(durable);

    let second_imported = imported_child_header(L1_NUMBER + 1, fixture.imported.hash_slow());
    let second_block = zone_block(2, fixture.block.hash(), &second_imported);
    let second_tip = BlockNumHash::new(2, second_block.hash());
    let notification = ExExNotification::ChainCommitted {
        new: chain(
            vec![second_block],
            vec![vec![zone_receipt(&second_imported)]],
        ),
    };
    let mut l1_client = L1Client::with_provider(l1_provider_with_collateral(
        &second_imported,
        U256::from(50_000),
    ));
    let zone_state = exact_zone_state_with_supply(
        &second_imported,
        fixture.token,
        U256::from(POST_WITHDRAWAL_SUPPLY),
    );

    let ready = fixture
        .checker
        .process_notification_once(&notification, &zone_state, &mut l1_client)
        .await
        .unwrap();

    assert_eq!(ready.tip(), second_tip);
    assert_eq!(fixture.checker.mirror_tip(), second_tip);
}

#[tokio::test]
async fn acquisition_failure_changes_neither_store_nor_mirror_and_returns_no_ack() {
    let mut fixture = LiveFixture::new();
    let parent = fixture.checker.current_snapshot_for_test();
    let mut l1 = L1Client::with_provider(l1_provider_with_collateral(
        &fixture.imported,
        U256::from(INITIAL_SUPPLY),
    ));

    assert!(matches!(
        fixture
            .checker
            .process_notification_once(&fixture.notification(), &UnavailableZoneState, &mut l1)
            .await,
        Err(RuntimeError::Check(CheckError::Acquisition(
            AcquisitionError::Unavailable {
                kind: AcquisitionSource::ExactZoneState,
                ..
            }
        )))
    ));
    assert_eq!(fixture.checker.current_snapshot_for_test(), parent);
    assert_eq!(
        fixture.checker.mirror_tip(),
        fixture.initialization.verified_zone_tip
    );
}

#[tokio::test]
async fn partial_notification_retry_skips_the_durable_prefix() {
    let mut fixture = LiveFixture::new();
    let second_imported = imported_child_header(L1_NUMBER + 1, fixture.imported.hash_slow());
    let second_block = zone_block(2, fixture.block.hash(), &second_imported);
    let second_tip = BlockNumHash::new(2, second_block.hash());
    let notification = ExExNotification::ChainCommitted {
        new: chain(
            vec![fixture.block.clone(), second_block],
            vec![
                fixture.receipts.clone(),
                vec![zone_receipt(&second_imported)],
            ],
        ),
    };
    let mut first_l1 = L1Client::with_provider(l1_provider_with_collateral_sequence(&[
        (&fixture.imported, U256::from(INITIAL_SUPPLY)),
        (&second_imported, U256::from(50_000)),
    ]));
    let first_state = exact_zone_state_with_supply(
        &fixture.imported,
        fixture.token,
        U256::from(POST_WITHDRAWAL_SUPPLY),
    );
    let second_state = exact_zone_state_with_supply(
        &second_imported,
        fixture.token,
        U256::from(POST_WITHDRAWAL_SUPPLY),
    );
    let unavailable_second = RoutedState {
        first_hash: fixture.block.hash(),
        first: first_state,
        second_hash: second_tip.hash,
        second: None,
    };

    assert!(matches!(
        fixture
            .checker
            .process_notification_once(&notification, &unavailable_second, &mut first_l1)
            .await,
        Err(RuntimeError::Check(CheckError::Acquisition(
            AcquisitionError::Unavailable {
                kind: AcquisitionSource::ExactZoneState,
                ..
            }
        )))
    ));
    assert_eq!(fixture.checker.mirror_tip().number, 1);

    // This client contains data only for the suffix. Success proves replay did
    // not reacquire the already-durable first block.
    let mut suffix_l1 = L1Client::with_provider(l1_provider_with_collateral(
        &second_imported,
        U256::from(50_000),
    ));
    let ready = fixture
        .checker
        .process_notification_once(&notification, &second_state, &mut suffix_l1)
        .await
        .unwrap();
    assert_eq!(ready.tip(), second_tip);
}

#[tokio::test]
async fn receipt_set_gap_is_retryable_and_never_reaches_provider_or_store() {
    let mut fixture = LiveFixture::new();
    let parent = fixture.checker.current_snapshot_for_test();
    let notification = ExExNotification::ChainCommitted {
        new: chain(vec![fixture.block.clone()], Vec::new()),
    };
    let mut disconnected = L1Client::new("not a URL".to_owned());

    assert!(matches!(
        fixture
            .checker
            .process_notification_once(&notification, &UnavailableZoneState, &mut disconnected)
            .await,
        Err(RuntimeError::Check(CheckError::Acquisition(
            AcquisitionError::Inconsistent {
                kind: AcquisitionSource::ZoneNotificationReceipts,
                ..
            }
        )))
    ));
    assert_eq!(fixture.checker.current_snapshot_for_test(), parent);
}

struct RoutedState {
    first_hash: B256,
    first: TestProvider,
    second_hash: B256,
    second: Option<TestProvider>,
}

struct CountingState {
    inner: TestProvider,
    calls: Arc<AtomicUsize>,
}

impl ExactStateLookup for CountingState {
    fn state_by_exact_block_hash(&self, block_hash: B256) -> ProviderResult<StateProviderBox> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.state_by_exact_block_hash(block_hash)
    }
}

impl ExactStateLookup for RoutedState {
    fn state_by_exact_block_hash(&self, block_hash: B256) -> ProviderResult<StateProviderBox> {
        if block_hash == self.first_hash {
            return self.first.state_by_exact_block_hash(block_hash);
        }
        if block_hash == self.second_hash {
            return self.second.as_ref().map_or_else(
                || Err(ProviderError::StateForHashNotFound(block_hash)),
                |provider| provider.state_by_exact_block_hash(block_hash),
            );
        }
        Err(ProviderError::StateForHashNotFound(block_hash))
    }
}
