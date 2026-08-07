use alloy_consensus::Sealable as _;

use super::*;
use crate::store::{db::StoreSnapshot, error::ParentTips};

#[tokio::test]
async fn duplicate_after_mirror_update_is_byte_identical_and_acknowledgeable() {
    let mut fixture = LiveFixture::new();
    let candidate = fixture.prepare().await;
    let durable = fixture.checker.commit_block(candidate).unwrap();
    let first_ready = fixture.checker.adopt_block(durable);
    let child = fixture.checker.current_snapshot_for_test();
    let mut disconnected = L1Client::new("not a URL".to_owned());

    let duplicate_ready = fixture
        .checker
        .process_notification_once(
            &fixture.notification(),
            &UnavailableZoneState,
            &mut disconnected,
        )
        .await
        .unwrap();

    assert_eq!(duplicate_ready, first_ready);
    assert_eq!(fixture.checker.current_snapshot_for_test(), child);
}

#[tokio::test]
async fn retained_old_block_acknowledges_the_newest_durable_tip() {
    let mut fixture = LiveFixture::new();
    let first_notification = fixture.notification();
    let mut first_l1 = L1Client::with_provider(l1_provider_with_collateral(
        &fixture.imported,
        U256::from(INITIAL_SUPPLY),
    ));
    let first_state = exact_zone_state_with_supply(
        &fixture.imported,
        fixture.token,
        U256::from(POST_WITHDRAWAL_SUPPLY),
    );
    fixture
        .checker
        .process_notification_once(&first_notification, &first_state, &mut first_l1)
        .await
        .unwrap();

    let second_imported = imported_child_header(L1_NUMBER + 1, fixture.imported.hash_slow());
    let second_block = zone_block(2, fixture.block.hash(), &second_imported);
    let second_receipt = zone_receipt(&second_imported);
    let second_tip = BlockNumHash::new(2, second_block.hash());
    let second_notification = ExExNotification::ChainCommitted {
        new: chain(vec![second_block], vec![vec![second_receipt]]),
    };
    let mut second_l1 = L1Client::with_provider(l1_provider_with_collateral(
        &second_imported,
        U256::from(50_000),
    ));
    let second_state = exact_zone_state_with_supply(
        &second_imported,
        fixture.token,
        U256::from(POST_WITHDRAWAL_SUPPLY),
    );
    fixture
        .checker
        .process_notification_once(&second_notification, &second_state, &mut second_l1)
        .await
        .unwrap();
    let after_second = fixture.checker.current_snapshot_for_test();
    let mut disconnected = L1Client::new("not a URL".to_owned());

    let ready = fixture
        .checker
        .process_notification_once(
            &first_notification,
            &UnavailableZoneState,
            &mut disconnected,
        )
        .await
        .unwrap();

    assert_eq!(ready.tip(), second_tip);
    assert_eq!(fixture.checker.current_snapshot_for_test(), after_second);
}

#[tokio::test]
async fn same_height_different_hash_fails_before_acquisition_or_write() {
    let mut fixture = LiveFixture::new();
    let candidate = fixture.prepare().await;
    let durable = fixture.checker.commit_block(candidate).unwrap();
    fixture.checker.adopt_block(durable);
    let child = fixture.checker.current_snapshot_for_test();
    let conflicting = zone_block_with_marker(
        1,
        fixture.initialization.verified_zone_tip.hash,
        &fixture.imported,
        0xff,
    );
    assert_ne!(conflicting.hash(), fixture.block.hash());
    let notification = ExExNotification::ChainCommitted {
        new: chain(
            vec![conflicting.clone()],
            vec![vec![zone_receipt(&fixture.imported)]],
        ),
    };
    let mut disconnected = L1Client::new("not a URL".to_owned());

    assert!(matches!(
        fixture
            .checker
            .process_notification_once(&notification, &UnavailableZoneState, &mut disconnected)
            .await,
        Err(RuntimeError::Store(StoreError::CanonicalConflict {
            height: 1,
            expected,
            actual,
        })) if expected == conflicting.hash() && actual == fixture.block.hash()
    ));
    assert_eq!(fixture.checker.current_snapshot_for_test(), child);
}

#[tokio::test]
async fn revert_only_restores_the_exact_parent_and_acknowledges_it() {
    let mut fixture = LiveFixture::new();
    let parent = fixture.checker.current_snapshot_for_test();
    let notification = fixture.notification();
    let mut l1 = L1Client::with_provider(l1_provider_with_collateral(
        &fixture.imported,
        U256::from(INITIAL_SUPPLY),
    ));
    let state = exact_zone_state_with_supply(
        &fixture.imported,
        fixture.token,
        U256::from(POST_WITHDRAWAL_SUPPLY),
    );
    fixture
        .checker
        .process_notification_once(&notification, &state, &mut l1)
        .await
        .unwrap();
    let old = chain(vec![fixture.block.clone()], vec![fixture.receipts.clone()]);
    let mut disconnected = L1Client::new("not a URL".to_owned());

    let ready = fixture
        .checker
        .process_notification_once(
            &ExExNotification::ChainReverted { old },
            &UnavailableZoneState,
            &mut disconnected,
        )
        .await
        .unwrap();

    assert_eq!(ready.tip(), parent.verified_zone_tip);
    assert_eq!(fixture.checker.current_snapshot_for_test(), parent);
}

