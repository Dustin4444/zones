//! End-to-end tests for the private zone RPC server.
//!
//! These tests launch a zone node with a private RPC server and verify:
//! - Authentication enforcement (missing/invalid tokens, wrong chain ID)
//! - Public method access
//! - Balance & state privacy (users only see their own data)
//! - Block redaction (activity-derived header fields zeroed, transactions cleared)
//! - Method tier enforcement (restricted/disabled/unknown methods)

use crate::utils::{
    DEFAULT_TIMEOUT, JsonRpcResponseExt, TEST_MNEMONIC, TIP20_TX_GAS, now_secs,
    start_zone_with_private_rpc, start_zone_with_private_rpc_l1,
    start_zone_with_private_rpc_l1_with_encryption,
};
use alloy::{
    primitives::{Address, B256, TxKind, U256, address, hex},
    signers::local::PrivateKeySigner,
};
use alloy_eips::eip2718::Encodable2718;
use alloy_provider::ProviderBuilder;
use alloy_signer::SignerSync;
use alloy_signer_local::{MnemonicBuilder, coins_bip39::English};
use alloy_sol_types::{SolCall, SolError};
use eyre::WrapErr;
use futures::{SinkExt, StreamExt};
use p256::ecdsa::SigningKey as P256SigningKey;
use rand::thread_rng;
use serde_json::{Value, json};
use std::{collections::HashSet, time::Duration};
use tempo_chainspec::spec::{TEMPO_T0_BASE_FEE, TEMPO_T1_BASE_FEE};
use tempo_contracts::precompiles::{
    ITIP20 as ContractTip20,
    account_keychain::IAccountKeychain::SignatureType as KeyInfoSignatureType,
};
use tempo_precompiles::{PATH_USD_ADDRESS, tip20::ITIP20 as PrecompileTip20};
use tempo_primitives::{
    TempoTxEnvelope,
    transaction::{AASigned, Call, PrimitiveSignature, TempoSignature, TempoTransaction},
};
use tempo_zone_contracts::{
    IZoneInbox, TEMPO_STATE_ADDRESS, TempoState, Unauthorized, ZONE_INBOX_ADDRESS,
    ZONE_TOKEN_ADDRESS,
};
use tokio::time::sleep;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use zone_rpc::types::{AuthorizationTokenInfoResponse, ZoneInfoResponse};

alloy::sol! {
    interface IMulticall3 {
        struct Call {
            address target;
            bytes callData;
        }

        function aggregate(Call[] memory calls)
            external
            payable
            returns (uint256 blockNumber, bytes[] memory returnData);
    }
}

fn corrupt_token_hex(token: &str) -> String {
    let mut bytes = hex::decode(token).expect("token hex should decode");
    let idx = usize::from(bytes.len() > 1);
    bytes[idx] ^= 0x01;
    hex::encode(bytes)
}

fn address_topic(address: Address) -> String {
    format!("{:#x}", B256::left_padding_from(address.as_slice()))
}

fn signed_sponsored_raw_transaction(
    signer: &PrivateKeySigner,
    fee_payer: &PrivateKeySigner,
    chain_id: u64,
) -> eyre::Result<String> {
    let mut transaction = TempoTransaction {
        chain_id,
        max_priority_fee_per_gas: TEMPO_T0_BASE_FEE as u128,
        max_fee_per_gas: TEMPO_T0_BASE_FEE as u128,
        gas_limit: 500_000,
        calls: vec![Call {
            to: TxKind::Call(signer.address()),
            value: U256::ZERO,
            input: Default::default(),
        }],
        fee_token: Some(PATH_USD_ADDRESS),
        ..Default::default()
    };

    let fee_payer_hash = transaction.fee_payer_signature_hash(signer.address());
    transaction.fee_payer_signature = Some(fee_payer.sign_hash_sync(&fee_payer_hash)?);

    let signature = signer.sign_hash_sync(&transaction.signature_hash())?;
    let signed = AASigned::new_unhashed(
        transaction,
        TempoSignature::Primitive(PrimitiveSignature::Secp256k1(signature)),
    );
    let envelope: TempoTxEnvelope = signed.into();

    Ok(format!("0x{}", hex::encode(envelope.encoded_2718())))
}

fn assert_filter_not_found_error(response: &serde_json::Value) -> eyre::Result<()> {
    response.expect_error(-32602, "filter not found")?;
    Ok(())
}

