//! Authenticated JSON-RPC server built on jsonrpsee.

use std::{
    convert::Infallible,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use alloy_primitives::Address;
use futures::future::BoxFuture;
use jsonrpsee::server::{
    BatchRequestConfig, ConnectionGuard, ConnectionState, HttpBody, HttpResponse, IdProvider,
    Methods, ServerConfig, http as rpc_http, middleware::rpc::RpcServiceBuilder,
    serve_with_graceful_shutdown, stop_channel, ws,
};
use tempo_contracts::precompiles::account_keychain::IAccountKeychain::{
    KeyInfo, SignatureType as KeyInfoSignatureType,
};
use tempo_primitives::transaction::{
    SignatureType as TempoSignatureType,
    tt_signature::{KeychainSignature, TempoSignature},
};
use tower::service_fn;
use tracing::{info, warn};

use crate::{
    auth::{self, AuthContext, now_unix_seconds},
    config::RedactedRpcConfig,
    error::{AuthError, AuthenticateError},
    metrics::RedactedRpcAuthMetrics,
    middleware::RedactedRpcLayer,
};

/// Maximum number of requests in a single JSON-RPC batch.
const MAX_BATCH_SIZE: u32 = 100;
/// Maximum WebSocket request or response size (1 MiB).
pub(crate) const MAX_RPC_MESSAGE_SIZE: u32 = 1 << 20;
/// Maximum number of active subscriptions per WebSocket connection.
const MAX_WS_SUBSCRIPTIONS: u32 = 32;
/// Maximum number of queued outbound messages before jsonrpsee applies backpressure.
const MAX_WS_OUTBOUND_QUEUE: u32 = 1024;
/// Maximum concurrent HTTP requests and WebSocket connections.
const MAX_CONNECTIONS: usize = 100;

#[derive(Debug, Default)]
struct HexIdProvider(AtomicU32);

impl IdProvider for HexIdProvider {
    fn next_id(&self) -> jsonrpsee::types::SubscriptionId<'static> {
        format!("0x{:x}", self.0.fetch_add(1, Ordering::Relaxed) + 1).into()
    }
}

type KeychainLookup = Arc<
    dyn Fn(Address, Address) -> BoxFuture<'static, eyre::Result<KeyInfo>> + Send + Sync + 'static,
>;

/// Transport associated with a JSON-RPC request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RpcTransport {
    Http,
    WebSocket,
}

/// A node-owned method table and the one callback needed by transport authentication.
///
/// The RPC crate intentionally does not know the concrete node API type. The node
/// registers methods directly against its concrete implementation and hands the
/// resulting jsonrpsee method table to the server.
#[derive(Clone)]
pub struct RedactedRpcModule {
    methods: Methods,
    keychain_lookup: KeychainLookup,
}

impl RedactedRpcModule {
    /// Create an authenticated method table.
    pub fn new<M, F, Fut>(methods: M, keychain_lookup: F) -> Self
    where
        M: Into<Methods>,
        F: Fn(Address, Address) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = eyre::Result<KeyInfo>> + Send + 'static,
    {
        Self {
            methods: methods.into(),
            keychain_lookup: Arc::new(move |account, key_id| {
                Box::pin(keychain_lookup(account, key_id))
            }),
        }
    }
}