#[tokio::test]
async fn one_block_reorg_unwinds_old_then_applies_replacement() {
    let mut fixture = LiveFixture::new();
    let first = fixture.notification();
    let mut first_l1 = L1Client::with_provider(l1_provider_with_collateral(
        &fixture.imported,
        U256::from(INITIAL_SUPPLY),
    ));
    let state = exact_zone_state_with_supply(
        &fixture.imported,
        fixture.token,
        U256::from(POST_WITHDRAWAL_SUPPLY),
    );
    fixture
        .checker
        .process_notification_once(&first, &state, &mut first_l1)
        .await
        .unwrap();

    let sender = Address::repeat_byte(0x53);
    let replacement = zone_block_with_user_withdrawal_marker(
        1,
        fixture.initialization.verified_zone_tip.hash,
        &fixture.imported,
        sender,
        fixture.token,
        0xa1,
    );
    assert_ne!(replacement.hash(), fixture.block.hash());
    let replacement_tip = BlockNumHash::new(1, replacement.hash());
    let notification = ExExNotification::ChainReorged {
        old: chain(vec![fixture.block.clone()], vec![fixture.receipts.clone()]),
        new: chain(vec![replacement], vec![fixture.receipts.clone()]),
    };
    let mut replacement_l1 = L1Client::with_provider(l1_provider_with_collateral(
        &fixture.imported,
        U256::from(INITIAL_SUPPLY),
    ));

    let ready = fixture
        .checker
        .process_notification_once(&notification, &state, &mut replacement_l1)
        .await
        .unwrap();

    assert_eq!(ready.tip(), replacement_tip);
    assert_eq!(fixture.checker.mirror_tip(), replacement_tip);
    assert_eq!(
        fixture
            .checker
            .current_snapshot_for_test()
            .verified_zone_tip,
        replacement_tip
    );
}

#[tokio::test]
async fn multi_block_reorg_unwinds_newest_first_and_applies_oldest_first() {
    let mut fixture = LiveFixture::new();
    let fork = TwoBlockFork::new(&fixture);
    let old = apply_old_branch(&mut fixture, &fork).await;
    let state = fork.replacement_state(&fixture);
    let mut l1 = fork.replacement_l1(&fixture);

    let ready = fixture
        .checker
        .process_notification_once(&fork.reorg(&fixture), &state, &mut l1)
        .await
        .unwrap();

    assert_eq!(ready.tip(), fork.replacement_tip());
    assert!(!ready.is_alerting());
    let replacement = fixture.checker.current_snapshot_for_test();
    assert_eq!(replacement.verified_zone_tip, fork.replacement_tip());
    assert_eq!(replacement.imported_tempo_tip, old.tip.imported_tempo_tip);
    assert_eq!(replacement.model_rows, old.tip.model_rows);
}

#[tokio::test]
async fn multi_block_reorg_resumes_after_one_durable_unwind_and_restart() {
    let mut fixture = LiveFixture::new();
    let fork = TwoBlockFork::new(&fixture);
    let old = apply_old_branch(&mut fixture, &fork).await;
    let identity = fixture.initialization.identity;

    // Simulate process death after the newest old block's per-block unwind
    // committed but before the remaining old prefix was restored.
    drop(fixture.checker);
    let store = CheckerStore::open_existing(fixture.directory.path(), identity).unwrap();
    assert_eq!(
        store.unwind_tip(fork.old_second_tip()).unwrap(),
        ParentTips::new(
            old.after_first.verified_zone_tip,
            old.after_first.imported_tempo_tip
        )
    );
    assert_eq!(store.load_current().unwrap(), old.after_first);
    drop(store);
    fixture.checker = PersistentChecker::from_store(
        CheckerStore::open_existing(fixture.directory.path(), identity).unwrap(),
    )
    .unwrap();

    let state = fork.replacement_state(&fixture);
    let mut l1 = fork.replacement_l1(&fixture);
    let ready = fixture
        .checker
        .process_notification_once(&fork.reorg(&fixture), &state, &mut l1)
        .await
        .unwrap();

    assert_eq!(ready.tip(), fork.replacement_tip());
    assert!(!ready.is_alerting());
    let replacement = fixture.checker.current_snapshot_for_test();
    assert_eq!(replacement.verified_zone_tip, fork.replacement_tip());
    assert_eq!(replacement.imported_tempo_tip, old.tip.imported_tempo_tip);
    assert_eq!(replacement.model_rows, old.tip.model_rows);
}