fn expect_redacted_block(response: &Value) -> eyre::Result<Value> {
    let block: Value = response.expect_result()?;
    eyre::ensure!(
        !block.is_null(),
        "block should not be null; complete response: {response}"
    );
    eyre::ensure!(
        block["transactions"]
            .as_array()
            .is_some_and(|transactions| transactions.is_empty()),
        "block transactions should be empty (redacted); complete response: {response}"
    );

    let zero_root = format!("{:#x}", B256::ZERO);
    eyre::ensure!(
        block["transactionsRoot"] == zero_root,
        "block transactionsRoot should be zero; complete response: {response}"
    );
    eyre::ensure!(
        block["receiptsRoot"] == zero_root,
        "block receiptsRoot should be zero; complete response: {response}"
    );
    eyre::ensure!(
        block["stateRoot"] == zero_root,
        "block stateRoot should be zero; complete response: {response}"
    );
    eyre::ensure!(
        block["extraData"] == "0x",
        "block extraData should be empty; complete response: {response}"
    );

    if let Some(withdrawals_root) = block.get("withdrawalsRoot") {
        eyre::ensure!(
            withdrawals_root.is_null() || withdrawals_root == zero_root.as_str(),
            "block withdrawalsRoot should be null or zero; complete response: {response}"
        );
    }
    if let Some(bloom) = block.get("logsBloom").and_then(Value::as_str) {
        let bloom_trimmed = bloom.strip_prefix("0x").unwrap_or(bloom);
        eyre::ensure!(
            bloom_trimmed.chars().all(|c| c == '0'),
            "block logsBloom should be all zeros; complete response: {response}"
        );
    }
    eyre::ensure!(
        block["gasUsed"] == "0x0",
        "block gasUsed should be zero; complete response: {response}"
    );
    if let Some(size) = block.get("size") {
        eyre::ensure!(
            size.as_str() == Some("0x0"),
            "block size should be zero; complete response: {response}"
        );
    }
    if let Some(blob_gas_used) = block.get("blobGasUsed") {
        eyre::ensure!(
            blob_gas_used.as_str() == Some("0x0"),
            "block blobGasUsed should be zero; complete response: {response}"
        );
    }
    if let Some(excess_blob_gas) = block.get("excessBlobGas") {
        eyre::ensure!(
            excess_blob_gas.as_str() == Some("0x0"),
            "block excessBlobGas should be zero; complete response: {response}"
        );
    }
    if let Some(withdrawals) = block.get("withdrawals") {
        eyre::ensure!(
            withdrawals
                .as_array()
                .is_some_and(|withdrawals| withdrawals.is_empty()),
            "block withdrawals should be empty when present; complete response: {response}"
        );
    }
    Ok(block)
}

type PrivateRpcWs =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(serde::Deserialize)]
struct LogSubscriptionNotification {
    params: LogSubscriptionParams,
}

#[derive(serde::Deserialize)]
struct LogSubscriptionParams {
    subscription: String,
    result: LogSubscriptionResult,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LogSubscriptionResult {
    transaction_hash: String,
}

fn expect_log_subscription_notification(
    notification: &Value,
    expected_subscription: &str,
) -> eyre::Result<String> {
    let parsed: LogSubscriptionNotification = serde_json::from_value(notification.clone())
        .wrap_err_with(|| {
            format!("malformed private log subscription notification: {notification}")
        })?;
    eyre::ensure!(
        parsed.params.subscription == expected_subscription,
        "expected subscription {expected_subscription}, got {}; complete notification: {notification}",
        parsed.params.subscription,
    );
    Ok(parsed.params.result.transaction_hash)
}

fn private_rpc_ws_url(http_url: &url::Url) -> eyre::Result<url::Url> {
    let mut ws_url = http_url.clone();
    let target_scheme = if ws_url.scheme() == "https" {
        "wss"
    } else {
        "ws"
    };
    ws_url
        .set_scheme(target_scheme)
        .map_err(|_| eyre::eyre!("failed to derive websocket URL"))?;
    Ok(ws_url)
}

fn jsonrpc_with_params(method: &str, params: Value, id: u64) -> String {
    json!({"jsonrpc":"2.0","method":method,"params":params,"id":id}).to_string()
}

async fn connect_private_rpc_ws(url: &url::Url, auth_token: &str) -> eyre::Result<PrivateRpcWs> {
    let ws_url = private_rpc_ws_url(url)?;
    let mut req = ws_url.as_str().into_client_request()?;
    req.headers_mut().insert(
        "x-authorization-token",
        auth_token
            .parse()
            .expect("auth token header should be valid"),
    );
    let (ws, _) = connect_async(req).await?;
    Ok(ws)
}

async fn ws_next_json(ws: &mut PrivateRpcWs) -> eyre::Result<Value> {
    let Some(msg) = tokio::time::timeout(DEFAULT_TIMEOUT, ws.next())
        .await
        .map_err(|_| eyre::eyre!("timed out waiting for websocket message"))?
    else {
        eyre::bail!("websocket closed unexpectedly");
    };

    match msg? {
        Message::Text(text) => Ok(serde_json::from_str(&text)?),
        other => {
            eyre::bail!("expected text websocket message, got {other:?}");
        }
    }
}

async fn ws_subscribe(ws: &mut PrivateRpcWs, params: Value) -> eyre::Result<String> {
    ws.send(Message::Text(
        jsonrpc_with_params("eth_subscribe", params, 1).into(),
    ))
    .await?;
    let response = ws_next_json(ws).await?;
    response.expect_result()
}

async fn ws_collect_messages_until_quiet(
    ws: &mut PrivateRpcWs,
    duration: Duration,
) -> eyre::Result<Vec<Value>> {
    let mut messages = Vec::new();

    loop {
        match tokio::time::timeout(duration, ws.next()).await {
            Err(_) => return Ok(messages),
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) => return Ok(messages),
            Ok(Some(Ok(Message::Text(text)))) => messages.push(serde_json::from_str(&text)?),
            Ok(Some(Ok(other))) => {
                eyre::bail!("unexpected websocket frame: {other:?}");
            }
            Ok(Some(Err(err))) => return Err(err.into()),
        }
    }
}

