use alloy_consensus::Sealable as _;

use super::super::*;

#[tokio::test]
async fn alert_restart_and_descendant_commit_need_no_acquisition() {
    let mut fixture = LiveFixture::new();
    let mut l1 = L1Client::with_provider(l1_provider_with_collateral(
        &fixture.imported,
        U256::from(INITIAL_SUPPLY),
    ));
    let divergent_state = exact_zone_state_with_supply(
        &fixture.imported,
        fixture.token,
        U256::from(POST_WITHDRAWAL_SUPPLY + 1),
    );
    fixture
        .checker
        .process_notification_once(&fixture.notification(), &divergent_state, &mut l1)
        .await
        .unwrap();

    let identity = fixture.initialization.identity;
    drop(fixture.checker);
    let store = CheckerStore::open_existing(fixture.directory.path(), identity).unwrap();
    fixture.checker = LiveChecker::from_store(store).unwrap();
    assert!(fixture.checker.is_alerting());

    let second_imported = imported_child_header(L1_NUMBER + 1, fixture.imported.hash_slow());
    let second = zone_block(2, fixture.block.hash(), &second_imported);
    let second_tip = BlockNumHash::new(2, second.hash());
    let notification = ExExNotification::ChainCommitted {
        // Missing receipts are deliberate: alert mode does not inspect a
        // descendant's observation payload.
        new: chain(vec![second], Vec::new()),
    };
    let mut disconnected = L1Client::new("not a URL".to_owned());
    let frozen = fixture.checker.current_snapshot_for_test();

    let ready = fixture
        .checker
        .process_notification_once(&notification, &UnavailableZoneState, &mut disconnected)
        .await
        .unwrap();

    assert_eq!(ready.tip(), second_tip);
    assert!(ready.is_alerting());
    assert_eq!(fixture.checker.current_snapshot_for_test(), frozen);
}

#[tokio::test]
async fn descendant_only_reorg_preserves_alert_without_acquisition() {
    let mut fixture = LiveFixture::new();
    let mut first_l1 = L1Client::with_provider(l1_provider_with_collateral(
        &fixture.imported,
        U256::from(INITIAL_SUPPLY),
    ));
    let divergent_state = exact_zone_state_with_supply(
        &fixture.imported,
        fixture.token,
        U256::from(POST_WITHDRAWAL_SUPPLY + 1),
    );
    fixture
        .checker
        .process_notification_once(&fixture.notification(), &divergent_state, &mut first_l1)
        .await
        .unwrap();
    let frozen = fixture.checker.current_snapshot_for_test();

    let second_imported = imported_child_header(L1_NUMBER + 1, fixture.imported.hash_slow());
    let old = zone_block(2, fixture.block.hash(), &second_imported);
    let replacement = zone_block_with_marker(2, fixture.block.hash(), &second_imported, 0xc1);
    assert_ne!(old.hash(), replacement.hash());
    let replacement_tip = BlockNumHash::new(2, replacement.hash());
    let notification = ExExNotification::ChainReorged {
        old: chain(vec![old], Vec::new()),
        new: chain(vec![replacement], Vec::new()),
    };
    let mut disconnected = L1Client::new("not a URL".to_owned());

    let ready = fixture
        .checker
        .process_notification_once(&notification, &UnavailableZoneState, &mut disconnected)
        .await
        .unwrap();

    assert_eq!(ready.tip(), replacement_tip);
    assert!(ready.is_alerting());
    assert_eq!(fixture.checker.current_snapshot_for_test(), frozen);
}

#[tokio::test]
async fn alert_rejects_descendant_notifications_with_the_wrong_finding_parent() {
    let mut fixture = LiveFixture::new();
    let mut first_l1 = L1Client::with_provider(l1_provider_with_collateral(
        &fixture.imported,
        U256::from(INITIAL_SUPPLY),
    ));
    let divergent_state = exact_zone_state_with_supply(
        &fixture.imported,
        fixture.token,
        U256::from(POST_WITHDRAWAL_SUPPLY + 1),
    );
    fixture
        .checker
        .process_notification_once(&fixture.notification(), &divergent_state, &mut first_l1)
        .await
        .unwrap();
    let frozen = fixture.checker.current_snapshot_for_test();

    let second_imported = imported_child_header(L1_NUMBER + 1, fixture.imported.hash_slow());
    let wrong_parent = zone_block(2, B256::repeat_byte(0xee), &second_imported);
    let notification = ExExNotification::ChainReverted {
        old: chain(vec![wrong_parent.clone()], Vec::new()),
    };
    let mut disconnected = L1Client::new("not a URL".to_owned());

    assert!(matches!(
        fixture
            .checker
            .process_notification_once(&notification, &UnavailableZoneState, &mut disconnected)
            .await,
        Err(RuntimeError::InvalidNotificationChain { .. })
    ));
    assert_eq!(fixture.checker.current_snapshot_for_test(), frozen);

    let notification = ExExNotification::ChainCommitted {
        new: chain(vec![wrong_parent], Vec::new()),
    };
    assert!(matches!(
        fixture
            .checker
            .process_notification_once(&notification, &UnavailableZoneState, &mut disconnected)
            .await,
        Err(RuntimeError::InvalidNotificationChain { .. })
    ));
    assert_eq!(fixture.checker.current_snapshot_for_test(), frozen);
}

#[tokio::test]
async fn alert_rejects_notifications_wholly_below_the_finding() {
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
    let second = zone_block(2, fixture.block.hash(), &second_imported);
    let second_notification = ExExNotification::ChainCommitted {
        new: chain(vec![second], vec![vec![zone_receipt(&second_imported)]]),
    };
    let mut second_l1 = L1Client::with_provider(l1_provider_with_collateral(
        &second_imported,
        U256::from(50_000),
    ));
    let divergent_state = exact_zone_state_with_supply(
        &second_imported,
        fixture.token,
        U256::from(POST_WITHDRAWAL_SUPPLY + 1),
    );
    fixture
        .checker
        .process_notification_once(&second_notification, &divergent_state, &mut second_l1)
        .await
        .unwrap();
    let frozen = fixture.checker.current_snapshot_for_test();

    let notification = ExExNotification::ChainReverted {
        old: chain(vec![fixture.block.clone()], Vec::new()),
    };
    let mut disconnected = L1Client::new("not a URL".to_owned());
    assert!(matches!(
        fixture
            .checker
            .process_notification_once(&notification, &UnavailableZoneState, &mut disconnected)
            .await,
        Err(RuntimeError::InvalidNotificationChain { .. })
    ));
    assert_eq!(fixture.checker.current_snapshot_for_test(), frozen);

    let notification = ExExNotification::ChainCommitted {
        new: chain(vec![fixture.block.clone()], Vec::new()),
    };
    assert!(matches!(
        fixture
            .checker
            .process_notification_once(&notification, &UnavailableZoneState, &mut disconnected)
            .await,
        Err(RuntimeError::InvalidNotificationChain { .. })
    ));
    assert_eq!(fixture.checker.current_snapshot_for_test(), frozen);
}
