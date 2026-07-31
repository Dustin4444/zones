//! Private zone RPC test contexts and auth-token builders.

use super::*;

use alloy_primitives::{Address, B256, U256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_signer::SignerSync;
use p256::ecdsa::SigningKey as P256SigningKey;
use std::{ops::Deref, time::Duration};
use tempo_contracts::precompiles::{
    ACCOUNT_KEYCHAIN_ADDRESS,
    account_keychain::IAccountKeychain::{
        IAccountKeychainInstance, KeyRestrictions, SignatureType as KeyInfoSignatureType,
    },
};
use tempo_primitives::transaction::tt_signature::TempoSignature;

/// Validity window for test auth tokens.
const AUTH_TOKEN_TTL_SECS: u64 = 600;

/// Token fields and signing digest valid from now for [`AUTH_TOKEN_TTL_SECS`].
fn fresh_token_fields(zone_id: u32, chain_id: u64) -> ([u8; 29], B256) {
    let now = now_secs();
    zone_node::rpc::auth::build_token_fields(zone_id, chain_id, now, now + AUTH_TOKEN_TTL_SECS)
}

/// Build a hex-encoded authorization token for the private zone RPC.
///
/// Signs the token with the given signer and returns the hex string (no `0x` prefix)
/// suitable for the `X-Authorization-Token` header.
pub(crate) fn build_auth_token(
    signer: &alloy_signer_local::PrivateKeySigner,
    zone_id: u32,
    chain_id: u64,
) -> String {
    let (fields, digest) = fresh_token_fields(zone_id, chain_id);
    let sig = signer.sign_hash_sync(&digest).expect("signing failed");
    auth_tokens::build_secp256k1_token(&sig, &fields)
}

fn build_auth_token_with_signature(
    signature: TempoSignature,
    zone_id: u32,
    chain_id: u64,
) -> String {
    let (fields, _) = fresh_token_fields(zone_id, chain_id);
    auth_tokens::build_token_with_signature(signature, &fields)
}

fn build_p256_auth_token(signing_key: &P256SigningKey, zone_id: u32, chain_id: u64) -> String {
    let (_, digest) = fresh_token_fields(zone_id, chain_id);
    build_auth_token_with_signature(
        sign_p256_signature(digest, signing_key).expect("p256 signing failed"),
        zone_id,
        chain_id,
    )
}

fn build_webauthn_auth_token(
    signing_key: &P256SigningKey,
    zone_id: u32,
    chain_id: u64,
    challenge_digest: Option<B256>,
) -> String {
    let (_, digest) = fresh_token_fields(zone_id, chain_id);
    build_auth_token_with_signature(
        sign_webauthn_signature(signing_key, challenge_digest.unwrap_or(digest))
            .expect("webauthn signing failed"),
        zone_id,
        chain_id,
    )
}

fn build_keychain_auth_token(
    signing_key: &P256SigningKey,
    root_account: Address,
    version: u8,
    zone_id: u32,
    chain_id: u64,
) -> (String, Address) {
    let (_, digest) = fresh_token_fields(zone_id, chain_id);
    let (signature, key_id) = sign_keychain_signature(digest, signing_key, root_account, version)
        .expect("keychain signing failed");

    (
        build_auth_token_with_signature(signature, zone_id, chain_id),
        key_id,
    )
}

static HTTP_CLIENT: std::sync::LazyLock<reqwest::Client> =
    std::sync::LazyLock::new(reqwest::Client::new);

/// POST a JSON-RPC request to the private zone RPC, optionally authenticated,
/// returning the HTTP status + raw body.
///
/// The raw form is what auth-failure tests need (401/403 responses have no
/// JSON body).
async fn rpc_call_raw(
    url: &url::Url,
    method: &str,
    params: serde_json::Value,
    auth_token: Option<&str>,
) -> eyre::Result<(reqwest::StatusCode, String)> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
        "id": 1
    });

    let mut request = HTTP_CLIENT.post(url.as_str()).json(&body);
    if let Some(token) = auth_token {
        request = request.header("x-authorization-token", token);
    }
    let resp = request.send().await?;

    let status = resp.status();
    let text = resp.text().await?;
    Ok((status, text))
}