/// Auth enforcement: missing header → 401, garbage token → 401/403, wrong chain ID → 403.
#[tokio::test(flavor = "multi_thread")]
async fn test_auth_rejection() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let ctx = start_zone_with_private_rpc().await?;

    // No auth header → 401
    let (status, _) = ctx
        .call_no_auth("eth_blockNumber", serde_json::json!([]))
        .await?;
    assert_eq!(status.as_u16(), 401, "missing auth should return 401");

    // Garbage token → 401 or 403
    let (status, _) = ctx
        .call_raw("eth_blockNumber", serde_json::json!([]), "deadbeef")
        .await?;
    assert!(
        status.as_u16() == 401 || status.as_u16() == 403,
        "invalid auth should return 401 or 403, got {status}"
    );

    // Valid signature but wrong chain ID → 403
    let bad_token = ctx.build_bad_token(&ctx.sequencer_signer, 1, ctx.config.chain_id + 1);
    let (status, _) = ctx
        .call_raw("eth_blockNumber", serde_json::json!([]), &bad_token)
        .await?;
    assert_eq!(status.as_u16(), 403, "wrong chain ID should return 403");

    Ok(())
}

/// Pool admission requires the transaction sender to hold an enabled zone token.
#[tokio::test(flavor = "multi_thread")]
async fn test_send_raw_transaction_requires_enabled_token_balance() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let mut ctx = start_zone_with_private_rpc().await?;
    let user_signer = PrivateKeySigner::random();
    let fee_payer = PrivateKeySigner::random();
    ctx.inject_deposit(
        PATH_USD_ADDRESS,
        fee_payer.address(),
        fee_payer.address(),
        1_000_000,
    )
    .await?;
    let raw = signed_sponsored_raw_transaction(&user_signer, &fee_payer, ctx.config.chain_id)?;

    for method in ["eth_sendRawTransaction", "eth_sendRawTransactionSync"] {
        let response = ctx.call_as_user(method, json!([raw]), &user_signer).await?;
        response.expect_error(
            -32603,
            "sender must hold a nonzero balance of an enabled zone token",
        )?;
    }

    ctx.inject_deposit(
        PATH_USD_ADDRESS,
        user_signer.address(),
        user_signer.address(),
        1_000_000,
    )
    .await?;

    let response = ctx
        .call_as_user("eth_sendRawTransaction", json!([raw]), &user_signer)
        .await?;
    let _: String = response.expect_result()?;

    Ok(())
}

/// Real P256 and WebAuthn auth tokens are accepted by the private RPC.
#[tokio::test(flavor = "multi_thread")]
async fn test_non_secp_auth_tokens() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let ctx = start_zone_with_private_rpc().await?;
    let p256_signer = P256SigningKey::random(&mut thread_rng());
    let webauthn_signer = P256SigningKey::random(&mut thread_rng());

    for token in [
        ctx.p256_token(&p256_signer),
        ctx.webauthn_token(&webauthn_signer),
    ] {
        let resp = ctx
            .call("eth_blockNumber", serde_json::json!([]), &token)
            .await?;
        let _: String = resp.expect_result()?;
    }

    Ok(())
}

/// Invalid P256 signatures and WebAuthn challenge mismatches are rejected.
#[tokio::test(flavor = "multi_thread")]
async fn test_invalid_non_secp_auth_tokens_are_rejected() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let ctx = start_zone_with_private_rpc().await?;
    let p256_signer = P256SigningKey::random(&mut thread_rng());
    let webauthn_signer = P256SigningKey::random(&mut thread_rng());

    let bad_p256 = corrupt_token_hex(&ctx.p256_token(&p256_signer));
    let (status, _) = ctx
        .call_raw("eth_blockNumber", serde_json::json!([]), &bad_p256)
        .await?;
    assert_eq!(status.as_u16(), 403, "invalid P256 token should return 403");

    let bad_webauthn = ctx.webauthn_token_with_challenge(&webauthn_signer, B256::repeat_byte(0x77));
    let (status, _) = ctx
        .call_raw("eth_blockNumber", serde_json::json!([]), &bad_webauthn)
        .await?;
    assert_eq!(
        status.as_u16(),
        403,
        "WebAuthn token with wrong challenge should return 403",
    );

    Ok(())
}

/// Authorized P256 keychain tokens authenticate as the root account in both V1 and V2 encodings.
#[tokio::test(flavor = "multi_thread")]
async fn test_keychain_auth_tokens_v1_and_v2() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let mut ctx = start_zone_with_private_rpc().await?;
    let root_signer = PrivateKeySigner::random();
    let access_signer = P256SigningKey::random(&mut thread_rng());

    ctx.inject_deposit(
        PATH_USD_ADDRESS,
        address!("0x0000000000000000000000000000000000001111"),
        root_signer.address(),
        1_000_000,
    )
    .await?;

    let (_, key_id) = ctx.keychain_p256_token(root_signer.address(), &access_signer, 0x04);
    ctx.authorize_keychain_key(
        &root_signer,
        key_id,
        KeyInfoSignatureType::P256,
        now_secs() + 300,
    )
    .await?;

    for version in [0x03, 0x04] {
        let (token, _) = ctx.keychain_p256_token(root_signer.address(), &access_signer, version);
        let resp = ctx
            .call(
                "eth_call",
                serde_json::json!([
                    {
                        "from": format!("{:#x}", root_signer.address()),
                        "to": format!("{:#x}", root_signer.address()),
                        "input": "0x"
                    },
                    "latest"
                ]),
                &token,
            )
            .await?;
        let result: String = resp.expect_result()?;
        assert_eq!(
            result, "0x",
            "keychain auth should allow calls from the root account",
        );
    }

    Ok(())
}

