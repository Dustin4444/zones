use alloy_consensus::Header;
use reth_provider::test_utils::MockEthProvider;

use super::*;
use crate::store::{
    schema::FindingKey,
    value::{FindingKind, FindingRecord, FindingStatus},
};

#[test]
fn persistent_runtime_refuses_incomplete_bootstrap() {
    let directory = TempDir::new().unwrap();
    let mut initialization = live_initialization();
    initialization.bootstrap = BootstrapState::zone_replay(initialization.imported_tempo_tip);
    let store = CheckerStore::open(directory.path(), initialization).unwrap();

    assert!(matches!(
        PersistentChecker::from_store(store),
        Err(RuntimeError::Store(StoreError::InvalidBootstrapProgress(
            "persistent live runtime requires completed bootstrap"
        )))
    ));
}

#[tokio::test]
async fn startup_unwinds_a_noncanonical_verified_tip_to_local_ancestor() {
    let mut fixture = LiveFixture::new();
    let candidate = fixture.prepare().await;
    let durable = fixture.checker.commit_block(candidate).unwrap();
    fixture.checker.adopt_block(durable);
    let genesis = fixture.initialization.verified_zone_tip;
    let canonical =
        canonical_hashes([(genesis.number, genesis.hash), (1, B256::repeat_byte(0xee))]);

    let ready = fixture.checker.reconcile_startup(&canonical).unwrap();

    assert_eq!(ready.tip(), genesis);
    assert!(!ready.is_alerting());
    assert_eq!(
        fixture
            .checker
            .current_snapshot_for_test()
            .verified_zone_tip,
        genesis
    );
}

#[test]
fn startup_at_a_canonical_alert_uses_the_node_head_without_replaying_descendants() {
    let directory = TempDir::new().unwrap();
    let initialization = live_initialization();
    let store = CheckerStore::open(directory.path(), initialization.clone()).unwrap();
    let finding_tip = BlockNumHash::new(1, B256::repeat_byte(0xa1));
    let key = FindingKey::new(finding_tip.number, finding_tip.hash, 0);
    let record = FindingRecord::new(
        initialization.verified_zone_tip.hash,
        Some(initialization.imported_tempo_tip),
        FindingStatus::Canonical,
        FindingKind::MissingSupply(initialization.identity.portal_identity().initial_token()),
    )
    .unwrap();
    store
        .activate_finding(key, record, initialization.verified_zone_tip)
        .unwrap();
    let mut checker = PersistentChecker::from_store(store).unwrap();
    let descendant = BlockNumHash::new(2, B256::repeat_byte(0xa3));
    let head = BlockNumHash::new(3, B256::repeat_byte(0xa4));
    let canonical = canonical_hashes([
        (
            initialization.verified_zone_tip.number,
            initialization.verified_zone_tip.hash,
        ),
        (finding_tip.number, finding_tip.hash),
        (descendant.number, descendant.hash),
        (head.number, head.hash),
    ]);

    let ready = checker.reconcile_startup(&canonical).unwrap();

    assert_eq!(ready.tip(), head);
    assert!(ready.is_alerting());
    assert_eq!(checker.mirror_tip(), initialization.verified_zone_tip);
}

#[test]
fn startup_orphans_a_removed_alert_and_resumes_from_verified_parent() {
    let directory = TempDir::new().unwrap();
    let initialization = live_initialization();
    let store = CheckerStore::open(directory.path(), initialization.clone()).unwrap();
    let finding_tip = BlockNumHash::new(1, B256::repeat_byte(0xa2));
    let key = FindingKey::new(finding_tip.number, finding_tip.hash, 0);
    let record = FindingRecord::new(
        initialization.verified_zone_tip.hash,
        Some(initialization.imported_tempo_tip),
        FindingStatus::Canonical,
        FindingKind::MissingSupply(initialization.identity.portal_identity().initial_token()),
    )
    .unwrap();
    store
        .activate_finding(key, record, initialization.verified_zone_tip)
        .unwrap();
    let mut checker = PersistentChecker::from_store(store).unwrap();
    let canonical = canonical_hashes([(
        initialization.verified_zone_tip.number,
        initialization.verified_zone_tip.hash,
    )]);

    let ready = checker.reconcile_startup(&canonical).unwrap();

    assert_eq!(ready.tip(), initialization.verified_zone_tip);
    assert!(!ready.is_alerting());
    assert_eq!(checker.current_snapshot_for_test().active_alert, None);
}

fn canonical_hashes(entries: impl IntoIterator<Item = (u64, B256)>) -> MockEthProvider {
    let provider = MockEthProvider::new();
    for (number, hash) in entries {
        provider.add_header(
            hash,
            Header {
                number,
                ..Default::default()
            },
        );
    }
    provider
}
