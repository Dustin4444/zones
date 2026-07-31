//! E2E tests for the TIP-403 policy proxy precompile on the zone.
//!
//! These tests verify that the zone TIP-403 precompile correctly serves authorization queries from
//! finalized raw L1 storage via `L1StateCache` and rejects mutating calls. The cache is populated
//! directly in tests (no L1 subscriber).

use alloy::primitives::{Address, TxKind, U256, address};
use alloy_provider::Provider;
use alloy_rpc_types_eth::TransactionRequest;
use tempo_chainspec::spec::TEMPO_T0_BASE_FEE;
use tempo_contracts::precompiles::ITIP403Registry::{self, PolicyType};
use tempo_precompiles::{PATH_USD_ADDRESS, TIP403_REGISTRY_ADDRESS, tip403_registry::AuthRole};
use zone_precompiles::ZONE_FEE_MANAGER_ADDRESS;

use crate::utils::{
    DEFAULT_TIMEOUT, PolicySeed, TIP20_TX_GAS, local_dev_zone_account, make_deposit,
    seed_raw_tip403_policy, seed_raw_tip403_token_policy, start_local_zone_with_fixture,
};

const ALICE: Address = address!("0x000000000000000000000000000000000000A11C");
const BOB: Address = address!("0x0000000000000000000000000000000000000B0B");
const CAROL: Address = address!("0x000000000000000000000000000000000000CA01");

/// Protocol fee collection must use the finalized L1 policy even when the tx does not call TIP-20.
#[tokio::test(flavor = "multi_thread")]
async fn test_l1_blacklisted_sender_cannot_pay_for_empty_transaction() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let (zone, mut fixture) = start_local_zone_with_fixture(10).await?;
    let (alice_provider, alice) = local_dev_zone_account(&zone)?;

    let deposit_amount = 1_000_000u128;
    let deposit = make_deposit(PATH_USD_ADDRESS, alice, alice, deposit_amount);
    fixture.inject_deposits(zone.deposit_queue(), vec![deposit]);
    zone.wait_for_balance(
        PATH_USD_ADDRESS,
        alice,
        U256::from(deposit_amount),
        DEFAULT_TIMEOUT,
    )
    .await?;
    let anchor = zone.wait_for_tempo_block_number(1, DEFAULT_TIMEOUT).await?;

    const BLACKLIST_POLICY_ID: u64 = 42;
    seed_raw_tip403_token_policy(
        &mut zone.l1_state_cache().lock(),
        anchor,
        PATH_USD_ADDRESS,
        BLACKLIST_POLICY_ID,
    );
    seed_raw_tip403_policy(
        zone.l1_state_cache(),
        anchor,
        &[PolicySeed::simple(
            BLACKLIST_POLICY_ID,
            PolicyType::BLACKLIST,
            &[(alice, true), (ZONE_FEE_MANAGER_ADDRESS, false)],
        )],
    )?;

    let request = TransactionRequest {
        to: Some(TxKind::Call(alice)),
        gas: Some(TIP20_TX_GAS),
        gas_price: Some(TEMPO_T0_BASE_FEE as u128),
        ..Default::default()
    };

    let nonce_before = alice_provider.get_transaction_count(alice).await?;
    let error = alice_provider
        .send_transaction(request)
        .await
        .expect_err("L1-blacklisted fee payer transaction must be rejected by the pool");
    assert!(
        error.to_string().contains("PolicyForbids"),
        "unexpected pool rejection: {error}"
    );
    assert_eq!(
        alice_provider.get_transaction_count(alice).await?,
        nonce_before,
        "rejected fee payment must not consume the sender nonce"
    );
    assert_eq!(
        zone.balance_of(PATH_USD_ADDRESS, alice).await?,
        U256::from(deposit_amount),
        "rejected fee payment must leave the sender balance unchanged"
    );

    Ok(())
}