/// Start the redacted zone RPC server.
pub async fn start_redacted_rpc(
    config: RedactedRpcConfig,
    rpc: RedactedRpcModule,
) -> eyre::Result<std::net::SocketAddr> {
    let listener = tokio::net::TcpListener::bind(config.listen_addr).await?;
    let local_addr = listener.local_addr()?;
    let config = Arc::new(config);
    let auth_metrics = RedactedRpcAuthMetrics::default();
    let methods = rpc.methods;
    let keychain_lookup = rpc.keychain_lookup;
    let server_config = ServerConfig::builder()
        .max_request_body_size(MAX_RPC_MESSAGE_SIZE)
        .max_response_body_size(MAX_RPC_MESSAGE_SIZE)
        .max_connections(MAX_CONNECTIONS as u32)
        .max_subscriptions_per_connection(MAX_WS_SUBSCRIPTIONS)
        .set_batch_request_config(BatchRequestConfig::Limit(MAX_BATCH_SIZE))
        .set_message_buffer_capacity(MAX_WS_OUTBOUND_QUEUE)
        .set_id_provider(HexIdProvider::default())
        .build();
    let rpc_middleware = RpcServiceBuilder::new().layer(RedactedRpcLayer);
    let connection_guard = ConnectionGuard::new(MAX_CONNECTIONS);
    let next_connection_id = Arc::new(AtomicU32::new(0));
    let (stop_handle, server_handle) = stop_channel();

    info!(target: "zone::rpc", %local_addr, "Starting redacted zone RPC server");

    tokio::spawn(async move {
        // Preserve the detached-server behavior of the old API. Keeping the sender
        // alive prevents the stop channel from shutting down with no external handle.
        let _server_handle = server_handle;

        loop {
            let accepted = tokio::select! {
                accepted = listener.accept() => accepted,
                _ = stop_handle.clone().shutdown() => break,
            };
            let (socket, remote_addr) = match accepted {
                Ok(accepted) => accepted,
                Err(error) => {
                    warn!(target: "zone::rpc", %error, "failed to accept RPC connection");
                    continue;
                }
            };

            let config = config.clone();
            let auth_metrics = auth_metrics.clone();
            let methods = methods.clone();
            let keychain_lookup = keychain_lookup.clone();
            let server_config = server_config.clone();
            let rpc_middleware = rpc_middleware.clone();
            let connection_guard = connection_guard.clone();
            let next_connection_id = next_connection_id.clone();
            let connection_stop = stop_handle.clone();

            let service = service_fn(move |mut request| {
                let config = config.clone();
                let auth_metrics = auth_metrics.clone();
                let methods = methods.clone();
                let keychain_lookup = keychain_lookup.clone();
                let server_config = server_config.clone();
                let rpc_middleware = rpc_middleware.clone();
                let connection_guard = connection_guard.clone();
                let connection_stop = connection_stop.clone();
                let connection_id = next_connection_id.fetch_add(1, Ordering::Relaxed);

                async move {
                    let is_websocket = ws::is_upgrade_request(&request);
                    let token = request
                        .headers()
                        .get(auth::X_AUTHORIZATION_TOKEN)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned)
                        .or_else(|| is_websocket.then(|| query_token(request.uri())).flatten());

                    let authenticated = match token {
                        Some(token) => authenticate_token(&token, &config, &keychain_lookup).await,
                        None => Err(AuthError::Missing.into()),
                    };
                    let auth = match authenticated {
                        Ok(auth) => auth,
                        Err(error) => {
                            if error.is_invalid() {
                                auth_metrics.auth_failures_total.increment(1);
                            }
                            error.log(if is_websocket { "ws" } else { "http" });
                            return Ok::<_, Infallible>(auth_error_response(&error));
                        }
                    };

                    let Some(connection_permit) = connection_guard.try_acquire() else {
                        return Ok(rpc_http::response::too_many_requests());
                    };
                    let connection =
                        ConnectionState::new(connection_stop, connection_id, connection_permit);
                    request.extensions_mut().insert(auth.clone());
                    request.extensions_mut().insert(if is_websocket {
                        RpcTransport::WebSocket
                    } else {
                        RpcTransport::Http
                    });

                    if is_websocket {
                        match ws::connect(
                            request,
                            server_config,
                            methods,
                            connection,
                            rpc_middleware,
                        )
                        .await
                        {
                            Ok((response, connection_future)) => {
                                tokio::spawn(async move {
                                    tokio::select! {
                                        _ = connection_future => {}
                                        _ = authentication_lifetime(auth, keychain_lookup) => {}
                                    }
                                });
                                Ok(response)
                            }
                            Err(response) => Ok(response),
                        }
                    } else {
                        Ok(rpc_http::call_with_service_builder(
                            request,
                            server_config,
                            connection,
                            methods,
                            rpc_middleware,
                        )
                        .await)
                    }
                }
            });

            let stopped = stop_handle.clone().shutdown();
            tokio::spawn(async move {
                if let Err(error) = serve_with_graceful_shutdown(socket, service, stopped).await {
                    warn!(
                        target: "zone::rpc",
                        %remote_addr,
                        %error,
                        "RPC connection failed"
                    );
                }
            });
        }
    });

    Ok(local_addr)
}

