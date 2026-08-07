use std::{collections::HashMap, sync::Arc, time::Duration};

use alloy_primitives::{Address, Bytes, b256};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use futures::{SinkExt, StreamExt};
use jsonrpsee::{PendingSubscriptionSink, RpcModule, types::ErrorObjectOwned};
use p256::ecdsa::SigningKey as P256SigningKey;
use parking_lot::Mutex;
use rand::thread_rng;
use serde_json::{Value, json};
use tempo_contracts::precompiles::account_keychain::IAccountKeychain::{
    KeyInfo, SignatureType as KeyInfoSignatureType,
};
use tokio::sync::Notify;
use tokio_tungstenite::{connect_async, tungstenite};
use zone_rpc::{
    RedactedRpcConfig, RedactedRpcModule, auth::build_token_fields, start_redacted_rpc,
    types::JsonRpcError,
};

#[path = "../../test-utils/auth_tokens.rs"]
mod auth_tokens;

use auth_tokens::{
    build_token_with_signature, now_secs, sign_keychain_signature, sign_p256_signature,
    sign_webauthn_signature,
};

// ---------------------------------------------------------------------------
// Mock API
// ---------------------------------------------------------------------------

#[derive(Default)]
struct MockRpc {
    key_infos: Mutex<HashMap<(Address, Address), KeyInfo>>,
    key_lookup_error: Option<&'static str>,
    ws_subscriptions_enabled: bool,
    blocking_rpc_started: Option<Arc<Notify>>,
}

impl MockRpc {
    fn with_key(account: Address, key_id: Address, signature_type: KeyInfoSignatureType) -> Self {
        let mut key_infos = HashMap::new();
        key_infos.insert(
            (account, key_id),
            KeyInfo {
                signatureType: signature_type,
                keyId: key_id,
                expiry: u64::MAX,
                enforceLimits: false,
                isRevoked: false,
            },
        );
        Self {
            key_infos: Mutex::new(key_infos),
            key_lookup_error: None,
            ws_subscriptions_enabled: false,
            blocking_rpc_started: None,
        }
    }

    fn with_key_and_blocking_request(
        account: Address,
        key_id: Address,
        signature_type: KeyInfoSignatureType,
    ) -> (Self, Arc<Notify>) {
        let mut api = Self::with_key(account, key_id, signature_type);
        let started = Arc::new(Notify::new());
        api.blocking_rpc_started = Some(started.clone());
        (api, started)
    }

    fn with_key_lookup_error(message: &'static str) -> Self {
        Self {
            key_infos: Mutex::new(HashMap::new()),
            key_lookup_error: Some(message),
            ws_subscriptions_enabled: false,
            blocking_rpc_started: None,
        }
    }

    fn with_ws_subscriptions() -> Self {
        Self {
            key_infos: Mutex::new(HashMap::new()),
            key_lookup_error: None,
            ws_subscriptions_enabled: true,
            blocking_rpc_started: None,
        }
    }

    fn revoke_key(&self, account: Address, key_id: Address) {
        if let Some(key_info) = self.key_infos.lock().get_mut(&(account, key_id)) {
            key_info.isRevoked = true;
        }
    }
}

impl MockRpc {
    async fn get_keychain_key(&self, account: Address, key_id: Address) -> eyre::Result<KeyInfo> {
        if let Some(message) = self.key_lookup_error {
            return Err(eyre::eyre!(message));
        }
        Ok(self
            .key_infos
            .lock()
            .get(&(account, key_id))
            .cloned()
            .unwrap_or(KeyInfo {
                signatureType: KeyInfoSignatureType::Secp256k1,
                keyId: Address::ZERO,
                expiry: 0,
                enforceLimits: false,
                isRevoked: false,
            }))
    }

