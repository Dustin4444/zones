use alloy_consensus::Sealable as _;

use super::super::*;

#[tokio::test]
async fn reorg_removing_alert_orphans_it_and_applies_replacement() {
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
    let old_alert = fixture
        .checker
        .current_snapshot_for_test()
        .active_alert
        .unwrap();

    let sender = Address::repeat_byte(0x53);
    let replacement = zone_block_with_user_withdrawal_marker(
        1,
        fixture.initialization.verified_zone_tip.hash,
        &fixture.imported,
        sender,
        0xb1,
    );
    let replacement_tip = BlockNumHash::new(1, replacement.hash());
    let notification = ExExNotification::ChainReorged {
        old: chain(vec![fixture.block.clone()], vec![fixture.receipts.clone()]),
        new: chain(vec![replacement.clone()], vec![fixture.receipts.clone()]),
    };
    let mut replacement_l1 = L1Client::with_provider(l1_provider_with_collateral(
        &fixture.imported,
        U256::from(INITIAL_SUPPLY),
    ));
    let valid_state = exact_zone_state_with_supply(
        &fixture.imported,
        fixture.token,
        U256::from(POST_WITHDRAWAL_SUPPLY),
    );

    let ready = fixture
        .checker
        .process_notification_once(&notification, &valid_state, &mut replacement_l1)
        .await
        .unwrap();

    assert_eq!(ready.tip(), replacement_tip);
    assert!(!ready.is_alerting());
    let current = fixture.checker.current_snapshot_for_test();
    assert_eq!(current.verified_zone_tip, replacement_tip);
    assert_eq!(current.active_alert, None);
    assert_eq!(
        fixture.checker.finding_for_test(old_alert.finding).status(),
        crate::store::value::FindingStatus::Orphaned
    );
    assert_eq!(
        old_alert.last_verified_parent,
        fixture.initialization.verified_zone_tip
    );
}

#[tokio::test]
async fn replacement_divergence_gets_a_new_key_and_retains_the_old_orphan() {
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
    let old_alert = fixture
        .checker
        .current_snapshot_for_test()
        .active_alert
        .unwrap();

    let sender = Address::repeat_byte(0x53);
    let replacement = zone_block_with_user_withdrawal_marker(
        1,
        fixture.initialization.verified_zone_tip.hash,
        &fixture.imported,
        sender,
        0xb2,
    );
    let replacement_tip = BlockNumHash::new(1, replacement.hash());
    let notification = ExExNotification::ChainReorged {
        old: chain(vec![fixture.block.clone()], vec![fixture.receipts.clone()]),
        new: chain(vec![replacement.clone()], vec![fixture.receipts.clone()]),
    };
    let mut replacement_l1 = L1Client::with_provider(l1_provider_with_collateral(
        &fixture.imported,
        U256::from(INITIAL_SUPPLY),
    ));

    let ready = fixture
        .checker
        .process_notification_once(&notification, &divergent_state, &mut replacement_l1)
        .await
        .unwrap();

    assert_eq!(ready.tip(), replacement_tip);
    assert!(ready.is_alerting());
    let current = fixture.checker.current_snapshot_for_test();
    let replacement_alert = current.active_alert.unwrap();
    assert_eq!(replacement_alert.finding.zone_hash(), replacement_tip.hash);
    assert_ne!(replacement_alert.finding, old_alert.finding);
    assert_eq!(
        current.verified_zone_tip,
        fixture.initialization.verified_zone_tip
    );
    assert_eq!(
        fixture.checker.finding_for_test(old_alert.finding).status(),
        crate::store::value::FindingStatus::Orphaned
    );

    let alerted = fixture.checker.current_snapshot_for_test();
    let replay = ExExNotification::ChainReorged {
        old: chain(vec![fixture.block.clone()], Vec::new()),
        new: chain(vec![replacement], Vec::new()),
    };
    let mut disconnected = L1Client::new("not a URL".to_owned());
    let ready = fixture
        .checker
        .process_notification_once(&replay, &UnavailableZoneState, &mut disconnected)
        .await
        .unwrap();

    assert_eq!(ready.tip(), replacement_tip);
    assert!(ready.is_alerting());
    assert_eq!(fixture.checker.current_snapshot_for_test(), alerted);
}

#[tokio::test]
async fn reorg_below_frozen_parent_unwinds_verified_state_before_replacement() {
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
    let old_second = zone_block(2, fixture.block.hash(), &second_imported);
    let old_second_receipts = vec![zone_receipt(&second_imported)];
    let old_second_notification = ExExNotification::ChainCommitted {
        new: chain(vec![old_second.clone()], vec![old_second_receipts.clone()]),
    };
    let mut second_l1 = L1Client::with_provider(l1_provider_with_collateral(
        &second_imported,
        U256::from(50_000),
    ));
    let divergent_second = exact_zone_state_with_supply(
        &second_imported,
        fixture.token,
        U256::from(POST_WITHDRAWAL_SUPPLY + 1),
    );
    fixture
        .checker
        .process_notification_once(&old_second_notification, &divergent_second, &mut second_l1)
        .await
        .unwrap();
    let old_alert = fixture
        .checker
        .current_snapshot_for_test()
        .active_alert
        .unwrap();
    assert_eq!(old_alert.last_verified_parent.number, 1);

    let sender = Address::repeat_byte(0x53);
    let replacement_first = zone_block_with_user_withdrawal_marker(
        1,
        fixture.initialization.verified_zone_tip.hash,
        &fixture.imported,
        sender,
        0xd1,
    );
    let replacement_second = zone_block(2, replacement_first.hash(), &second_imported);
    let replacement_tip = BlockNumHash::new(2, replacement_second.hash());
    let notification = ExExNotification::ChainReorged {
        old: chain(
            vec![fixture.block.clone(), old_second],
            vec![fixture.receipts.clone(), old_second_receipts],
        ),
        new: chain(
            vec![replacement_first.clone(), replacement_second.clone()],
            vec![
                fixture.receipts.clone(),
                vec![zone_receipt(&second_imported)],
            ],
        ),
    };
    let mut replacement_l1 = L1Client::with_provider(l1_provider_with_collateral_sequence(&[
        (&fixture.imported, U256::from(INITIAL_SUPPLY)),
        (&second_imported, U256::from(50_000)),
    ]));
    let state = TwoBlockState {
        first_hash: replacement_first.hash(),
        first: exact_zone_state_with_supply(
            &fixture.imported,
            fixture.token,
            U256::from(POST_WITHDRAWAL_SUPPLY),
        ),
        second_hash: replacement_second.hash(),
        second: exact_zone_state_with_supply(
            &second_imported,
            fixture.token,
            U256::from(POST_WITHDRAWAL_SUPPLY),
        ),
    };

    let ready = fixture
        .checker
        .process_notification_once(&notification, &state, &mut replacement_l1)
        .await
        .unwrap();

    assert_eq!(ready.tip(), replacement_tip);
    assert!(!ready.is_alerting());
    let current = fixture.checker.current_snapshot_for_test();
    assert_eq!(current.verified_zone_tip, replacement_tip);
    assert_eq!(current.active_alert, None);
    assert_eq!(
        fixture.checker.finding_for_test(old_alert.finding).status(),
        crate::store::value::FindingStatus::Orphaned
    );
}
