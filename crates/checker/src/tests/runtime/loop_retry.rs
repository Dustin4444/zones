use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use reth_storage_api::{
    StateProviderBox,
    errors::provider::{ProviderError, ProviderResult},
};

use super::*;
use crate::{
    observe::ExactStateLookup,
    runtime::{RuntimeStatus, process_retained_notification},
};

#[tokio::test]
async fn retained_notification_retries_the_same_block_until_acquisition_recovers() {
    let mut fixture = LiveFixture::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let state = FailOnceState {
        inner: exact_zone_state_with_supply(
            &fixture.imported,
            fixture.token,
            U256::from(POST_WITHDRAWAL_SUPPLY),
        ),
        calls: Arc::clone(&calls),
    };
    let collateral = U256::from(INITIAL_SUPPLY);
    let mut l1_client = L1Client::with_provider(l1_provider_with_collateral_sequence(&[
        (&fixture.imported, collateral),
        (&fixture.imported, collateral),
    ]));
    let mut status = RuntimeStatus::new();
    let notification = fixture.notification();

    let ready = process_retained_notification(
        &mut fixture.checker,
        &notification,
        &state,
        &mut l1_client,
        &mut status,
    )
    .await
    .unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(ready.tip(), BlockNumHash::new(1, fixture.block.hash()));
    assert_eq!(
        fixture
            .checker
            .current_snapshot_for_test()
            .verified_zone_tip,
        ready.tip()
    );
}

#[tokio::test]
async fn retained_reorg_retries_after_alert_removal_and_applies_replacement() {
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
    assert!(fixture.checker.is_alerting());
    let old_alert = fixture
        .checker
        .current_snapshot_for_test()
        .active_alert
        .unwrap();

    let replacement = zone_block_with_user_withdrawal_marker(
        1,
        fixture.initialization.verified_zone_tip.hash,
        &fixture.imported,
        Address::repeat_byte(0x53),
        0xe1,
    );
    let replacement_tip = BlockNumHash::new(1, replacement.hash());
    let notification = ExExNotification::ChainReorged {
        old: chain(vec![fixture.block.clone()], vec![fixture.receipts.clone()]),
        new: chain(vec![replacement], vec![fixture.receipts.clone()]),
    };
    let collateral = U256::from(INITIAL_SUPPLY);
    let mut replacement_l1 = L1Client::with_provider(l1_provider_with_collateral_sequence(&[
        (&fixture.imported, collateral),
        (&fixture.imported, collateral),
    ]));
    let calls = Arc::new(AtomicUsize::new(0));
    let state = FailOnceState {
        inner: exact_zone_state_with_supply(
            &fixture.imported,
            fixture.token,
            U256::from(POST_WITHDRAWAL_SUPPLY),
        ),
        calls: Arc::clone(&calls),
    };
    let mut status = RuntimeStatus::new();
    status.mark_started(true);
    assert!(status.is_alerting());

    let ready = process_retained_notification(
        &mut fixture.checker,
        &notification,
        &state,
        &mut replacement_l1,
        &mut status,
    )
    .await
    .unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(ready.tip(), replacement_tip);
    assert!(!ready.is_alerting());
    assert!(!fixture.checker.is_alerting());
    assert!(!status.is_alerting());
    let current = fixture.checker.current_snapshot_for_test();
    assert_eq!(current.verified_zone_tip, replacement_tip);
    assert_eq!(current.active_alert, None);
    assert_eq!(
        fixture.checker.finding_for_test(old_alert.finding).status(),
        crate::store::value::FindingStatus::Orphaned
    );
}

struct FailOnceState {
    inner: TestProvider,
    calls: Arc<AtomicUsize>,
}

impl ExactStateLookup for FailOnceState {
    fn state_by_exact_block_hash(&self, block_hash: B256) -> ProviderResult<StateProviderBox> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(ProviderError::StateForHashNotFound(block_hash));
        }
        self.inner.state_by_exact_block_hash(block_hash)
    }
}