    fn rpc_module(self: Arc<Self>) -> RedactedRpcModule {
        let mut module = RpcModule::from_arc(self.clone());
        module
            .register_method("eth_blockNumber", |_, _, _| {
                zone_rpc::types::to_raw(&"0x42")
            })
            .unwrap();
        module
            .register_method("eth_chainId", |_, _, _| zone_rpc::types::to_raw(&"0x1"))
            .unwrap();
        module
            .register_async_method("eth_sendRawTransactionSync", |params, api, _| async move {
                let _: (Bytes,) = params
                    .parse()
                    .map_err(|_| JsonRpcError::invalid_params("expected [data]"))?;
                let started = api.blocking_rpc_started.clone();
                if let Some(started) = started {
                    started.notify_one();
                    std::future::pending::<()>().await;
                }
                Err::<Box<serde_json::value::RawValue>, _>(JsonRpcError::internal(
                    "not implemented",
                ))
            })
            .unwrap();
        module
            .register_subscription(
                "eth_subscribe",
                "eth_subscription",
                "eth_unsubscribe",
                mock_subscription,
            )
            .unwrap();

        let keychain_api = self;
        RedactedRpcModule::new(module, move |account, key_id| {
            let api = keychain_api.clone();
            async move { api.get_keychain_key(account, key_id).await }
        })
    }
}

async fn mock_subscription(
    params: jsonrpsee::types::Params<'static>,
    pending: PendingSubscriptionSink,
    api: Arc<MockRpc>,
    _extensions: jsonrpsee::Extensions,
) {
    let params: Vec<Value> = match params.parse() {
        Ok(params) => params,
        Err(_) => {
            pending
                .reject(ErrorObjectOwned::from(JsonRpcError::invalid_params(
                    "expected [subscription, params?]",
                )))
                .await;
            return;
        }
    };
    let kind = params.first().and_then(Value::as_str);
    let item = match kind {
        Some("newHeads") if params.len() == 1 => json!({
            "hash": format!(
                "{:#x}",
                b256!("0x4444444444444444444444444444444444444444444444444444444444444444")
            ),
            "number": "0x42",
            "parentHash": format!("{:#x}", alloy_primitives::B256::ZERO),
            "logsBloom": format!("0x{}", "0".repeat(512)),
            "gasUsed": "0x0",
            "size": "0x0",
            "transactionsRoot": format!("{:#x}", alloy_primitives::B256::ZERO),
            "receiptsRoot": format!("{:#x}", alloy_primitives::B256::ZERO),
            "stateRoot": format!("{:#x}", alloy_primitives::B256::ZERO),
            "extraData": "0x",
        }),
        Some("logs") if params.get(1).is_none_or(Value::is_object) => json!({
            "address": format!("{:#x}", Address::ZERO),
            "topics": [format!(
                "{:#x}",
                b256!("0x1111111111111111111111111111111111111111111111111111111111111111")
            )],
            "data": "0x",
            "blockHash": format!(
                "{:#x}",
                b256!("0x2222222222222222222222222222222222222222222222222222222222222222")
            ),
            "blockNumber": "0x42",
            "transactionHash": format!(
                "{:#x}",
                b256!("0x3333333333333333333333333333333333333333333333333333333333333333")
            ),
            "transactionIndex": "0x0",
            "logIndex": "0x0",
            "removed": false
        }),
        Some("newPendingTransactions" | "syncing" | "newPendingTransactionsWithBody") => {
            pending
                .reject(ErrorObjectOwned::from(JsonRpcError::method_disabled()))
                .await;
            return;
        }
        _ => {
            pending
                .reject(ErrorObjectOwned::from(JsonRpcError::invalid_params(
                    "invalid subscription parameters",
                )))
                .await;
            return;
        }
    };

    if !api.ws_subscriptions_enabled {
        pending
            .reject(ErrorObjectOwned::from(JsonRpcError::method_disabled()))
            .await;
        return;
    }
    let sink = match pending.accept().await {
        Ok(sink) => sink,
        Err(_) => return,
    };
    let item = zone_rpc::types::to_raw(&item).unwrap();
    let _ = sink.send(item).await;
    sink.closed().await;
}

// ---------------------------------------------------------------------------
// Test context
// ---------------------------------------------------------------------------