/// Keychain auth rejects missing, revoked, expired, and signature-type-mismatched keys.
#[tokio::test(flavor = "multi_thread")]
async fn test_keychain_auth_rejection_cases() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let mut ctx = start_zone_with_private_rpc().await?;

    let missing_root = PrivateKeySigner::random();
    let missing_access = P256SigningKey::random(&mut thread_rng());
    let (missing_token, _) = ctx.keychain_p256_token(missing_root.address(), &missing_access, 0x04);
    let (status, _) = ctx
        .call_raw("eth_blockNumber", serde_json::json!([]), &missing_token)
        .await?;
    assert_eq!(
        status.as_u16(),
        403,
        "missing keychain auth should return 403"
    );

    let revoked_root = PrivateKeySigner::random();
    let revoked_access = P256SigningKey::random(&mut thread_rng());
    ctx.inject_deposit(
        PATH_USD_ADDRESS,
        address!("0x0000000000000000000000000000000000002222"),
        revoked_root.address(),
        1_000_000,
    )
    .await?;
    let (revoked_token, revoked_key_id) =
        ctx.keychain_p256_token(revoked_root.address(), &revoked_access, 0x04);
    ctx.authorize_keychain_key(
        &revoked_root,
        revoked_key_id,
        KeyInfoSignatureType::P256,
        now_secs() + 300,
    )
    .await?;
    ctx.revoke_keychain_key(&revoked_root, revoked_key_id)
        .await?;
    let (status, _) = ctx
        .call_raw("eth_blockNumber", serde_json::json!([]), &revoked_token)
        .await?;
    assert_eq!(status.as_u16(), 403, "revoked key should return 403");

    let expired_root = PrivateKeySigner::random();
    let expired_access = P256SigningKey::random(&mut thread_rng());
    ctx.inject_deposit(
        PATH_USD_ADDRESS,
        address!("0x0000000000000000000000000000000000003333"),
        expired_root.address(),
        1_000_000,
    )
    .await?;
    let (expired_token, expired_key_id) =
        ctx.keychain_p256_token(expired_root.address(), &expired_access, 0x04);
    ctx.authorize_keychain_key(
        &expired_root,
        expired_key_id,
        KeyInfoSignatureType::P256,
        now_secs() + 1,
    )
    .await?;
    // Intentionally clock-based: exercise real authorization-token expiry, not node readiness.
    sleep(std::time::Duration::from_secs(2)).await;
    let (status, _) = ctx
        .call_raw("eth_blockNumber", serde_json::json!([]), &expired_token)
        .await?;
    assert_eq!(status.as_u16(), 403, "expired key should return 403");

    let mismatch_root = PrivateKeySigner::random();
    let mismatch_access = P256SigningKey::random(&mut thread_rng());
    ctx.inject_deposit(
        PATH_USD_ADDRESS,
        address!("0x0000000000000000000000000000000000004444"),
        mismatch_root.address(),
        1_000_000,
    )
    .await?;
    let (mismatch_token, mismatch_key_id) =
        ctx.keychain_p256_token(mismatch_root.address(), &mismatch_access, 0x04);
    ctx.authorize_keychain_key(
        &mismatch_root,
        mismatch_key_id,
        KeyInfoSignatureType::Secp256k1,
        now_secs() + 300,
    )
    .await?;
    let (status, _) = ctx
        .call_raw("eth_blockNumber", serde_json::json!([]), &mismatch_token)
        .await?;
    assert_eq!(
        status.as_u16(),
        403,
        "signature-type mismatch should return 403",
    );

    Ok(())
}

/// Public methods work for both sequencer and users without leaking private fee activity.
#[tokio::test(flavor = "multi_thread")]
async fn test_public_methods() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let ctx = start_zone_with_private_rpc().await?;
    let user_signer = PrivateKeySigner::random();

    for method in ["eth_blockNumber", "eth_chainId"] {
        let seq_resp = ctx.call_as_sequencer(method, serde_json::json!([])).await?;
        let _: String = seq_resp.expect_result()?;

        let user_resp = ctx
            .call_as_user(method, serde_json::json!([]), &user_signer)
            .await?;
        let _: String = user_resp.expect_result()?;
    }

    for (method, expected) in [
        ("eth_gasPrice", U256::from(TEMPO_T1_BASE_FEE)),
        ("eth_maxPriorityFeePerGas", U256::ZERO),
    ] {
        for response in [
            ctx.call_as_sequencer(method, json!([])).await?,
            ctx.call_as_user(method, json!([]), &user_signer).await?,
        ] {
            let result: String = response.expect_result()?;
            assert_eq!(result, format!("{expected:#x}"));
        }
    }

    Ok(())
}

/// Filter ownership is scoped to the creating account, and uninstall removes follow-up access.
#[tokio::test(flavor = "multi_thread")]
async fn test_filter_ownership_and_uninstall_cleanup() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let mut ctx = start_zone_with_private_rpc().await?;
    let owner_signer = PrivateKeySigner::random();
    let other_signer = PrivateKeySigner::random();

    let create_resp = ctx
        .call_as_user("eth_newBlockFilter", serde_json::json!([]), &owner_signer)
        .await?;
    let filter_id: String = create_resp.expect_result()?;

    ctx.inject_empty_block().await?;

    let owner_changes = ctx
        .call_as_user(
            "eth_getFilterChanges",
            serde_json::json!([filter_id.clone()]),
            &owner_signer,
        )
        .await?;
    let owner_changes: Vec<String> = owner_changes.expect_result()?;
    assert!(
        !owner_changes.is_empty(),
        "owner should observe at least one new block hash"
    );

    let other_changes = ctx
        .call_as_user(
            "eth_getFilterChanges",
            serde_json::json!([filter_id.clone()]),
            &other_signer,
        )
        .await?;
    assert_filter_not_found_error(&other_changes)?;

    let uninstall_resp = ctx
        .call_as_user(
            "eth_uninstallFilter",
            serde_json::json!([filter_id.clone()]),
            &owner_signer,
        )
        .await?;
    assert!(
        uninstall_resp.expect_result::<bool>()?,
        "owner uninstall should succeed",
    );

    let after_uninstall = ctx
        .call_as_user(
            "eth_getFilterChanges",
            serde_json::json!([filter_id]),
            &owner_signer,
        )
        .await?;
    assert_filter_not_found_error(&after_uninstall)?;

    Ok(())
}