/// List policies: whitelists authorize only set entries (fail-closed), while
/// blacklists authorize everyone except set entries.
#[tokio::test(flavor = "multi_thread")]
async fn test_policy_proxy_list_authorization() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    const LIST_POLICY_ID: u64 = 5;
    for (policy_type, listed_authorized, unlisted_authorized) in [
        (PolicyType::WHITELIST, true, false),
        (PolicyType::BLACKLIST, false, true),
    ] {
        let (zone, mut fixture) = start_local_zone_with_fixture(10).await?;

        seed_raw_tip403_policy(
            zone.l1_state_cache(),
            1,
            &[
                PolicySeed::simple(LIST_POLICY_ID, policy_type, &[(ALICE, true)]),
                PolicySeed::simple(LIST_POLICY_ID, policy_type, &[(BOB, false)]),
            ],
        )?;

        fixture.inject_empty_blocks(zone.deposit_queue(), 3);
        zone.wait_for_tempo_block_number(3, DEFAULT_TIMEOUT).await?;

        let registry = fixture.tip403_registry_check(
            &zone,
            PATH_USD_ADDRESS,
            &[ALICE, BOB],
            1,
            LIST_POLICY_ID,
        )?;
        assert_eq!(
            registry.is_auth_as(ALICE, ALICE, AuthRole::Transfer).await,
            listed_authorized,
            "{policy_type:?}: listed entry"
        );
        assert_eq!(
            registry.is_auth_as(BOB, BOB, AuthRole::Transfer).await,
            unlisted_authorized,
            "{policy_type:?}: unlisted entry"
        );
    }

    Ok(())
}

/// Compound policy: delegates to sub-policies based on role.
#[tokio::test(flavor = "multi_thread")]
async fn test_policy_proxy_compound_policy() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let (zone, mut fixture) = start_local_zone_with_fixture(10).await?;

    const SENDER_WHITELIST_ID: u64 = 5;
    const RECIPIENT_BLACKLIST_ID: u64 = 6;
    const COMPOUND_POLICY_ID: u64 = 10;
    seed_raw_tip403_policy(
        zone.l1_state_cache(),
        1,
        &[
            PolicySeed::simple(SENDER_WHITELIST_ID, PolicyType::WHITELIST, &[(ALICE, true)]),
            PolicySeed::simple(
                RECIPIENT_BLACKLIST_ID,
                PolicyType::BLACKLIST,
                &[(ALICE, false), (BOB, true)],
            ),
            PolicySeed::compound(
                COMPOUND_POLICY_ID,
                SENDER_WHITELIST_ID,
                RECIPIENT_BLACKLIST_ID,
                1,
            ),
        ],
    )?;

    fixture.inject_empty_blocks(zone.deposit_queue(), 3);
    zone.wait_for_tempo_block_number(3, DEFAULT_TIMEOUT).await?;

    let registry = fixture.tip403_registry_check(
        &zone,
        PATH_USD_ADDRESS,
        &[ALICE, BOB],
        1,
        COMPOUND_POLICY_ID,
    )?;

    // Alice is in sender whitelist → authorized as sender
    let alice_sender = registry.is_auth_as(ALICE, ALICE, AuthRole::Sender).await;
    assert!(alice_sender, "ALICE should be authorized as sender");

    // Bob is in recipient blacklist → NOT authorized as recipient
    let bob_recipient = registry.is_auth_as(BOB, ALICE, AuthRole::Recipient).await;
    assert!(
        !bob_recipient,
        "BOB should NOT be authorized as recipient (blacklisted)"
    );

    Ok(())
}

/// Builtin policies: policy 0 = reject all, policy 1 = allow all.
#[tokio::test(flavor = "multi_thread")]
async fn test_policy_proxy_builtin_policies() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let (zone, mut fixture) = start_local_zone_with_fixture(10).await?;

    let registry = fixture.tip403_registry_check(&zone, PATH_USD_ADDRESS, &[ALICE], 1, 0)?;

    fixture.inject_empty_block(zone.deposit_queue());
    zone.wait_for_tempo_block_number(1, DEFAULT_TIMEOUT).await?;
    assert!(
        !registry.is_auth_as(ALICE, ALICE, AuthRole::Transfer).await,
        "policy 0 should reject all"
    );

    let registry = fixture.tip403_registry_check(&zone, PATH_USD_ADDRESS, &[ALICE], 2, 1)?;
    fixture.inject_empty_block(zone.deposit_queue());
    zone.wait_for_tempo_block_number(2, DEFAULT_TIMEOUT).await?;
    assert!(
        registry.is_auth_as(ALICE, ALICE, AuthRole::Transfer).await,
        "policy 1 should allow all"
    );

    Ok(())
}

/// Mutating calls (e.g. createPolicy) should revert with ReadOnlyRegistry.
#[tokio::test(flavor = "multi_thread")]
async fn test_policy_proxy_reverts_mutating_calls() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let (zone, mut fixture) = start_local_zone_with_fixture(10).await?;

    fixture.inject_empty_blocks(zone.deposit_queue(), 3);
    zone.wait_for_tempo_block_number(3, DEFAULT_TIMEOUT).await?;

    let registry = ITIP403Registry::new(TIP403_REGISTRY_ADDRESS, zone.provider());

    // createPolicy should revert
    let result = registry
        .createPolicy(
            address!("0x0000000000000000000000000000000000000001"),
            PolicyType::WHITELIST,
        )
        .call()
        .await;

    assert!(result.is_err(), "createPolicy should revert on zone proxy");

    Ok(())
}