const ZONE_ID: u32 = 1;
const CHAIN_ID: u64 = 42;

struct TestContext {
    addr: std::net::SocketAddr,
    signer: PrivateKeySigner,
}

impl TestContext {
    async fn start(api: MockRpc) -> Self {
        Self::start_shared(Arc::new(api)).await
    }

    async fn start_shared(api: Arc<MockRpc>) -> Self {
        let signer = PrivateKeySigner::random();
        let config = RedactedRpcConfig {
            listen_addr: ([127, 0, 0, 1], 0).into(),
            l1_rpc_url: "http://127.0.0.1:1".to_string(),
            zone_rpc_url: "http://127.0.0.1:1".to_string(),
            retry_connection_interval: std::time::Duration::from_millis(100),
            zone_id: ZONE_ID,
            chain_id: CHAIN_ID,
            max_auth_token_validity: zone_rpc::auth::DEFAULT_MAX_AUTH_TOKEN_VALIDITY,
            zone_portal: Address::ZERO,
        };
        let addr = start_redacted_rpc(config, api.rpc_module()).await.unwrap();
        Self { addr, signer }
    }

    fn build_token(&self) -> String {
        let now = now_secs();
        self.build_token_expiring_at(now, now + 600)
    }

    fn build_token_expiring_at(&self, issued_at: u64, expires_at: u64) -> String {
        let (fields, digest) = build_token_fields(ZONE_ID, CHAIN_ID, issued_at, expires_at);
        let sig = self.signer.sign_hash_sync(&digest).expect("signing failed");

        let mut blob = Vec::with_capacity(65 + fields.len());
        blob.extend_from_slice(&sig.r().to_be_bytes::<32>());
        blob.extend_from_slice(&sig.s().to_be_bytes::<32>());
        blob.push(sig.v() as u8);
        blob.extend_from_slice(&fields);

        alloy_primitives::hex::encode(&blob)
    }

    fn ws_url(&self) -> String {
        format!("ws://{}", self.addr)
    }