/// Balance & state privacy: users see `0x0` for other addresses (balance and nonce),
/// can see their own, and sequencer has full access.
#[tokio::test(flavor = "multi_thread")]
async fn test_balance_privacy() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let mut ctx = start_zone_with_private_rpc().await?;

    let depositor = address!("0x0000000000000000000000000000000000001111");
    let recipient = address!("0x0000000000000000000000000000000000005678");
    let deposit_amount: u128 = 1_000_000;

    ctx.inject_deposit(PATH_USD_ADDRESS, depositor, recipient, deposit_amount)
        .await?;

    let user_signer = PrivateKeySigner::random();

    // User querying another address's balance → 0x0
    let resp = ctx.get_balance_as_user(recipient, &user_signer).await?;
    let balance: String = resp.expect_result()?;
    assert_eq!(
        balance, "0x0",
        "non-owner should see 0x0 balance for other addresses"
    );

    // User querying another address's tx count → 0x0
    let resp = ctx.get_tx_count_as_user(recipient, &user_signer).await?;
    let tx_count: String = resp.expect_result()?;
    assert_eq!(
        tx_count, "0x0",
        "non-owner should see 0x0 for other address's tx count"
    );

    // User querying own balance → works (no error)
    let resp = ctx
        .get_balance_as_user(user_signer.address(), &user_signer)
        .await?;
    let _: String = resp.expect_result()?;

    // Sequencer querying any address → full access
    let resp = ctx.get_balance_as_sequencer(recipient).await?;
    let _: String = resp.expect_result()?;

    Ok(())
}

/// `eth_call` against the zone TIP-20 enforces read privacy for `balanceOf`
/// and `allowance`, while the configured sequencer retains access.
#[tokio::test(flavor = "multi_thread")]
async fn test_tip20_eth_call_privacy() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let mut ctx = start_zone_with_private_rpc().await?;

    let owner_signer = MnemonicBuilder::<English>::default()
        .phrase(TEST_MNEMONIC)
        .build()?;
    let owner = owner_signer.address();
    let spender_signer = PrivateKeySigner::random();
    let spender = spender_signer.address();
    let outsider_signer = PrivateKeySigner::random();

    let deposit_amount: u128 = 1_000_000;
    let allowance_amount: u128 = 333_333;

    ctx.inject_deposit(PATH_USD_ADDRESS, owner, owner, deposit_amount)
        .await?;

    let owner_provider = ProviderBuilder::new()
        .wallet(owner_signer.clone())
        .connect_http(ctx.zone.http_url().clone());
    let approve_pending = ContractTip20::new(PATH_USD_ADDRESS, &owner_provider)
        .approve(spender, U256::from(allowance_amount))
        .gas_price(TEMPO_T0_BASE_FEE as u128)
        .gas(TIP20_TX_GAS)
        .send()
        .await?;
    ctx.fixture.inject_empty_block(ctx.zone.deposit_queue());
    let approve_receipt = approve_pending.get_receipt().await?;
    assert!(approve_receipt.status(), "approve should succeed");
    let expected_owner_balance = ctx.zone.balance_of(PATH_USD_ADDRESS, owner).await?;

    let balance_call = PrecompileTip20::balanceOfCall { account: owner };
    let balance_data = format!("0x{}", hex::encode(balance_call.abi_encode()));
    let allowance_call = PrecompileTip20::allowanceCall { owner, spender };
    let allowance_data = format!("0x{}", hex::encode(allowance_call.abi_encode()));

    let outsider_balance = ctx
        .call_as_user(
            "eth_call",
            serde_json::json!([
                {
                    "to": format!("{PATH_USD_ADDRESS:#x}"),
                    "data": balance_data,
                },
                "latest"
            ]),
            &outsider_signer,
        )
        .await?;
    outsider_balance.expect_error_response()?;

    let outsider_allowance = ctx
        .call_as_user(
            "eth_call",
            serde_json::json!([
                {
                    "to": format!("{PATH_USD_ADDRESS:#x}"),
                    "data": allowance_data,
                },
                "latest"
            ]),
            &outsider_signer,
        )
        .await?;
    outsider_allowance.expect_error_response()?;

    let sequencer_balance = ctx
        .call_as_sequencer(
            "eth_call",
            serde_json::json!([
                {
                    "from": format!("{:#x}", ctx.sequencer_signer.address()),
                    "to": format!("{PATH_USD_ADDRESS:#x}"),
                    "data": format!("0x{}", hex::encode(balance_call.abi_encode())),
                },
                "latest"
            ]),
        )
        .await?;
    let sequencer_balance: String = sequencer_balance.expect_result()?;
    let sequencer_balance_bytes = hex::decode(sequencer_balance.trim_start_matches("0x"))?;
    assert_eq!(
        PrecompileTip20::balanceOfCall::abi_decode_returns(&sequencer_balance_bytes)?,
        expected_owner_balance
    );

    let sequencer_allowance = ctx
        .call_as_sequencer(
            "eth_call",
            serde_json::json!([
                {
                    "from": format!("{:#x}", ctx.sequencer_signer.address()),
                    "to": format!("{PATH_USD_ADDRESS:#x}"),
                    "data": format!("0x{}", hex::encode(allowance_call.abi_encode())),
                },
                "latest"
            ]),
        )
        .await?;
    let sequencer_allowance: String = sequencer_allowance.expect_result()?;
    let sequencer_allowance_bytes = hex::decode(sequencer_allowance.trim_start_matches("0x"))?;
    assert_eq!(
        PrecompileTip20::allowanceCall::abi_decode_returns(&sequencer_allowance_bytes)?,
        U256::from(allowance_amount)
    );

    Ok(())
}