/// Send an authenticated JSON-RPC request and return the parsed response body
/// (including `jsonrpc`, `id`, `result`/`error`).
async fn private_rpc_call(
    url: &url::Url,
    method: &str,
    params: serde_json::Value,
    auth_token: &str,
) -> eyre::Result<serde_json::Value> {
    let (status, text) = rpc_call_raw(url, method, params, Some(auth_token)).await?;
    if !status.is_success() && text.is_empty() {
        eyre::bail!("HTTP {status}");
    }
    Ok(serde_json::from_str(&text)?)
}

/// Context for private RPC e2e tests.
///
/// Wraps a zone node with a running private RPC server in front, providing
/// helpers for authenticated and unauthenticated request testing.
pub(crate) struct PrivateRpcTestCtx {
    /// The underlying zone test node.
    pub zone: ZoneTestNode,
    /// URL of the private RPC server (not the zone's direct HTTP endpoint).
    pub private_rpc_url: url::Url,
    /// The sequencer signer (gets full access on the private RPC).
    pub sequencer_signer: alloy_signer_local::PrivateKeySigner,
    /// Private RPC server configuration.
    pub config: zone_node::rpc::PrivateRpcConfig,
    /// L1 fixture for injecting deposits.
    pub fixture: L1Fixture,
}

/// Private RPC e2e context backed by a real L1 node and deployed ZonePortal.
pub(crate) struct PrivateRpcL1TestCtx {
    ctx: PrivateRpcTestCtx,
    l1: L1TestNode,
    portal_address: Address,
}

impl Deref for PrivateRpcL1TestCtx {
    type Target = PrivateRpcTestCtx;

    fn deref(&self) -> &Self::Target {
        &self.ctx
    }
}

impl PrivateRpcL1TestCtx {
    /// Returns the real L1 node for tests that require one.
    pub(crate) fn l1(&self) -> &L1TestNode {
        &self.l1
    }

    /// Returns the real portal address for tests that require one.
    pub(crate) fn portal_address(&self) -> Address {
        self.portal_address
    }
}

impl PrivateRpcTestCtx {
    /// Build an auth token for the sequencer.
    fn sequencer_token(&self) -> String {
        build_auth_token(
            &self.sequencer_signer,
            self.config.zone_id,
            self.config.chain_id,
        )
    }

    /// Build an auth token for a regular (non-sequencer) user.
    pub(crate) fn user_token(&self, signer: &alloy_signer_local::PrivateKeySigner) -> String {
        build_auth_token(signer, self.config.zone_id, self.config.chain_id)
    }

    /// Build a P256 auth token for a non-sequencer caller.
    pub(crate) fn p256_token(&self, signing_key: &P256SigningKey) -> String {
        build_p256_auth_token(signing_key, self.config.zone_id, self.config.chain_id)
    }

    /// Build a WebAuthn auth token for a non-sequencer caller.
    pub(crate) fn webauthn_token(&self, signing_key: &P256SigningKey) -> String {
        build_webauthn_auth_token(signing_key, self.config.zone_id, self.config.chain_id, None)
    }

    /// Build a WebAuthn auth token with an overridden challenge digest.
    pub(crate) fn webauthn_token_with_challenge(
        &self,
        signing_key: &P256SigningKey,
        challenge_digest: B256,
    ) -> String {
        build_webauthn_auth_token(
            signing_key,
            self.config.zone_id,
            self.config.chain_id,
            Some(challenge_digest),
        )
    }

    /// Build a Keychain auth token signed by a P256 access key.
    pub(crate) fn keychain_p256_token(
        &self,
        root_account: Address,
        signing_key: &P256SigningKey,
        version: u8,
    ) -> (String, Address) {
        build_keychain_auth_token(
            signing_key,
            root_account,
            version,
            self.config.zone_id,
            self.config.chain_id,
        )
    }