/// Compound policy `isAuthorized` checks BOTH sender AND recipient sub-policies (Transfer role).
#[tokio::test(flavor = "multi_thread")]
async fn test_compound_policy_transfer_role_authorization() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let (zone, mut fixture) = start_local_zone_with_fixture(10).await?;

    // Seed the complete policy membership at block 1.
    seed_raw_tip403_policy(
        zone.l1_state_cache(),
        1,
        &[
            PolicySeed::simple(
                5,
                PolicyType::WHITELIST,
                &[(ALICE, true), (BOB, false), (CAROL, true)],
            ),
            PolicySeed::simple(
                6,
                PolicyType::BLACKLIST,
                &[(ALICE, false), (BOB, true), (CAROL, true)],
            ),
            PolicySeed::compound(10, 5, 6, 1),
        ],
    )?;

    fixture.inject_empty_blocks(zone.deposit_queue(), 3);
    zone.wait_for_tempo_block_number(3, DEFAULT_TIMEOUT).await?;

    let registry =
        fixture.tip403_registry_check(&zone, PATH_USD_ADDRESS, &[ALICE, BOB, CAROL], 1, 10)?;

    // Alice: whitelisted as sender + NOT in recipient blacklist → true
    let alice_auth = registry.is_auth_as(ALICE, ALICE, AuthRole::Transfer).await;
    assert!(
        alice_auth,
        "ALICE should be authorized (passes both sender and recipient checks)"
    );

    // Bob: NOT in sender whitelist → false (short-circuits before recipient check)
    let bob_auth = registry.is_auth_as(BOB, BOB, AuthRole::Transfer).await;
    assert!(
        !bob_auth,
        "BOB should NOT be authorized (not in sender whitelist)"
    );

    // Carol is whitelisted as sender but blacklisted as recipient, so transfer auth fails.
    let carol_auth = registry.is_auth_as(CAROL, CAROL, AuthRole::Transfer).await;
    assert!(
        !carol_auth,
        "CAROL should NOT be authorized (passes sender but fails recipient blacklist)"
    );

    Ok(())
}

/// Block-versioned raw L1 policy writes update the proxy's responses.
#[tokio::test(flavor = "multi_thread")]
async fn test_policy_proxy_uses_block_versioned_raw_state() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let (zone, mut fixture) = start_local_zone_with_fixture(10).await?;

    // Step 1: materialize block-1 state before accepting block 1, then query at anchor 1.
    let registry = fixture.tip403_registry_check(&zone, PATH_USD_ADDRESS, &[ALICE], 1, 5)?;
    seed_raw_tip403_policy(
        zone.l1_state_cache(),
        1,
        &[PolicySeed::simple(
            5,
            PolicyType::WHITELIST,
            &[(ALICE, true)],
        )],
    )?;
    fixture.inject_empty_block(zone.deposit_queue());
    zone.wait_for_tempo_block_number(1, DEFAULT_TIMEOUT).await?;

    let authorized = registry.is_auth_as(ALICE, ALICE, AuthRole::Transfer).await;
    assert!(authorized, "ALICE should be authorized at block 1");

    // Step 2: materialize block-2 state before accepting block 2, then query at anchor 2.
    seed_raw_tip403_policy(
        zone.l1_state_cache(),
        2,
        &[PolicySeed::simple(
            5,
            PolicyType::WHITELIST,
            &[(ALICE, false)],
        )],
    )?;
    fixture.inject_empty_block(zone.deposit_queue());
    zone.wait_for_tempo_block_number(2, DEFAULT_TIMEOUT).await?;

    let authorized = registry.is_auth_as(ALICE, ALICE, AuthRole::Transfer).await;
    assert!(!authorized, "ALICE should NOT be authorized at block 2");

    // Step 3: materialize the compound policy before accepting block 3.
    let registry = fixture.tip403_registry_check(&zone, PATH_USD_ADDRESS, &[ALICE], 3, 10)?;
    seed_raw_tip403_policy(
        zone.l1_state_cache(),
        3,
        &[PolicySeed::compound(10, 5, 1, 1)],
    )?;
    fixture.inject_empty_block(zone.deposit_queue());
    zone.wait_for_tempo_block_number(3, DEFAULT_TIMEOUT).await?;

    // Policy 10 uses the block-2 whitelist where Alice was removed.
    let authorized = registry.is_auth_as(ALICE, ALICE, AuthRole::Transfer).await;
    assert!(!authorized, "compound policy 10 should reject ALICE");

    Ok(())
}