    fn http_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

/// Build a JSON-RPC request string.
fn jsonrpc(method: &str, id: u64) -> String {
    serde_json::json!({"jsonrpc":"2.0","method":method,"params":[],"id":id}).to_string()
}

fn jsonrpc_with_params(method: &str, params: Value, id: u64) -> String {
    serde_json::json!({"jsonrpc":"2.0","method":method,"params":params,"id":id}).to_string()
}

/// Connect to the WS endpoint using the X-Authorization-Token header.
async fn connect_with_header(
    ctx: &TestContext,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let token = ctx.build_token();
    connect_with_token(&ctx.ws_url(), ctx.addr, &token)
        .await
        .expect("ws connect failed")
}

async fn connect_with_token(
    ws_url: &str,
    addr: std::net::SocketAddr,
    token: &str,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tungstenite::Error,
> {
    let req = tungstenite::http::Request::builder()
        .uri(ws_url)
        .header("x-authorization-token", token)
        .header(
            "sec-websocket-key",
            tungstenite::handshake::client::generate_key(),
        )
        .header("host", addr.to_string())
        .header("upgrade", "websocket")
        .header("connection", "upgrade")
        .header("sec-websocket-version", "13")
        .body(())
        .unwrap();

    connect_async(req).await.map(|(ws, _)| ws)
}

/// Parse a JSON-RPC response from a WS text message.
fn parse_response(msg: tungstenite::Message) -> Value {
    match msg {
        tungstenite::Message::Text(t) => serde_json::from_str(&t).expect("invalid json"),
        other => panic!("expected text message, got {other:?}"),
    }
}

async fn http_call(ctx: &TestContext, token: Option<&str>, request: Value) -> reqwest::Response {
    let mut request = reqwest::Client::new().post(ctx.http_url()).json(&request);
    if let Some(token) = token {
        request = request.header(zone_rpc::auth::X_AUTHORIZATION_TOKEN, token);
    }
    request.send().await.expect("HTTP request failed")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn http_roundtrip_with_header_auth() {
    let ctx = TestContext::start(MockRpc::default()).await;
    let response = http_call(
        &ctx,
        Some(&ctx.build_token()),
        json!({"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["result"], "0x42");
    assert_eq!(body["id"], 1);
}

#[tokio::test]
async fn http_rejects_missing_auth() {
    let ctx = TestContext::start(MockRpc::default()).await;
    let response = http_call(
        &ctx,
        None,
        json!({"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn http_rejects_websocket_subscription_method() {
    let ctx = TestContext::start(MockRpc::with_ws_subscriptions()).await;
    let response = http_call(
        &ctx,
        Some(&ctx.build_token()),
        json!({"jsonrpc":"2.0","method":"eth_subscribe","params":["newHeads"],"id":1}),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], -32006);
    assert_eq!(body["error"]["message"], "Method disabled");
}

#[tokio::test]
async fn ws_roundtrip_with_header_auth() {
    let ctx = TestContext::start(MockRpc::default()).await;
    let mut ws = connect_with_header(&ctx).await;

    ws.send(tungstenite::Message::Text(
        jsonrpc("eth_blockNumber", 1).into(),
    ))
    .await
    .unwrap();
    let resp = parse_response(ws.next().await.unwrap().unwrap());

    assert_eq!(resp["id"], 1);
    assert_eq!(resp["result"], "0x42");
}

#[tokio::test]
async fn ws_roundtrip_with_query_auth() {
    let ctx = TestContext::start(MockRpc::default()).await;
    let token = ctx.build_token();
    let url = format!("{}/?token={token}", ctx.ws_url());

    let (mut ws, _) = connect_async(&url).await.expect("ws connect failed");

    ws.send(tungstenite::Message::Text(
        jsonrpc("eth_blockNumber", 1).into(),
    ))
    .await
    .unwrap();
    let resp = parse_response(ws.next().await.unwrap().unwrap());

    assert_eq!(resp["id"], 1);
    assert_eq!(resp["result"], "0x42");
}

#[tokio::test]
async fn ws_reject_no_auth() {
    let ctx = TestContext::start(MockRpc::default()).await;
    let result = connect_async(ctx.ws_url()).await;
    // Server should reject the upgrade — tungstenite surfaces this as an error
    // with the HTTP 401 status.
    let err = result.expect_err("should fail without auth");
    let tungstenite::Error::Http(response) = err else {
        panic!("expected HTTP error, got {err:?}");
    };
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn ws_reject_invalid_token() {
    let ctx = TestContext::start(MockRpc::default()).await;
    let req = tungstenite::http::Request::builder()
        .uri(ctx.ws_url())
        .header("x-authorization-token", "deadbeef")
        .header(
            "sec-websocket-key",
            tungstenite::handshake::client::generate_key(),
        )
        .header("host", ctx.addr.to_string())
        .header("upgrade", "websocket")
        .header("connection", "upgrade")
        .header("sec-websocket-version", "13")
        .body(())
        .unwrap();

    let err = connect_async(req)
        .await
        .expect_err("should fail with bad token");
    let tungstenite::Error::Http(response) = err else {
        panic!("expected HTTP error, got {err:?}");
    };
    assert_eq!(response.status(), 403);
}

#[tokio::test]
async fn ws_multiple_requests() {
    let ctx = TestContext::start(MockRpc::default()).await;
    let mut ws = connect_with_header(&ctx).await;

    for i in 1..=5 {
        ws.send(tungstenite::Message::Text(
            jsonrpc("eth_blockNumber", i).into(),
        ))
        .await
        .unwrap();
        let resp = parse_response(ws.next().await.unwrap().unwrap());
        assert_eq!(resp["id"], i);
        assert_eq!(resp["result"], "0x42");
    }
}

#[tokio::test]
async fn ws_batch_request() {
    let ctx = TestContext::start(MockRpc::default()).await;
    let mut ws = connect_with_header(&ctx).await;

    let batch = serde_json::json!([
        {"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1},
        {"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":2},
    ]);
    ws.send(tungstenite::Message::Text(batch.to_string().into()))
        .await
        .unwrap();

    let resp = parse_response(ws.next().await.unwrap().unwrap());
    let arr = resp.as_array().expect("expected batch response array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["id"], 1);
    assert_eq!(arr[0]["result"], "0x42");
    assert_eq!(arr[1]["id"], 2);
    assert_eq!(arr[1]["result"], "0x1");
}

#[tokio::test]
async fn ws_invalid_json() {
    let ctx = TestContext::start(MockRpc::default()).await;
    let mut ws = connect_with_header(&ctx).await;

    ws.send(tungstenite::Message::Text("{broken".into()))
        .await
        .unwrap();
    let resp = parse_response(ws.next().await.unwrap().unwrap());

    assert_eq!(resp["error"]["code"], -32700);
}

#[tokio::test]
async fn ws_unknown_method() {
    let ctx = TestContext::start(MockRpc::default()).await;
    let mut ws = connect_with_header(&ctx).await;

    ws.send(tungstenite::Message::Text(jsonrpc("eth_foobar", 1).into()))
        .await
        .unwrap();
    let resp = parse_response(ws.next().await.unwrap().unwrap());

    assert_eq!(resp["id"], 1);
    assert_eq!(resp["error"]["code"], -32601);
}

#[tokio::test]
async fn ws_disabled_subscription_method() {
    let ctx = TestContext::start(MockRpc::default()).await;
    let mut ws = connect_with_header(&ctx).await;

    ws.send(tungstenite::Message::Text(
        jsonrpc_with_params("eth_subscribe", json!(["newHeads"]), 1).into(),
    ))
    .await
    .unwrap();
    let resp = parse_response(ws.next().await.unwrap().unwrap());

    assert_eq!(resp["id"], 1);
    assert_eq!(resp["error"]["code"], -32006);
}

#[tokio::test]
async fn ws_subscribe_new_heads_emits_redacted_headers() {
    let ctx = TestContext::start(MockRpc::with_ws_subscriptions()).await;
    let mut ws = connect_with_header(&ctx).await;

    ws.send(tungstenite::Message::Text(
        jsonrpc_with_params("eth_subscribe", json!(["newHeads"]), 1).into(),
    ))
    .await
    .unwrap();
    let resp = parse_response(ws.next().await.unwrap().unwrap());

    assert_eq!(resp["id"], 1);
    let subscription_id = resp["result"].as_str().expect("subscription id");

    let notification = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("timed out waiting for newHeads notification")
        .unwrap()
        .unwrap();
    let notification = parse_response(notification);

    assert_eq!(notification["method"], "eth_subscription");
    assert_eq!(notification["params"]["subscription"], subscription_id);
    assert_eq!(
        notification["params"]["result"]["hash"],
        format!(
            "{:#x}",
            b256!("0x4444444444444444444444444444444444444444444444444444444444444444")
        )
    );
    assert_eq!(
        notification["params"]["result"]["logsBloom"],
        format!("0x{}", "0".repeat(512))
    );
    assert!(
        notification["params"]["result"]
            .get("transactions")
            .is_none()
    );
}

#[tokio::test]
async fn ws_subscribe_logs_emits_notifications() {
    let ctx = TestContext::start(MockRpc::with_ws_subscriptions()).await;
    let mut ws = connect_with_header(&ctx).await;

    ws.send(tungstenite::Message::Text(
        jsonrpc_with_params("eth_subscribe", json!(["logs", {}]), 1).into(),
    ))
    .await
    .unwrap();
    let resp = parse_response(ws.next().await.unwrap().unwrap());

    assert_eq!(resp["id"], 1);
    let subscription_id = resp["result"].as_str().expect("subscription id");

    let notification = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("timed out waiting for log notification")
        .unwrap()
        .unwrap();
    let notification = parse_response(notification);

    assert_eq!(notification["method"], "eth_subscription");
    assert_eq!(notification["params"]["subscription"], subscription_id);
    assert_eq!(notification["params"]["result"]["blockNumber"], "0x42");
}

#[tokio::test]
async fn ws_subscribe_pending_transactions_is_disabled() {
    let ctx = TestContext::start(MockRpc::with_ws_subscriptions()).await;
    let mut ws = connect_with_header(&ctx).await;

    for (id, params) in [
        (1, json!(["newPendingTransactions"])),
        (2, json!(["newPendingTransactions", true])),
        (3, json!(["newPendingTransactions", {}])),
    ] {
        ws.send(tungstenite::Message::Text(
            jsonrpc_with_params("eth_subscribe", params, id).into(),
        ))
        .await
        .unwrap();
        let resp = parse_response(ws.next().await.unwrap().unwrap());

        assert_eq!(resp["id"], id);
        assert_eq!(resp["error"]["code"], -32006);
    }
}

#[tokio::test]
async fn ws_unsubscribe_removes_subscription() {
    let ctx = TestContext::start(MockRpc::with_ws_subscriptions()).await;
    let mut ws = connect_with_header(&ctx).await;

    ws.send(tungstenite::Message::Text(
        jsonrpc_with_params("eth_subscribe", json!(["logs", {}]), 1).into(),
    ))
    .await
    .unwrap();
    let resp = parse_response(ws.next().await.unwrap().unwrap());
    let subscription_id = resp["result"].clone();

    ws.send(tungstenite::Message::Text(
        jsonrpc_with_params("eth_unsubscribe", json!([subscription_id]), 2).into(),
    ))
    .await
    .unwrap();
    let resp = loop {
        let message = parse_response(ws.next().await.unwrap().unwrap());
        if message["id"] == 2 {
            break message;
        }
    };

    assert_eq!(resp["id"], 2);
    assert_eq!(resp["result"], true);
}

#[tokio::test]
async fn ws_subscribe_rejects_invalid_param_shapes() {
    let ctx = TestContext::start(MockRpc::with_ws_subscriptions()).await;
    let mut ws = connect_with_header(&ctx).await;

    for (id, params) in [(1, json!(["newHeads", false])), (2, json!(["logs", false]))] {
        ws.send(tungstenite::Message::Text(
            jsonrpc_with_params("eth_subscribe", params, id).into(),
        ))
        .await
        .unwrap();
        let resp = parse_response(ws.next().await.unwrap().unwrap());
        assert_eq!(resp["id"], id);
        assert_eq!(resp["error"]["code"], -32602);
    }
}

#[tokio::test]
async fn ws_subscribe_rejects_too_many_active_subscriptions() {
    let ctx = TestContext::start(MockRpc::with_ws_subscriptions()).await;
    let mut ws = connect_with_header(&ctx).await;

    for id in 1..=32 {
        ws.send(tungstenite::Message::Text(
            jsonrpc_with_params("eth_subscribe", json!(["newHeads"]), id).into(),
        ))
        .await
        .unwrap();

        let resp = loop {
            let message = parse_response(ws.next().await.unwrap().unwrap());
            if message["id"] == id {
                break message;
            }
        };

        assert!(resp["result"].as_str().is_some());
    }

    ws.send(tungstenite::Message::Text(
        jsonrpc_with_params("eth_subscribe", json!(["newHeads"]), 33).into(),
    ))
    .await
    .unwrap();

    let resp = loop {
        let message = parse_response(ws.next().await.unwrap().unwrap());
        if message["id"] == 33 {
            break message;
        }
    };

    assert_eq!(resp["error"]["code"], -32006);
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Too many subscriptions")
    );
}

#[tokio::test]
async fn ws_empty_batch() {
    let ctx = TestContext::start(MockRpc::default()).await;
    let mut ws = connect_with_header(&ctx).await;

    ws.send(tungstenite::Message::Text("[]".into()))
        .await
        .unwrap();
    let resp = parse_response(ws.next().await.unwrap().unwrap());

    assert_eq!(resp["error"]["code"], -32600);
}

#[tokio::test]
async fn ws_roundtrip_with_p256_auth() {
    let ctx = TestContext::start(MockRpc::default()).await;
    let signing_key = P256SigningKey::random(&mut thread_rng());
    let now = now_secs();
    let (fields, digest) = build_token_fields(ZONE_ID, CHAIN_ID, now, now + 600);
    let token = build_token_with_signature(
        sign_p256_signature(digest, &signing_key).expect("p256 signing should succeed"),
        &fields,
    );
    let mut ws = connect_with_token(&ctx.ws_url(), ctx.addr, &token)
        .await
        .expect("p256 ws connect failed");

    ws.send(tungstenite::Message::Text(
        jsonrpc("eth_blockNumber", 9).into(),
    ))
    .await
    .unwrap();
    let resp = parse_response(ws.next().await.unwrap().unwrap());

    assert_eq!(resp["id"], 9);
    assert_eq!(resp["result"], "0x42");
}

#[tokio::test]
async fn ws_roundtrip_with_webauthn_auth() {
    let ctx = TestContext::start(MockRpc::default()).await;
    let signing_key = P256SigningKey::random(&mut thread_rng());
    let now = now_secs();
    let (fields, digest) = build_token_fields(ZONE_ID, CHAIN_ID, now, now + 600);
    let token = build_token_with_signature(
        sign_webauthn_signature(&signing_key, digest).expect("webauthn signing should succeed"),
        &fields,
    );
    let mut ws = connect_with_token(&ctx.ws_url(), ctx.addr, &token)
        .await
        .expect("webauthn ws connect failed");

    ws.send(tungstenite::Message::Text(
        jsonrpc("eth_chainId", 10).into(),
    ))
    .await
    .unwrap();
    let resp = parse_response(ws.next().await.unwrap().unwrap());

    assert_eq!(resp["id"], 10);
    assert_eq!(resp["result"], "0x1");
}

#[tokio::test]
async fn ws_roundtrip_with_keychain_auth() {
    let root_account = Address::repeat_byte(0x55);
    let access_signer = P256SigningKey::random(&mut thread_rng());
    let now = now_secs();
    let (fields, digest) = build_token_fields(ZONE_ID, CHAIN_ID, now, now + 600);
    let (signature, key_id) = sign_keychain_signature(digest, &access_signer, root_account, 0x04)
        .expect("keychain signing should succeed");
    let ctx = TestContext::start(MockRpc::with_key(
        root_account,
        key_id,
        KeyInfoSignatureType::P256,
    ))
    .await;
    let token = build_token_with_signature(signature, &fields);
    let mut ws = connect_with_token(&ctx.ws_url(), ctx.addr, &token)
        .await
        .expect("authorized keychain ws connect failed");

    ws.send(tungstenite::Message::Text(
        jsonrpc("eth_blockNumber", 11).into(),
    ))
    .await
    .unwrap();
    let resp = parse_response(ws.next().await.unwrap().unwrap());

    assert_eq!(resp["id"], 11);
    assert_eq!(resp["result"], "0x42");
}

#[tokio::test]
async fn ws_closes_when_auth_token_expires() {
    let ctx = TestContext::start(MockRpc::default()).await;
    let now = now_secs();
    let token = ctx.build_token_expiring_at(now, now + 1);
    let mut ws = connect_with_token(&ctx.ws_url(), ctx.addr, &token)
        .await
        .expect("ws connect failed");

    let _closed = tokio::time::timeout(Duration::from_secs(3), ws.next())
        .await
        .expect("timed out waiting for token-expiry close");
}

#[tokio::test]
async fn ws_closes_when_keychain_key_is_revoked() {
    let root_account = Address::repeat_byte(0x77);
    let access_signer = P256SigningKey::random(&mut thread_rng());
    let now = now_secs();
    let (fields, digest) = build_token_fields(ZONE_ID, CHAIN_ID, now, now + 600);
    let (signature, key_id) = sign_keychain_signature(digest, &access_signer, root_account, 0x04)
        .expect("keychain signing should succeed");
    let api = Arc::new(MockRpc::with_key(
        root_account,
        key_id,
        KeyInfoSignatureType::P256,
    ));
    let ctx = TestContext::start_shared(api.clone()).await;
    let token = build_token_with_signature(signature, &fields);
    let mut ws = connect_with_token(&ctx.ws_url(), ctx.addr, &token)
        .await
        .expect("authorized keychain ws connect failed");

    api.revoke_key(root_account, key_id);

    let _closed = tokio::time::timeout(Duration::from_secs(3), ws.next())
        .await
        .expect("timed out waiting for keychain-revocation close");
}

#[tokio::test]
async fn ws_closes_when_keychain_key_is_revoked_during_request() {
    let root_account = Address::repeat_byte(0x78);
    let access_signer = P256SigningKey::random(&mut thread_rng());
    let now = now_secs();
    let (fields, digest) = build_token_fields(ZONE_ID, CHAIN_ID, now, now + 600);
    let (signature, key_id) = sign_keychain_signature(digest, &access_signer, root_account, 0x04)
        .expect("keychain signing should succeed");
    let (api, blocking_rpc_started) =
        MockRpc::with_key_and_blocking_request(root_account, key_id, KeyInfoSignatureType::P256);
    let api = Arc::new(api);
    let ctx = TestContext::start_shared(api.clone()).await;
    let token = build_token_with_signature(signature, &fields);
    let mut ws = connect_with_token(&ctx.ws_url(), ctx.addr, &token)
        .await
        .expect("authorized keychain ws connect failed");

    ws.send(tungstenite::Message::Text(
        jsonrpc_with_params("eth_sendRawTransactionSync", json!(["0x00"]), 1).into(),
    ))
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(1), blocking_rpc_started.notified())
        .await
        .expect("blocking RPC was not dispatched");

    api.revoke_key(root_account, key_id);

    let _closed = tokio::time::timeout(Duration::from_secs(3), ws.next())
        .await
        .expect("timed out waiting for keychain-revocation close");
}

#[tokio::test]
async fn ws_reject_unauthorized_keychain_token() {
    let root_account = Address::repeat_byte(0x44);
    let access_signer = P256SigningKey::random(&mut thread_rng());
    let ctx = TestContext::start(MockRpc::default()).await;
    let now = now_secs();
    let (fields, digest) = build_token_fields(ZONE_ID, CHAIN_ID, now, now + 600);
    let (signature, _key_id) = sign_keychain_signature(digest, &access_signer, root_account, 0x04)
        .expect("keychain signing should succeed");
    let token = build_token_with_signature(signature, &fields);

    let err = connect_with_token(&ctx.ws_url(), ctx.addr, &token)
        .await
        .expect_err("missing keychain authorization should fail");
    let tungstenite::Error::Http(response) = err else {
        panic!("expected HTTP error, got {err:?}");
    };
    assert_eq!(response.status(), 403);
}

#[tokio::test]
async fn ws_keychain_lookup_failure_returns_500() {
    let root_account = Address::repeat_byte(0x66);
    let access_signer = P256SigningKey::random(&mut thread_rng());
    let ctx = TestContext::start(MockRpc::with_key_lookup_error("key lookup failed")).await;
    let now = now_secs();
    let (fields, digest) = build_token_fields(ZONE_ID, CHAIN_ID, now, now + 600);
    let (signature, _key_id) = sign_keychain_signature(digest, &access_signer, root_account, 0x04)
        .expect("keychain signing should succeed");
    let token = build_token_with_signature(signature, &fields);

    let err = connect_with_token(&ctx.ws_url(), ctx.addr, &token)
        .await
        .expect_err("keychain lookup failure should fail");
    let tungstenite::Error::Http(response) = err else {
        panic!("expected HTTP error, got {err:?}");
    };
    assert_eq!(response.status(), 500);
}
