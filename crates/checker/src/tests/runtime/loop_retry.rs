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
    let status = RuntimeStatus::new();
    let notification = fixture.notification();

    let ready = process_retained_notification(
        &mut fixture.checker,
        &notification,
        &state,
        &mut l1_client,
        &status,
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
