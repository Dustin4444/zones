use alloy_consensus::Sealable as _;
use alloy_primitives::{Bytes, Log, LogData, b256};

use super::*;
use crate::{model::constants::ZONE_INBOX_ADDRESS, store::value::FindingKind};

mod descendants;
mod reorg;

#[tokio::test]
async fn unsupported_successful_zone_event_enters_durable_alert_mode() {
    let mut fixture = LiveFixture::new();
    fixture.receipts[0].logs.insert(
        0,
        Log {
            address: ZONE_INBOX_ADDRESS,
            data: LogData::new_unchecked(
                vec![b256!(
                    "4620415fad9c416306a56ca0ee640b3418628a5f2e45ddde3ddf7452a7a654fb"
                )],
                Bytes::new(),
            ),
        },
    );
    assert!(matches!(
        crate::observe::observe_l2_block(&fixture.block, &fixture.receipts),
        Err(crate::observe::ObservationError::ProtocolEvent { .. })
    ));
    let mut disconnected = L1Client::new("not a URL".to_owned());
    let notification = fixture.notification();

    let ready = fixture
        .checker
        .process_notification_once(&notification, &UnavailableZoneState, &mut disconnected)
        .await
        .unwrap();

    assert!(ready.is_alerting());
    assert_eq!(ready.tip(), BlockNumHash::new(1, fixture.block.hash()));
    let snapshot = fixture.checker.current_snapshot_for_test();
    assert_eq!(
        snapshot.verified_zone_tip,
        fixture.initialization.verified_zone_tip
    );
    let record = fixture
        .checker
        .finding_for_test(snapshot.active_alert.unwrap().finding);
    assert!(matches!(
        record.kind(),
        FindingKind::UnsupportedProtocolEvent(..)
    ));
    assert_eq!(
        record.imported_tempo(),
        Some(BlockNumHash::new(L1_NUMBER, fixture.imported.hash_slow()))
    );
}

#[tokio::test]
async fn tempo_continuity_finding_precedes_l1_connection() {
    let mut fixture = LiveFixture::new();
    let imported = imported_child_header(L1_NUMBER + 5, B256::repeat_byte(0xe1));
    let block = zone_block(1, fixture.initialization.verified_zone_tip.hash, &imported);
    let block_tip = BlockNumHash::new(1, block.hash());
    let notification = ExExNotification::ChainCommitted {
        new: chain(vec![block], vec![vec![zone_receipt(&imported)]]),
    };
    let mut disconnected = L1Client::new("not a URL".to_owned());

    let ready = fixture
        .checker
        .process_notification_once(&notification, &UnavailableZoneState, &mut disconnected)
        .await
        .unwrap();

    assert_eq!(ready.tip(), block_tip);
    assert!(ready.is_alerting());
    let snapshot = fixture.checker.current_snapshot_for_test();
    assert_eq!(
        snapshot.verified_zone_tip,
        fixture.initialization.verified_zone_tip
    );
    let record = fixture
        .checker
        .finding_for_test(snapshot.active_alert.unwrap().finding);
    assert!(matches!(record.kind(), FindingKind::TempoContinuity(..)));
    assert_eq!(
        record.imported_tempo(),
        Some(BlockNumHash::new(
            imported.inner.number,
            imported.hash_slow()
        ))
    );
}

#[tokio::test]
async fn first_finding_freezes_model_and_acknowledges_the_notification_suffix() {
    let mut fixture = LiveFixture::new();
    let parent = fixture.checker.current_snapshot_for_test();
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
    let mut l1 = L1Client::with_provider(l1_provider_with_collateral(
        &fixture.imported,
        U256::from(INITIAL_SUPPLY),
    ));
    let divergent_state = exact_zone_state_with_supply(
        &fixture.imported,
        fixture.token,
        U256::from(POST_WITHDRAWAL_SUPPLY + 1),
    );

    let ready = fixture
        .checker
        .process_notification_once(&notification, &divergent_state, &mut l1)
        .await
        .unwrap();

    assert_eq!(ready.tip(), second_tip);
    assert!(ready.is_alerting());
    let frozen = fixture.checker.current_snapshot_for_test();
    assert_eq!(frozen.verified_zone_tip, parent.verified_zone_tip);
    assert_eq!(frozen.imported_tempo_tip, parent.imported_tempo_tip);
    assert_eq!(frozen.model_rows, parent.model_rows);
    let alert = frozen.active_alert.unwrap();
    assert_eq!(alert.finding.zone_hash(), fixture.block.hash());
    assert_eq!(
        fixture
            .checker
            .finding_for_test(alert.finding)
            .imported_tempo(),
        Some(BlockNumHash::new(L1_NUMBER, fixture.imported.hash_slow()))
    );
}
