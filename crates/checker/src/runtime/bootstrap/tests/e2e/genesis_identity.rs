use alloy_primitives::Address;
use tempfile::TempDir;

use super::super::super::create_fresh;
use super::support::*;
use crate::runtime::{RuntimeError, bootstrap::error::BootstrapError};

#[tokio::test]
async fn creation_initial_token_must_match_the_independent_zone_genesis_identity() {
    let fixture = PreGenesisFixture::new();
    let local_initial_token = Address::repeat_byte(0x99);
    let zone_provider = zone_provider_with_initial_token(
        fixture.zone_genesis,
        fixture.anchor.tip(),
        local_initial_token,
    );
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("wrong-initial-token");
    let script = RpcScript::new();
    script.push_creation_authentication_prefix(&fixture);

    let result = create_fresh(
        &fixture.config(None),
        fixture.zone_chain_id,
        &path,
        &zone_provider,
        &script.provider(),
    )
    .await;

    assert!(matches!(
        result,
        Err(RuntimeError::Bootstrap(error))
            if matches!(
                error.as_ref(),
                BootstrapError::InitialTokenMismatch { expected, actual }
                    if *expected == local_initial_token && *actual == fixture.initial_token
            )
    ));
    script.assert_consumed();
    assert!(!path.exists(), "identity mismatch must precede DB creation");
}