/// `eth_call` against ZoneInbox refund balances is scoped to the authenticated
/// owner, preventing arbitrary `refunds(token, owner)` reads.
#[tokio::test(flavor = "multi_thread")]
async fn test_zone_inbox_refunds_eth_call_privacy() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let ctx = start_zone_with_private_rpc_l1().await?;

    let owner_signer = PrivateKeySigner::random();
    let owner = owner_signer.address();
    let outsider_signer = PrivateKeySigner::random();

    let refunds_call = IZoneInbox::refundsCall {
        token: ZONE_TOKEN_ADDRESS,
        owner,
    };
    let refunds_data = format!("0x{}", hex::encode(refunds_call.abi_encode()));

    let outsider_refunds = ctx
        .call_as_user(
            "eth_call",
            json!([
                {
                    "to": format!("{ZONE_INBOX_ADDRESS:#x}"),
                    "data": refunds_data,
                },
                "latest"
            ]),
            &outsider_signer,
        )
        .await?;
    let outsider_error = outsider_refunds.expect_error_response()?;
    let unauthorized_selector = format!("0x{}", hex::encode(Unauthorized::SELECTOR));
    assert!(
        outsider_error.message.contains("Unauthorized")
            && outsider_error.message.contains(&unauthorized_selector),
        "the ZoneInbox getter must reject a direct non-owner refund read with Unauthorized(): {outsider_refunds}"
    );

    let owner_refunds = ctx
        .call_as_user(
            "eth_call",
            json!([
                {
                    "to": format!("{ZONE_INBOX_ADDRESS:#x}"),
                    "data": format!("0x{}", hex::encode(refunds_call.abi_encode())),
                },
                "latest"
            ]),
            &owner_signer,
        )
        .await?;
    let owner_refunds: String = owner_refunds.expect_result()?;
    let owner_refunds_bytes = hex::decode(owner_refunds.trim_start_matches("0x"))?;
    assert_eq!(
        IZoneInbox::refundsCall::abi_decode_returns(&owner_refunds_bytes)?,
        0,
        "own refunds(token, owner) read should retain normal eth_call behavior"
    );

    let multicall = IMulticall3::aggregateCall {
        calls: vec![IMulticall3::Call {
            target: ZONE_INBOX_ADDRESS,
            callData: refunds_call.abi_encode().into(),
        }],
    };
    let forwarded_refunds = ctx
        .call_as_user(
            "eth_call",
            json!([
                {
                    "to": format!("{:#x}", alloy_provider::MULTICALL3_ADDRESS),
                    "data": format!("0x{}", hex::encode(multicall.abi_encode())),
                },
                "latest"
            ]),
            &outsider_signer,
        )
        .await?;
    forwarded_refunds.expect_error_response()?;

    Ok(())
}

/// Simulation methods reject contract creation and override extensions.
#[tokio::test(flavor = "multi_thread")]
async fn test_simulation_validation_rejects_create_and_overrides() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let ctx = start_zone_with_private_rpc().await?;
    let user_signer = PrivateKeySigner::random();
    let simulation_target = format!("{:#x}", Address::repeat_byte(0x11));

    for method in ["eth_call", "eth_estimateGas"] {
        let create_resp = ctx
            .call_as_user(
                method,
                json!([
                    {
                        "data": "0x60006000f3",
                    },
                    "latest"
                ]),
                &user_signer,
            )
            .await?;
        create_resp.expect_error(-32602, "contract creation not supported on zones")?;

        let user_override_resp = ctx
            .call_as_user(
                method,
                json!([
                    {
                        "to": simulation_target.clone(),
                        "data": "0x"
                    },
                    "latest",
                    {}
                ]),
                &user_signer,
            )
            .await?;
        user_override_resp.expect_error(-32602, "state overrides not allowed")?;
    }

    let fill_resp = ctx
        .call_as_user(
            "eth_fillTransaction",
            json!([
                {
                    "gas": "0x5208",
                }
            ]),
            &user_signer,
        )
        .await?;
    fill_resp.expect_error(-32602, "contract creation not supported on zones")?;

    Ok(())
}

