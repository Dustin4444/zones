use alloy_consensus::Sealable as _;

use super::*;

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
async fn reverts_and_reorgs_fail_closed_without_touching_the_parent() {
    let mut fixture = LiveFixture::new();
    let parent = fixture.checker.current_snapshot_for_test();
    let old = chain(vec![fixture.block.clone()], vec![fixture.receipts.clone()]);
    let replacement = chain(vec![fixture.block.clone()], vec![fixture.receipts.clone()]);
    let mut disconnected = L1Client::new("not a URL".to_owned());

    assert!(matches!(
        fixture
            .checker
            .process_notification_once(
                &ExExNotification::ChainReverted { old: old.clone() },
                &UnavailableZoneState,
                &mut disconnected,
            )
            .await,
        Err(RuntimeError::UnsupportedNotification("revert"))
    ));
    assert!(matches!(
        fixture
            .checker
            .process_notification_once(
                &ExExNotification::ChainReorged {
                    old,
                    new: replacement,
                },
                &UnavailableZoneState,
                &mut disconnected,
            )
            .await,
        Err(RuntimeError::UnsupportedNotification("reorg"))
    ));
    assert_eq!(fixture.checker.current_snapshot_for_test(), parent);
}