struct TwoBlockFork {
    imported: TempoHeader,
    old_second: reth_primitives_traits::RecoveredBlock<Block>,
    old_second_receipts: Vec<TempoReceipt>,
    replacement_first: reth_primitives_traits::RecoveredBlock<Block>,
    replacement_second: reth_primitives_traits::RecoveredBlock<Block>,
    replacement_second_receipts: Vec<TempoReceipt>,
}

impl TwoBlockFork {
    fn new(fixture: &LiveFixture) -> Self {
        let imported = imported_child_header(L1_NUMBER + 1, fixture.imported.hash_slow());
        let old_second = zone_block(2, fixture.block.hash(), &imported);
        let old_second_receipts = vec![zone_receipt(&imported)];
        let replacement_first = zone_block_with_user_withdrawal_marker(
            1,
            fixture.initialization.verified_zone_tip.hash,
            &fixture.imported,
            Address::repeat_byte(0x53),
            fixture.token,
            0xa2,
        );
        let replacement_second = zone_block(2, replacement_first.hash(), &imported);
        let replacement_second_receipts = vec![zone_receipt(&imported)];
        Self {
            imported,
            old_second,
            old_second_receipts,
            replacement_first,
            replacement_second,
            replacement_second_receipts,
        }
    }

    fn old_second_tip(&self) -> BlockNumHash {
        BlockNumHash::new(2, self.old_second.hash())
    }

    fn replacement_tip(&self) -> BlockNumHash {
        BlockNumHash::new(2, self.replacement_second.hash())
    }

    fn reorg(&self, fixture: &LiveFixture) -> ExExNotification<TempoPrimitives> {
        ExExNotification::ChainReorged {
            old: chain(
                vec![fixture.block.clone(), self.old_second.clone()],
                vec![fixture.receipts.clone(), self.old_second_receipts.clone()],
            ),
            new: chain(
                vec![
                    self.replacement_first.clone(),
                    self.replacement_second.clone(),
                ],
                vec![
                    fixture.receipts.clone(),
                    self.replacement_second_receipts.clone(),
                ],
            ),
        }
    }

    fn replacement_state(&self, fixture: &LiveFixture) -> TwoBlockState {
        TwoBlockState {
            first_hash: self.replacement_first.hash(),
            first: exact_zone_state_with_supply(
                &fixture.imported,
                fixture.token,
                U256::from(POST_WITHDRAWAL_SUPPLY),
            ),
            second_hash: self.replacement_second.hash(),
            second: exact_zone_state_with_supply(
                &self.imported,
                fixture.token,
                U256::from(POST_WITHDRAWAL_SUPPLY),
            ),
        }
    }

    fn replacement_l1(&self, fixture: &LiveFixture) -> L1Client {
        L1Client::with_provider(l1_provider_with_collateral_sequence(&[
            (&fixture.imported, U256::from(INITIAL_SUPPLY)),
            (&self.imported, U256::from(50_000)),
        ]))
    }
}

async fn apply_old_branch(fixture: &mut LiveFixture, fork: &TwoBlockFork) -> AppliedOldBranch {
    let mut first_l1 = L1Client::with_provider(l1_provider_with_collateral(
        &fixture.imported,
        U256::from(INITIAL_SUPPLY),
    ));
    let first_state = exact_zone_state_with_supply(
        &fixture.imported,
        fixture.token,
        U256::from(POST_WITHDRAWAL_SUPPLY),
    );
    fixture
        .checker
        .process_notification_once(&fixture.notification(), &first_state, &mut first_l1)
        .await
        .unwrap();
    let after_first = fixture.checker.current_snapshot_for_test();

    let notification = ExExNotification::ChainCommitted {
        new: chain(
            vec![fork.old_second.clone()],
            vec![fork.old_second_receipts.clone()],
        ),
    };
    let mut second_l1 = L1Client::with_provider(l1_provider_with_collateral(
        &fork.imported,
        U256::from(50_000),
    ));
    let second_state = exact_zone_state_with_supply(
        &fork.imported,
        fixture.token,
        U256::from(POST_WITHDRAWAL_SUPPLY),
    );
    fixture
        .checker
        .process_notification_once(&notification, &second_state, &mut second_l1)
        .await
        .unwrap();

    AppliedOldBranch {
        after_first,
        tip: fixture.checker.current_snapshot_for_test(),
    }
}

struct AppliedOldBranch {
    after_first: StoreSnapshot,
    tip: StoreSnapshot,
}