    /// Send an authenticated JSON-RPC call to the private RPC server.
    pub(crate) async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
        auth_token: &str,
    ) -> eyre::Result<serde_json::Value> {
        private_rpc_call(&self.private_rpc_url, method, params, auth_token).await
    }

    /// Send a JSON-RPC call authenticated as the sequencer.
    pub(crate) async fn call_as_sequencer(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> eyre::Result<serde_json::Value> {
        let token = self.sequencer_token();
        self.call(method, params, &token).await
    }

    /// Send a JSON-RPC call authenticated as a regular user.
    pub(crate) async fn call_as_user(
        &self,
        method: &str,
        params: serde_json::Value,
        signer: &alloy_signer_local::PrivateKeySigner,
    ) -> eyre::Result<serde_json::Value> {
        let token = self.user_token(signer);
        self.call(method, params, &token).await
    }

    /// Send a JSON-RPC call with a raw auth token string, returning HTTP status + body.
    pub(crate) async fn call_raw(
        &self,
        method: &str,
        params: serde_json::Value,
        auth_token: &str,
    ) -> eyre::Result<(reqwest::StatusCode, String)> {
        rpc_call_raw(&self.private_rpc_url, method, params, Some(auth_token)).await
    }

    /// Send a JSON-RPC call with no auth header, returning HTTP status + body.
    pub(crate) async fn call_no_auth(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> eyre::Result<(reqwest::StatusCode, String)> {
        rpc_call_raw(&self.private_rpc_url, method, params, None).await
    }

    /// Inject an empty L1 block and wait for it to be processed.
    pub(crate) async fn inject_empty_block(&mut self) -> eyre::Result<()> {
        let dq = self.zone.deposit_queue().clone();
        self.fixture.inject_empty_block(&dq);
        self.zone
            .wait_for_tempo_block_number(1, DEFAULT_TIMEOUT)
            .await?;
        Ok(())
    }

    /// Inject a deposit and wait for the balance to appear.
    pub(crate) async fn inject_deposit(
        &mut self,
        token: Address,
        depositor: Address,
        recipient: Address,
        amount: u128,
    ) -> eyre::Result<()> {
        let deposit = self
            .fixture
            .make_deposit(token, depositor, recipient, amount);
        let dq = self.zone.deposit_queue().clone();
        self.fixture.inject_deposits(&dq, vec![deposit]);
        self.zone
            .wait_for_balance(token, recipient, U256::from(amount), DEFAULT_TIMEOUT)
            .await?;
        Ok(())
    }

    /// Query `eth_getBalance` via the private RPC as a specific user.
    pub(crate) async fn get_balance_as_user(
        &self,
        address: Address,
        signer: &alloy_signer_local::PrivateKeySigner,
    ) -> eyre::Result<serde_json::Value> {
        self.call_as_user(
            "eth_getBalance",
            serde_json::json!([format!("{address:#x}"), "latest"]),
            signer,
        )
        .await
    }

    /// Query `eth_getBalance` via the private RPC as the sequencer.
    pub(crate) async fn get_balance_as_sequencer(
        &self,
        address: Address,
    ) -> eyre::Result<serde_json::Value> {
        self.call_as_sequencer(
            "eth_getBalance",
            serde_json::json!([format!("{address:#x}"), "latest"]),
        )
        .await
    }

    /// Query `eth_getTransactionCount` via the private RPC as a specific user.
    pub(crate) async fn get_tx_count_as_user(
        &self,
        address: Address,
        signer: &alloy_signer_local::PrivateKeySigner,
    ) -> eyre::Result<serde_json::Value> {
        self.call_as_user(
            "eth_getTransactionCount",
            serde_json::json!([format!("{address:#x}"), "latest"]),
            signer,
        )
        .await
    }

    /// Authorize an access key for a root account on the zone keychain precompile.
    pub(crate) async fn authorize_keychain_key(
        &mut self,
        root_signer: &alloy_signer_local::PrivateKeySigner,
        key_id: Address,
        signature_type: KeyInfoSignatureType,
        expiry: u64,
    ) -> eyre::Result<()> {
        let provider = ProviderBuilder::new()
            .wallet(root_signer.clone())
            .connect_http(self.zone.http_url().clone());
        let keychain = IAccountKeychainInstance::new(ACCOUNT_KEYCHAIN_ADDRESS, &provider);
        let pending = keychain
            .authorizeKey_1(
                key_id,
                signature_type,
                KeyRestrictions {
                    expiry,
                    enforceLimits: false,
                    limits: vec![],
                    allowAnyCalls: true,
                    allowedCalls: vec![],
                },
            )
            .send()
            .await?;
        self.fixture.inject_empty_block(self.zone.deposit_queue());
        let receipt = pending.get_receipt().await?;
        eyre::ensure!(receipt.status(), "authorizeKey failed");
        Ok(())
    }

    /// Revoke an access key from a root account on the zone keychain precompile.
    pub(crate) async fn revoke_keychain_key(
        &mut self,
        root_signer: &alloy_signer_local::PrivateKeySigner,
        key_id: Address,
    ) -> eyre::Result<()> {
        let provider = ProviderBuilder::new()
            .wallet(root_signer.clone())
            .connect_http(self.zone.http_url().clone());
        let keychain = IAccountKeychainInstance::new(ACCOUNT_KEYCHAIN_ADDRESS, &provider);
        let pending = keychain.revokeKey(key_id).send().await?;
        self.fixture.inject_empty_block(self.zone.deposit_queue());
        let receipt = pending.get_receipt().await?;
        eyre::ensure!(receipt.status(), "revokeKey failed");
        Ok(())
    }
}

async fn zone_chain_id(zone: &ZoneTestNode) -> eyre::Result<u64> {
    let chain_id: alloy_primitives::U64 = zone
        .provider()
        .raw_request("eth_chainId".into(), ())
        .await?;
    Ok(chain_id.to())
}

async fn start_private_rpc_url(
    zone: &ZoneTestNode,
    config: zone_node::rpc::PrivateRpcConfig,
) -> eyre::Result<url::Url> {
    let local_addr =
        zone_node::rpc::start_private_rpc(config.clone(), zone.rpc_api(config).await?).await?;
    Ok(format!("http://{local_addr}").parse()?)
}

/// Start a zone node with a private RPC server for testing.
///
/// Returns a context with:
/// - A running zone node with L1 state cache seeded
/// - A private RPC server on a random port
/// - Sequencer credentials for testing access control
pub(crate) async fn start_zone_with_private_rpc() -> eyre::Result<PrivateRpcTestCtx> {
    let sequencer_signer = alloy_signer_local::PrivateKeySigner::random();
    let sequencer_address = sequencer_signer.address();

    let zone = ZoneTestNode::start_local().await?;
    let fixture = L1Fixture::new();

    fixture.seed_l1_cache(
        zone.l1_state_cache(),
        zone.enabled_tokens(),
        Address::ZERO,
        sequencer_address,
        20,
    );

    let chain_id = zone_chain_id(&zone).await?;

    let config = zone_node::rpc::PrivateRpcConfig {
        listen_addr: ([127, 0, 0, 1], 0).into(),
        l1_rpc_url: DUMMY_L1_URL.to_string(),
        zone_rpc_url: zone.http_url().to_string(),
        retry_connection_interval: Duration::from_millis(100),
        zone_id: 0,
        chain_id,
        max_auth_token_validity: zone_node::rpc::auth::DEFAULT_MAX_AUTH_TOKEN_VALIDITY,
        zone_portal: Address::ZERO,
    };

    let private_rpc_url = start_private_rpc_url(&zone, config.clone()).await?;

    Ok(PrivateRpcTestCtx {
        zone,
        private_rpc_url,
        sequencer_signer,
        config,
        fixture,
    })
}

/// Start a zone with a private RPC server backed by a real L1 and a portal
/// with a registered sequencer encryption key.
pub(crate) async fn start_zone_with_private_rpc_l1() -> eyre::Result<PrivateRpcL1TestCtx> {
    let (l1, zone, portal_address) = start_l1_and_zone().await?;

    let key = k256::SecretKey::from(l1.dev_signer().credential());
    l1.set_sequencer_encryption_key(portal_address, &key)
        .await?;

    let chain_id = zone_chain_id(&zone).await?;

    let config = zone_node::rpc::PrivateRpcConfig {
        listen_addr: ([127, 0, 0, 1], 0).into(),
        l1_rpc_url: l1.http_url().to_string(),
        zone_rpc_url: zone.http_url().to_string(),
        retry_connection_interval: Duration::from_millis(100),
        zone_id: 1,
        chain_id,
        max_auth_token_validity: zone_node::rpc::auth::DEFAULT_MAX_AUTH_TOKEN_VALIDITY,
        zone_portal: portal_address,
    };

    let private_rpc_url = start_private_rpc_url(&zone, config.clone()).await?;
    let sequencer_signer = l1.dev_signer();

    Ok(PrivateRpcL1TestCtx {
        ctx: PrivateRpcTestCtx {
            zone,
            private_rpc_url,
            sequencer_signer,
            config,
            fixture: L1Fixture::new(),
        },
        l1,
        portal_address,
    })
}