fn auth_error_response(error: &AuthenticateError) -> HttpResponse {
    HttpResponse::builder()
        .status(error.status_code())
        .body(HttpBody::empty())
        .expect("known HTTP status produces a valid response")
}

fn query_token(uri: &http::Uri) -> Option<String> {
    url::form_urlencoded::parse(uri.query()?.as_bytes())
        .find_map(|(key, value)| (key == "token").then(|| value.into_owned()))
}

/// Authenticate using a raw token string.
async fn authenticate_token(
    token_value: &str,
    config: &RedactedRpcConfig,
    keychain_lookup: &KeychainLookup,
) -> Result<AuthContext, AuthenticateError> {
    let token = auth::parse_auth_header(token_value)?;
    let max_auth_token_validity = config
        .max_auth_token_validity
        .min(auth::DEFAULT_MAX_AUTH_TOKEN_VALIDITY);

    token.validate_with_max_auth_token_validity(
        config.zone_id,
        config.chain_id,
        max_auth_token_validity,
    )?;

    let signature =
        TempoSignature::from_bytes(&token.signature).map_err(|_| AuthError::InvalidSignature)?;
    let caller = signature
        .recover_signer(&token.digest)
        .map_err(|_| AuthError::InvalidSignature)?;

    let keychain_key_id = if let TempoSignature::Keychain(keychain_signature) = &signature {
        Some(
            validate_keychain_signature(keychain_lookup, caller, keychain_signature, &token.digest)
                .await?,
        )
    } else {
        None
    };

    Ok(AuthContext {
        caller,
        expires_at: token.expires_at,
        keychain_key_id,
    })
}

async fn validate_keychain_signature(
    keychain_lookup: &KeychainLookup,
    caller: Address,
    keychain_signature: &KeychainSignature,
    digest: &alloy_primitives::B256,
) -> Result<Address, AuthenticateError> {
    let key_id = keychain_signature
        .key_id(digest)
        .map_err(|_| AuthError::InvalidSignature)?;
    let key_info = keychain_lookup(caller, key_id).await?;
    validate_keychain_key_info(&key_info)?;

    let expected_signature_type = match keychain_signature.signature.signature_type() {
        TempoSignatureType::Secp256k1 => KeyInfoSignatureType::Secp256k1,
        TempoSignatureType::P256 => KeyInfoSignatureType::P256,
        TempoSignatureType::WebAuthn => KeyInfoSignatureType::WebAuthn,
    };
    if key_info.signatureType != expected_signature_type {
        return Err(AuthError::KeychainSignatureTypeMismatch.into());
    }

    Ok(key_id)
}

async fn authentication_lifetime(auth: AuthContext, keychain_lookup: KeychainLookup) {
    let token_expiry = tokio::time::sleep(duration_until_unix_timestamp(auth.expires_at));
    tokio::pin!(token_expiry);

    let Some(key_id) = auth.keychain_key_id else {
        token_expiry.await;
        return;
    };

    let mut keychain_recheck = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            biased;
            _ = &mut token_expiry => return,
            _ = keychain_recheck.tick() => {
                let valid = tokio::select! {
                    biased;
                    _ = &mut token_expiry => false,
                    key_info = keychain_lookup(auth.caller, key_id) => match key_info {
                        Ok(key_info) => validate_keychain_key_info(&key_info).is_ok(),
                        Err(error) => {
                            warn!(target: "zone::rpc", %error, "ws keychain revalidation failed");
                            false
                        }
                    }
                };
                if !valid {
                    return;
                }
            }
        }
    }
}

fn duration_until_unix_timestamp(timestamp: u64) -> Duration {
    let deadline = UNIX_EPOCH + Duration::from_secs(timestamp);
    deadline
        .duration_since(SystemTime::now())
        .unwrap_or_default()
}

fn validate_keychain_key_info(key_info: &KeyInfo) -> Result<(), AuthenticateError> {
    if key_info.isRevoked {
        return Err(AuthError::RevokedKeychainKey.into());
    }
    if key_info.keyId.is_zero() {
        return Err(AuthError::UnauthorizedKeychainKey.into());
    }
    if key_info.expiry <= now_unix_seconds() {
        return Err(AuthError::ExpiredKeychainKey.into());
    }
    Ok(())
}