/// Block access control: full=true is rejected for all callers;
/// full=false returns redacted blocks without activity-derived commitments.
#[tokio::test(flavor = "multi_thread")]
async fn test_block_access_control() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let mut ctx = start_zone_with_private_rpc().await?;
    ctx.inject_empty_block().await?;

    let user_signer = PrivateKeySigner::random();

    // full=true → rejected with -32005
    let resp = ctx
        .call_as_user(
            "eth_getBlockByNumber",
            serde_json::json!(["latest", true]),
            &user_signer,
        )
        .await?;
    resp.expect_error_code(-32005)?;

    // full=false → redacted block (empty txs, zeroed logsBloom)
    let resp = ctx
        .call_as_user(
            "eth_getBlockByNumber",
            serde_json::json!(["latest", false]),
            &user_signer,
        )
        .await?;
    let block = expect_redacted_block(&resp)?;

    let block_hash = block["hash"]
        .as_str()
        .ok_or_else(|| eyre::eyre!("redacted block had no hash; complete response: {resp}"))?
        .to_owned();
    let resp = ctx
        .call_as_user(
            "eth_getBlockByHash",
            serde_json::json!([block_hash, false]),
            &user_signer,
        )
        .await?;
    expect_redacted_block(&resp)?;

    Ok(())
}

/// Method tier enforcement: restricted → -32005 for all callers,
/// disabled → -32006 for everyone, unknown → -32601.
#[tokio::test(flavor = "multi_thread")]
async fn test_method_tiers() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let ctx = start_zone_with_private_rpc().await?;
    let user_signer = PrivateKeySigner::random();

    // Restricted methods → -32005 for all callers
    for method in [
        "eth_getCode",
        "eth_getStorageAt",
        "eth_getBlockReceipts",
        "debug_traceTransaction",
        "txpool_content",
    ] {
        let resp = ctx
            .call_as_user(method, serde_json::json!([]), &user_signer)
            .await?;
        resp.expect_error_code(-32005)?;
    }

    // Disabled methods → -32006 for everyone
    for method in [
        "eth_subscribe",
        "eth_newPendingTransactionFilter",
        "eth_mining",
        "eth_hashrate",
    ] {
        let resp = ctx
            .call_as_user(method, serde_json::json!([]), &user_signer)
            .await?;
        resp.expect_error_code(-32006)?;
    }

    // Unknown method → -32601
    let resp = ctx
        .call_as_user(
            "eth_someNonexistentMethod",
            serde_json::json!([]),
            &user_signer,
        )
        .await?;
    resp.expect_error_code(-32601)?;

    Ok(())
}

/// WebSocket log subscriptions are sender-scoped: callers only see logs
/// from their own transactions.
#[tokio::test(flavor = "multi_thread")]
async fn test_ws_logs_subscription_is_sender_scoped() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let mut ctx = start_zone_with_private_rpc().await?;
    let owner_signer = MnemonicBuilder::<English>::default()
        .phrase(TEST_MNEMONIC)
        .build()?;
    let outsider_signer = PrivateKeySigner::random();
    let spender = PrivateKeySigner::random().address();

    ctx.inject_deposit(
        PATH_USD_ADDRESS,
        owner_signer.address(),
        owner_signer.address(),
        1_000_000,
    )
    .await?;
    ctx.inject_deposit(
        PATH_USD_ADDRESS,
        outsider_signer.address(),
        outsider_signer.address(),
        1_000_000,
    )
    .await?;

    let owner_token = ctx.user_token(&owner_signer);
    let mut owner_ws = connect_private_rpc_ws(&ctx.private_rpc_url, &owner_token).await?;

    owner_ws
        .send(Message::Text(
            jsonrpc_with_params(
                "eth_subscribe",
                json!(["logs", {"address": format!("{PATH_USD_ADDRESS:#x}")}]),
                1,
            )
            .into(),
        ))
        .await?;
    let broad_subscription = ws_next_json(&mut owner_ws).await?;
    eyre::ensure!(
        broad_subscription["id"] == 1,
        "broad subscription response had the wrong id; complete response: {broad_subscription}"
    );
    broad_subscription.expect_error_code(-32602)?;

    let owner_subscription = ws_subscribe(
        &mut owner_ws,
        json!([
            "logs",
            {
                "address": format!("{PATH_USD_ADDRESS:#x}"),
                "topics": [null, address_topic(owner_signer.address())],
            }
        ]),
    )
    .await?;

    let owner_provider = ProviderBuilder::new()
        .wallet(owner_signer.clone())
        .connect_http(ctx.zone.http_url().clone());
    let outsider_provider = ProviderBuilder::new()
        .wallet(outsider_signer.clone())
        .connect_http(ctx.zone.http_url().clone());

    let owner_pending = ContractTip20::new(PATH_USD_ADDRESS, &owner_provider)
        .approve(spender, U256::from(111u64))
        .gas_price(TEMPO_T0_BASE_FEE as u128)
        .gas(TIP20_TX_GAS)
        .send()
        .await?;
    let outsider_pending = ContractTip20::new(PATH_USD_ADDRESS, &outsider_provider)
        .approve(spender, U256::from(222u64))
        .gas_price(TEMPO_T0_BASE_FEE as u128)
        .gas(TIP20_TX_GAS)
        .send()
        .await?;

    let owner_hash = *owner_pending.tx_hash();

    ctx.fixture.inject_empty_block(ctx.zone.deposit_queue());

    let owner_receipt = owner_pending.get_receipt().await?;
    let outsider_receipt = outsider_pending.get_receipt().await?;
    assert!(owner_receipt.status(), "owner approve should succeed");
    assert!(outsider_receipt.status(), "outsider approve should succeed");

    let mut owner_notifications = vec![ws_next_json(&mut owner_ws).await?];
    owner_notifications
        .extend(ws_collect_messages_until_quiet(&mut owner_ws, Duration::from_millis(500)).await?);
    let owner_hashes = owner_notifications
        .into_iter()
        .map(|notification| {
            expect_log_subscription_notification(&notification, &owner_subscription)
        })
        .collect::<eyre::Result<HashSet<_>>>()?;
    assert_eq!(owner_hashes, HashSet::from([format!("{owner_hash:#x}")]));

    Ok(())
}

