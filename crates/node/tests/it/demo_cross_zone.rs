//! Demo Flow 3: Cross-Zone Transfer
//!
//! Requires `forge build --root specs/ref-impls` for the Foundry artifacts.

use crate::utils::{
    L1TestNode, WithdrawalArgs, ZoneAccount, ZoneCreationConfig, ZoneTestNode, spawn_sequencer,
};
use alloy::primitives::U256;
use tempo_precompiles::PATH_USD_ADDRESS;
use tempo_zone_contracts::ZONE_TOKEN_ADDRESS;

/// Longer timeout for real L1 tests.
const L1_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Cross-zone transfer via SwapAndDepositRouter between two open zones:
///
/// ```text
///  Zone A (Alice)       L1 (Router)         Zone B (Bob)
///   |--- withdraw 500 -->|                    |
///   |                    |--- deposit 500 --->|
///   |                    |                    |
///   |  ✓ Alice -= 500                  ✓ Bob += 500
/// ```
///
#[tokio::test(flavor = "multi_thread")]
async fn test_cross_zone_send() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let l1 = L1TestNode::start().await?;

    // Separate sequencer keys for each zone to avoid L1 nonce conflicts
    let seq_a_signer = l1.signer_at(2);
    let seq_b_signer = l1.signer_at(3);

    // Bob is a separate user (mnemonic index 4)
    let bob_signer = l1.signer_at(4);
    let bob_address = bob_signer.address();
    let refund_burner = l1.signer_at(5).address();

    let factory = l1.native_zone_factory().await?;
    let portal_a = l1
        .create_zone_with_admin_sequencer_and_config(
            factory,
            l1.dev_address(),
            seq_a_signer.address(),
            ZoneCreationConfig::open(),
        )
        .await?;
    let portal_b = l1
        .create_zone_with_admin_sequencer_and_config(
            factory,
            l1.dev_address(),
            seq_b_signer.address(),
            ZoneCreationConfig::open(),
        )
        .await?;
    let router = l1.deploy_router(factory).await?;

    let zone_a = ZoneTestNode::start_from_l1(l1.http_url(), l1.ws_url(), portal_a).await?;
    let zone_b = ZoneTestNode::start_from_l1(l1.http_url(), l1.ws_url(), portal_b).await?;

    zone_a.wait_for_l2_tempo_finalized(0, L1_TIMEOUT).await?;
    zone_b.wait_for_l2_tempo_finalized(0, L1_TIMEOUT).await?;
    zone_a.assert_access_enforced(false).await?;
    zone_b.assert_access_enforced(false).await?;
    zone_a.assert_gateway_open(true).await?;
    zone_b.assert_gateway_open(true).await?;

    let mut alice = ZoneAccount::from_l1_and_zone(&l1, &zone_a, portal_a);
    let deposit_amount: u128 = 1_000_000; // 1 pathUSD
    l1.fund_user(alice.address(), deposit_amount * 2).await?;
    alice.deposit(deposit_amount, L1_TIMEOUT, &zone_a).await?;

    let _seq_a = spawn_sequencer(&l1, &zone_a, portal_a, seq_a_signer.clone()).await;
    let _seq_b = spawn_sequencer(&l1, &zone_b, portal_b, seq_b_signer.clone()).await;

    // Verify Bob starts with zero on zone_b
    let bob_before = zone_b.balance_of(ZONE_TOKEN_ADDRESS, bob_address).await?;
    assert_eq!(
        bob_before,
        U256::ZERO,
        "Bob should start with zero on zone_b"
    );

    let cross_amount: u128 = 500_000; // 0.5 pathUSD

    // Use the cross_zone_via_router helper but with Bob as the recipient
    let args = WithdrawalArgs::cross_zone_via_router(
        cross_amount,
        router,
        portal_b,
        PATH_USD_ADDRESS,
        bob_address,   // recipient on zone_b is Bob
        refund_burner, // Tempo refund target if Zone B later bounces the deposit
    );
    alice.withdraw_with(args).await?;
    let callback_succeeded = l1
        .wait_for_withdrawal_processed_status(
            portal_a,
            router,
            PATH_USD_ADDRESS,
            cross_amount,
            std::time::Duration::from_secs(60),
        )
        .await?;
    eyre::ensure!(callback_succeeded, "cross-zone router callback failed");

    let cross_timeout = std::time::Duration::from_secs(60);
    zone_b
        .wait_for_balance(
            ZONE_TOKEN_ADDRESS,
            bob_address,
            U256::from(cross_amount),
            cross_timeout,
        )
        .await?;

    let bob_final = zone_b.balance_of(ZONE_TOKEN_ADDRESS, bob_address).await?;
    assert_eq!(
        bob_final,
        U256::from(cross_amount),
        "Bob should have received the cross-zone deposit on zone_b"
    );

    let alice_final = zone_a
        .balance_of(ZONE_TOKEN_ADDRESS, alice.address())
        .await?;
    assert!(
        alice_final <= U256::from(deposit_amount - cross_amount),
        "Alice's zone_a balance should decrease by at least the cross-zone amount (got {alice_final})"
    );

    Ok(())
}