/// Pending transaction subscriptions are disabled because they expose mempool activity.
#[tokio::test(flavor = "multi_thread")]
async fn test_ws_pending_transaction_subscriptions_are_disabled() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let ctx = start_zone_with_private_rpc().await?;
    let user_signer = PrivateKeySigner::random();
    let user_token = ctx.user_token(&user_signer);
    let mut user_ws = connect_private_rpc_ws(&ctx.private_rpc_url, &user_token).await?;

    for (id, params) in [
        (1, json!(["newPendingTransactions"])),
        (2, json!(["newPendingTransactions", true])),
        (3, json!(["newPendingTransactions", {}])),
    ] {
        user_ws
            .send(Message::Text(
                jsonrpc_with_params("eth_subscribe", params, id).into(),
            ))
            .await?;
        let response = ws_next_json(&mut user_ws).await?;
        eyre::ensure!(
            response["id"] == id,
            "pending-subscription response had the wrong id; complete response: {response}"
        );
        response.expect_error_code(-32006)?;
    }

    Ok(())
}

/// Zone-specific metadata methods return the authenticated account/token expiry
/// and the configured zone metadata.
#[tokio::test(flavor = "multi_thread")]
async fn test_zone_metadata_methods() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let ctx = start_zone_with_private_rpc_l1().await?;
    let user_signer = PrivateKeySigner::random();

    let auth_info = ctx
        .call_as_user(
            "zone_getAuthorizationTokenInfo",
            serde_json::json!([]),
            &user_signer,
        )
        .await?;
    let auth_info: AuthorizationTokenInfoResponse = auth_info.expect_result()?;
    assert_eq!(auth_info.account, user_signer.address());
    assert!(
        !auth_info.expires_at.is_zero(),
        "expiresAt should be non-zero"
    );

    let zone_info = ctx
        .call_as_user("zone_getZoneInfo", serde_json::json!([]), &user_signer)
        .await?;
    let zone_info: ZoneInfoResponse = zone_info.expect_result()?;
    assert_eq!(zone_info.zone_id.to::<u64>(), u64::from(ctx.config.zone_id));
    assert!(zone_info.is_access_enforced);
    assert!(!zone_info.is_gateway_open);
    assert_eq!(zone_info.zone_tokens, vec![PATH_USD_ADDRESS],);
    assert_eq!(zone_info.chain_id.to::<u64>(), ctx.config.chain_id);
    let tempo_block_number = TempoState::new(TEMPO_STATE_ADDRESS, ctx.zone.provider())
        .tempoBlockNumber()
        .call()
        .await?;
    assert_eq!(zone_info.tempo_block_number.to::<u64>(), tempo_block_number,);

    Ok(())
}

/// `zone_getZoneInfo` returns every token currently enabled on the portal.
#[tokio::test(flavor = "multi_thread")]
async fn test_zone_get_zone_info_returns_all_enabled_tokens() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let ctx = start_zone_with_private_rpc_l1().await?;
    let user_signer = PrivateKeySigner::random();
    let alpha_salt = B256::with_last_byte(0x44);
    let alpha_token = ctx
        .l1()
        .create_tip20("AlphaUSD", "aUSD", alpha_salt)
        .await?;

    ctx.l1()
        .enable_token_on_portal(ctx.portal_address(), alpha_token)
        .await?;

    let zone_info = ctx
        .call_as_user("zone_getZoneInfo", serde_json::json!([]), &user_signer)
        .await?;
    let zone_tokens = zone_info.expect_result::<ZoneInfoResponse>()?.zone_tokens;

    assert_eq!(zone_tokens, vec![PATH_USD_ADDRESS, alpha_token],);

    Ok(())
}

fn encryption_public_key(secret_key: &k256::SecretKey) -> (String, u8) {
    use k256::elliptic_curve::sec1::ToEncodedPoint;

    let encoded = secret_key.public_key().to_encoded_point(true);
    (
        format!("{:#x}", B256::from_slice(encoded.x().unwrap())),
        encoded.as_bytes()[0],
    )
}

/// The method returns the latest key on Tempo L1 without waiting for the Zone
/// to process the key rotation.
#[tokio::test(flavor = "multi_thread")]
async fn test_zone_get_encryption_key_reads_latest_l1_key() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let ctx = start_zone_with_private_rpc_l1_with_encryption().await?;
    let portal_address = ctx.portal_address();
    let caller = ctx.l1().user_signer();

    let second_key = k256::SecretKey::from_slice(&[0x42; 32])?;
    ctx.l1()
        .set_sequencer_encryption_key(portal_address, &second_key)
        .await?;

    let (second_x, second_prefix) = encryption_public_key(&second_key);
    let second = ctx
        .call_as_user("zone_getEncryptionKey", serde_json::json!([]), &caller)
        .await?;
    let second: Value = second.expect_result()?;
    assert_eq!(
        second,
        serde_json::json!({
            "x": second_x,
            "yParity": second_prefix,
            "keyIndex": "0x1",
        })
    );

    Ok(())
}
