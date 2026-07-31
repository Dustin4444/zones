//! Zone node harness ([`ZoneTestNode`]) and the mock L1 JSON-RPC server.

use super::*;

use alloy::genesis::Genesis;
use alloy_primitives::{Address, U256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types_eth::Filter;
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use k256::SecretKey;
use reth_node_api::FullNodeComponents;
use reth_node_builder::{NodeBuilder, NodeConfig, NodeHandle, rpc::RethRpcAddOns};
use reth_node_core::{args::RpcServerArgs, exit::NodeExitFuture};
use reth_provider::{BlockNumReader, CanonStateSubscriptions, ChainSpecProvider, HeaderProvider};
use reth_rpc_builder::RpcModuleSelection;
use reth_tasks::Runtime;
use std::{future::Future, num::NonZeroU32, pin::Pin, sync::Arc, time::Duration};
use tempo_alloy::rpc::TempoHeaderResponse;
use tempo_contracts::precompiles::ITIP20;
use tempo_precompiles::{self, PATH_USD_ADDRESS, tip403_registry::ALLOW_ALL_POLICY_ID};
use tempo_primitives::TempoHeader;
use tempo_zone_contracts::{
    IZoneOutbox, TEMPO_STATE_ADDRESS, TempoState, ZONE_CONFIG_ADDRESS, ZONE_OUTBOX_ADDRESS,
    ZoneConfig,
    ZonePortal::{self},
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_util::sync::CancellationToken;
use zone_chainspec::ZoneChainSpec;
use zone_l1::{DepositQueue, L1BlockTracker, L1StateCache, state::EnabledTokenRegistry};
use zone_node::{ZoneNode, ZoneSequencerAddOnsConfig};
use zone_p2p::{LeadershipSchedule, P2pConfig};

/// Dummy L1 URL used when no real L1 is needed.
///
/// The launch helper recognizes this sentinel and replaces it with a local RPC
/// server that exposes the enabled-token snapshot required during node startup.
pub(crate) const DUMMY_L1_URL: &str = "http://127.0.0.1:1";

pub(crate) async fn spawn_test_l1_rpc(chain_id: u64) -> eyre::Result<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let enabled_tokens = Arc::new(vec![PATH_USD_ADDRESS]);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let enabled_tokens = enabled_tokens.clone();
            tokio::spawn(handle_test_l1_rpc_request(stream, enabled_tokens, chain_id));
        }
    });
    Ok(format!("http://{address}"))
}

async fn handle_test_l1_rpc_request(
    mut stream: tokio::net::TcpStream,
    enabled_tokens: Arc<Vec<Address>>,
    chain_id: u64,
) {
    let mut request = Vec::new();
    let mut buf = [0u8; 1024];
    let mut headers_end = None;
    let mut content_length = 0usize;

    loop {
        let Ok(read) = stream.read(&mut buf).await else {
            return;
        };
        if read == 0 {
            return;
        }
        request.extend_from_slice(&buf[..read]);

        if headers_end.is_none()
            && let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n")
        {
            headers_end = Some(end + 4);
            let headers = String::from_utf8_lossy(&request[..end]);
            content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().ok())
                        .flatten()
                })
                .unwrap_or(0);
        }

        if let Some(end) = headers_end
            && request.len() >= end + content_length
        {
            break;
        }
    }

    let request = headers_end
        .and_then(|end| request.get(end..end + content_length))
        .and_then(|body| serde_json::from_slice::<serde_json::Value>(body).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let id = request
        .get("id")
        .cloned()
        .unwrap_or_else(|| serde_json::json!(1));
    let method = request
        .get("method")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let result = match method {
        "eth_chainId" => serde_json::json!(format!("0x{chain_id:x}")),
        "eth_blockNumber" => serde_json::json!("0x0"),
        "eth_getCode" => serde_json::json!("0x01"),
        "eth_newBlockFilter" => serde_json::json!("0x1"),
        "eth_getFilterChanges" => serde_json::json!([]),
        "eth_uninstallFilter" => serde_json::json!(true),
        "eth_getHeaderByNumber" => serde_json::to_value(TempoHeaderResponse {
            inner: alloy_rpc_types_eth::Header::new(TempoHeader::default()),
            timestamp_millis: 0,
        })
        .expect("test L1 header should serialize"),
        "eth_call" => {
            let input = request
                .pointer("/params/0/input")
                .or_else(|| request.pointer("/params/0/data"))
                .and_then(serde_json::Value::as_str)
                .and_then(|input| const_hex::decode(input.trim_start_matches("0x")).ok())
                .unwrap_or_default();

            if input.starts_with(&ZonePortal::enabledTokenCountCall::SELECTOR) {
                serde_json::json!(const_hex::encode_prefixed(
                    U256::from(enabled_tokens.len()).abi_encode()
                ))
            } else if input.starts_with(&ZonePortal::enabledTokenAtCall::SELECTOR) {
                let index = input
                    .get(4..36)
                    .map(U256::from_be_slice)
                    .map(|index| index.to::<u64>() as usize);
                index
                    .and_then(|index| enabled_tokens.get(index))
                    .map(|token| serde_json::json!(const_hex::encode_prefixed(token.abi_encode())))
                    .unwrap_or(serde_json::Value::Null)
            } else {
                serde_json::Value::Null
            }
        }
        _ => serde_json::Value::Null,
    };
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

pub(crate) trait TestNodeHandle: Send {
    fn subscribe_to_canonical_state(
        &self,
    ) -> reth_provider::CanonStateNotifications<tempo_primitives::TempoPrimitives>;

    fn node_exit_future_mut(&mut self) -> &mut NodeExitFuture;

    fn spawn_sequencer(
        &self,
        config: zone_sequencer::ZoneSequencerConfig,
        signer: alloy_signer_local::PrivateKeySigner,
    ) -> Pin<Box<dyn Future<Output = zone_sequencer::ZoneSequencerHandle> + Send + '_>>;
}

impl<Node, AddOns> TestNodeHandle for NodeHandle<Node, AddOns>
where
    Node: FullNodeComponents<
        Types: reth_node_api::NodeTypes<Primitives = tempo_primitives::TempoPrimitives>,
    >,
    AddOns: RethRpcAddOns<Node>,
{
    fn subscribe_to_canonical_state(
        &self,
    ) -> reth_provider::CanonStateNotifications<tempo_primitives::TempoPrimitives> {
        self.node.provider().subscribe_to_canonical_state()
    }

    fn node_exit_future_mut(&mut self) -> &mut NodeExitFuture {
        &mut self.node_exit_future
    }

    fn spawn_sequencer(
        &self,
        config: zone_sequencer::ZoneSequencerConfig,
        signer: alloy_signer_local::PrivateKeySigner,
    ) -> Pin<Box<dyn Future<Output = zone_sequencer::ZoneSequencerHandle> + Send + '_>> {
        let provider = self.node.provider().clone();
        Box::pin(async move {
            zone_sequencer::spawn_zone_sequencer(
                config,
                signer,
                provider,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
        })
    }
}

type RpcApiFuture =
    Pin<Box<dyn Future<Output = eyre::Result<Arc<dyn zone_node::rpc::ZoneRpcApi>>>>>;
type RpcApiFactory = dyn Fn(zone_node::rpc::PrivateRpcConfig) -> RpcApiFuture + Send + Sync;

/// A self-contained Tempo Zone L2 node for integration testing.
///
/// Wraps an in-process reth node configured as a Zone, providing:
/// - An HTTP RPC endpoint for provider connections
/// - A [`DepositQueue`] handle for injecting synthetic L1 blocks
/// - A [`L1StateCache`] for seeding TempoState storage-read data
///
/// # Construction
///
/// Use one of the static constructors depending on your test scenario:
///
/// - [`start_local()`](Self::start_local) — standalone node, no real L1, fastest for unit-style e2e
/// - [`start_local_with_chain_id()`](Self::start_local_with_chain_id) — standalone with custom chain ID (multi-zone tests)
/// - [`start_from_l1()`](Self::start_from_l1) — connected to a real [`L1TestNode`], genesis patched from L1 header
/// - [`start()`](Self::start) — connected to an external L1 via WebSocket URL
pub(crate) struct ZoneTestNode {
    http_url: url::Url,
    deposit_queue: DepositQueue,
    enabled_tokens: EnabledTokenRegistry,
    l1_state_cache: L1StateCache,
    l1_block_tracker: L1BlockTracker,
    rpc_api_factory: Arc<RpcApiFactory>,
    node_handle: Box<dyn TestNodeHandle>,
    /// Cancels the `ZoneEngine`, when this node runs one.
    ///
    /// Exercises the graceful-stop path used by the leadership role controller.
    engine_stop: Option<CancellationToken>,
    /// The shared leadership schedule
    leadership: Option<LeadershipSchedule>,
    _tasks: Runtime,
}

/// Parameters for [`ZoneTestNode::launch`].
///
/// `..Default::default()` gives a self-contained node on the dummy L1 with a
/// fresh unique chain ID, a throwaway sequencer key, and the standard
/// withdrawal batch interval.
pub(crate) struct ZoneNodeParams {
    pub(crate) l1_ws_url: String,
    pub(crate) portal_address: Address,
    pub(crate) chain_id: u64,
    pub(crate) genesis: Option<Genesis>,
    /// `None` uses a throwaway key — fine unless the test exercises encrypted
    /// deposits or asserts on the block beneficiary.
    pub(crate) sequencer_signer: Option<alloy_signer_local::PrivateKeySigner>,
    pub(crate) withdrawal_batch_interval_blocks: u64,
    pub(crate) p2p_config: Option<P2pConfig>,
    pub(crate) spawn_engine: bool,
}

impl Default for ZoneNodeParams {
    fn default() -> Self {
        Self {
            l1_ws_url: DUMMY_L1_URL.to_string(),
            portal_address: Address::ZERO,
            chain_id: next_unique_chain_id(),
            genesis: None,
            sequencer_signer: None,
            withdrawal_batch_interval_blocks: WITHDRAWAL_BATCH_INTERVAL_BLOCKS,
            p2p_config: None,
            spawn_engine: true,
        }
    }
}

impl ZoneTestNode {
    /// Returns the HTTP RPC URL for connecting providers to this node.
    pub(crate) fn http_url(&self) -> &url::Url {
        &self.http_url
    }

    /// Stop this node's task runtime while retaining its storage handles for the test lifetime.
    pub(crate) fn crash(&self) {
        let _ = self
            ._tasks
            .graceful_shutdown_with_timeout(Duration::from_secs(5));
    }

    pub(crate) async fn spawn_sequencer(
        &self,
        config: zone_sequencer::ZoneSequencerConfig,
        signer: alloy_signer_local::PrivateKeySigner,
    ) -> zone_sequencer::ZoneSequencerHandle {
        self.node_handle.spawn_sequencer(config, signer).await
    }

    /// Stops the `ZoneEngine` at a block boundary and waits until block production has
    /// actually ceased.
    ///
    /// Returns the head the engine stopped at.
    pub(crate) async fn stop_engine(&self) -> eyre::Result<u64> {
        let stop = self
            .engine_stop
            .as_ref()
            .ok_or_else(|| eyre::eyre!("this test node does not run a ZoneEngine"))?;
        stop.cancel();

        // The engine finishes the block in flight before returning, so poll until the head
        // holds still rather than assuming it stops instantly.
        let provider = self.provider();
        let mut previous = provider.get_block_number().await?;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let current = provider.get_block_number().await?;
            if current == previous {
                return Ok(current);
            }
            previous = current;
        }
        eyre::bail!("ZoneEngine kept producing blocks after cancellation")
    }

    /// Wait until the ZoneOutbox's `withdrawalBatchIndex` equals `expected`.
    #[allow(dead_code)] // adopted by the e2e suites in a follow-up
    pub(crate) async fn wait_for_withdrawal_batch_index(
        &self,
        expected: u64,
        timeout: Duration,
    ) -> eyre::Result<()> {
        let outbox = IZoneOutbox::new(ZONE_OUTBOX_ADDRESS, self.provider());
        let description = format!("ZoneOutbox withdrawalBatchIndex == {expected}");
        poll_until(timeout, DEFAULT_POLL, &description, || {
            let outbox = &outbox;
            async move {
                let index = outbox.lastBatch().call().await?.withdrawalBatchIndex;
                if index == expected {
                    Ok(Some(()))
                } else {
                    Ok(None)
                }
            }
        })
        .await?;
        Ok(())
    }

    /// Fetch the transaction that emitted `log` and decode its calldata as a
    /// `finalizeWithdrawalBatch` call.
    #[allow(dead_code)] // adopted by the e2e suites in a follow-up
    pub(crate) async fn decode_finalize_batch_call(
        &self,
        log: &alloy_rpc_types_eth::Log,
    ) -> eyre::Result<IZoneOutbox::finalizeWithdrawalBatchCall> {
        let tx_hash = log
            .transaction_hash
            .ok_or_else(|| eyre::eyre!("BatchFinalized log missing transaction hash"))?;
        let finalize_tx = self
            .provider()
            .get_transaction_by_hash(tx_hash)
            .await?
            .ok_or_else(|| eyre::eyre!("finalizeWithdrawalBatch tx {tx_hash} not found"))?;
        Ok(IZoneOutbox::finalizeWithdrawalBatchCall::abi_decode(
            alloy_consensus::Transaction::input(&finalize_tx).as_ref(),
        )?)
    }

    /// Returns an HTTP provider connected to this zone node.
    pub(crate) fn provider(&self) -> alloy_provider::DynProvider {
        ProviderBuilder::new()
            .connect_http(self.http_url.clone())
            .erased()
    }

    /// Assert the gateway view exposed by the L2 ZoneConfig predeploy.
    pub(crate) async fn assert_zone_gateway(
        &self,
        gateway: Address,
        expected: bool,
    ) -> eyre::Result<()> {
        let config = ZoneConfig::new(ZONE_CONFIG_ADDRESS, self.provider());
        eyre::ensure!(
            config.isZoneGateway(gateway).call().await? == expected,
            "ZoneConfig gateway state for {gateway} did not equal {expected}"
        );
        Ok(())
    }

    /// Assert whether account enforcement is enabled in the L2 ZoneConfig predeploy.
    pub(crate) async fn assert_access_enforced(&self, expected: bool) -> eyre::Result<()> {
        let config = ZoneConfig::new(ZONE_CONFIG_ADDRESS, self.provider());
        let actual = config.isAccessEnforced().call().await?;
        eyre::ensure!(
            actual == expected,
            "ZoneConfig access enforcement {actual} did not equal {expected}"
        );
        Ok(())
    }

    /// Assert whether gateway registration is open in the L2 ZoneConfig predeploy.
    pub(crate) async fn assert_gateway_open(&self, expected: bool) -> eyre::Result<()> {
        let config = ZoneConfig::new(ZONE_CONFIG_ADDRESS, self.provider());
        let actual = config.isGatewayOpen().call().await?;
        eyre::ensure!(
            actual == expected,
            "ZoneConfig gateway openness {actual} did not equal {expected}"
        );
        Ok(())
    }

    /// Assert the mode-aware account authorization view exposed by L2 ZoneConfig.
    pub(crate) async fn assert_allowed_account(
        &self,
        account: Address,
        expected: bool,
    ) -> eyre::Result<()> {
        let config = ZoneConfig::new(ZONE_CONFIG_ADDRESS, self.provider());
        eyre::ensure!(
            config.isAllowedAccount(account).call().await? == expected,
            "ZoneConfig account state for {account} did not equal {expected}"
        );
        Ok(())
    }

    /// Returns a handle to the deposit queue for injecting synthetic L1 blocks.
    pub(crate) fn deposit_queue(&self) -> &DepositQueue {
        &self.deposit_queue
    }

    /// Returns the enabled-token registry used by pool admission.
    pub(crate) fn enabled_tokens(&self) -> &EnabledTokenRegistry {
        &self.enabled_tokens
    }

    /// Returns a handle to the L1 state cache for seeding precompile data.
    pub(crate) fn l1_state_cache(&self) -> &L1StateCache {
        &self.l1_state_cache
    }

    /// Returns the L1 anchors observed by this node.
    pub(crate) fn l1_block_tracker(&self) -> &L1BlockTracker {
        &self.l1_block_tracker
    }

    /// Returns this node's leadership schedule (multi-sequencer nodes only).
    pub(crate) fn leadership(&self) -> &LeadershipSchedule {
        self.leadership
            .as_ref()
            .expect("this test node was not started in multi-sequencer mode")
    }

    /// Builds the real private RPC API backed by the node's EthHandlers.
    pub(crate) async fn rpc_api(
        &self,
        config: zone_node::rpc::PrivateRpcConfig,
    ) -> eyre::Result<Arc<dyn zone_node::rpc::ZoneRpcApi>> {
        (self.rpc_api_factory)(config).await
    }

    /// Subscribe to canonical state notifications.
    pub(crate) fn subscribe_to_canonical_state(
        &self,
    ) -> reth_provider::CanonStateNotifications<tempo_primitives::TempoPrimitives> {
        self.node_handle.subscribe_to_canonical_state()
    }

    pub(crate) async fn wait_for_node_exit(&mut self) -> eyre::Result<()> {
        self.node_handle.node_exit_future_mut().await
    }

    /// Wait for a TIP-20 token balance to reach at least `min_balance` on this zone.
    ///
    /// Polls the token's `balanceOf` until `balance >= min_balance`, then
    /// returns the observed balance. Useful for verifying deposit mints.
    ///
    /// **Important:** passing `U256::ZERO` returns immediately (any balance satisfies `>= 0`).
    /// Use the expected post-deposit balance as `min_balance` to actually wait.
    pub(crate) async fn wait_for_balance(
        &self,
        token: Address,
        account: Address,
        min_balance: U256,
        timeout: Duration,
    ) -> eyre::Result<U256> {
        let tip20 = ITIP20::new(token, self.provider());
        poll_until(timeout, DEFAULT_POLL, "token balance", || {
            let tip20 = &tip20;
            async move {
                // balanceOf may revert with Uninitialized() if the token hasn't
                // been created yet (e.g. waiting for a TokenEnabled event to be
                // processed). Treat reverts as "not ready" rather than fatal.
                let balance = match tip20.balanceOf(account).from(account).call().await {
                    Ok(b) => b,
                    Err(_) => return Ok(None),
                };
                if balance >= min_balance {
                    Ok(Some(balance))
                } else {
                    Ok(None)
                }
            }
        })
        .await
    }

    /// Reads `tempoBlockNumber` from the L2 `TempoState` predeploy right now.
    pub(crate) async fn tempo_block_number(&self) -> eyre::Result<u64> {
        Ok(TempoState::new(TEMPO_STATE_ADDRESS, self.provider())
            .tempoBlockNumber()
            .call()
            .await?)
    }

    /// Wait for `tempoBlockNumber` on this zone to reach at least `target`.
    ///
    /// Returns the observed block number once it reaches the target.
    pub(crate) async fn wait_for_tempo_block_number(
        &self,
        target: u64,
        timeout: Duration,
    ) -> eyre::Result<u64> {
        let tempo_state = TempoState::new(TEMPO_STATE_ADDRESS, self.provider());
        poll_until(
            timeout,
            DEFAULT_POLL,
            &format!("tempoBlockNumber >= {target}"),
            || {
                let tempo_state = &tempo_state;
                async move {
                    // During a pre-creation replay the zone can advance before the initial
                    // TokenEnabled event has initialized the default fee token on L2. Treat the
                    // resulting transient eth_call failure as "not ready" and keep polling.
                    let n = match tempo_state.tempoBlockNumber().call().await {
                        Ok(n) => n,
                        Err(err) if err.to_string().contains("InvalidToken") => return Ok(None),
                        Err(err) => return Err(err.into()),
                    };
                    if n >= target { Ok(Some(n)) } else { Ok(None) }
                }
            },
        )
        .await
    }

    /// Wait for the zone L2 RPC head to reach at least `target`.
    ///
    /// This polls `eth_blockNumber`, which is useful when a test needs to assert
    /// that a follower imported leader-produced zone blocks over P2P.
    pub(crate) async fn wait_for_block_number(
        &self,
        target: u64,
        timeout: Duration,
    ) -> eyre::Result<u64> {
        let provider = self.provider();
        poll_until(
            timeout,
            DEFAULT_POLL,
            &format!("eth_blockNumber >= {target}"),
            || {
                let provider = &provider;
                async move {
                    let n = provider.get_block_number().await?;
                    if n >= target { Ok(Some(n)) } else { Ok(None) }
                }
            },
        )
        .await
    }

    /// Read a TIP-20 token balance on this zone (single-shot, no polling).
    pub(crate) async fn balance_of(&self, token: Address, account: Address) -> eyre::Result<U256> {
        Ok(ITIP20::new(token, self.provider())
            .balanceOf(account)
            .from(account)
            .call()
            .await?)
    }

    /// Wait for the zone L2 to finalize an L1 block beyond `after_block`.
    ///
    /// Polls for [`TempoState::TempoBlockFinalized`] logs on the zone L2 until
    /// one appears with a `blockNumber > after_block`, then confirms the on-chain
    /// `tempoBlockNumber` matches. Returns the finalized block number.
    ///
    /// Use this instead of manually polling `tempoBlockNumber()` — it's both
    /// event-driven (checks logs each iteration) and verifies consistency.
    pub(crate) async fn wait_for_l2_tempo_finalized(
        &self,
        after_block: u64,
        timeout: Duration,
    ) -> eyre::Result<u64> {
        let provider = self.provider();
        let tempo_state = TempoState::new(TEMPO_STATE_ADDRESS, &provider);

        let filter = Filter::new()
            .address(TEMPO_STATE_ADDRESS)
            .event_signature(TempoState::TempoBlockFinalized::SIGNATURE_HASH);

        poll_until(
            timeout,
            DEFAULT_POLL,
            "TempoBlockFinalized past target",
            || {
                let provider = &provider;
                let tempo_state = &tempo_state;
                let filter = &filter;
                async move {
                    // Check logs first — fast path when events already emitted
                    let logs = provider.get_logs(filter).await?;
                    for log in logs.iter().rev() {
                        if let Ok(ev) = TempoState::TempoBlockFinalized::decode_log(&log.inner)
                            && ev.blockNumber > after_block
                        {
                            // Confirm on-chain state matches
                            let on_chain = match tempo_state.tempoBlockNumber().call().await {
                                Ok(n) => n,
                                Err(err) if err.to_string().contains("InvalidToken") => {
                                    return Ok(None);
                                }
                                Err(err) => return Err(err.into()),
                            };
                            if on_chain >= ev.blockNumber {
                                return Ok(Some(on_chain));
                            }
                        }
                    }
                    Ok(None)
                }
            },
        )
        .await
    }

    /// Start a zone node pointing at a real L1 WebSocket URL.
    pub(crate) async fn start(l1_ws_url: String, portal_address: Address) -> eyre::Result<Self> {
        Self::launch(ZoneNodeParams {
            l1_ws_url,
            portal_address,
            ..Default::default()
        })
        .await
    }

    /// Start a zone node connected to a real L1, generating genesis from the L1's
    /// current block header.
    ///
    /// See [`build_l1_anchored_genesis`] for details on how the genesis is patched.
    pub(crate) async fn start_from_l1(
        l1_http_url: &url::Url,
        l1_ws_url: &url::Url,
        portal_address: Address,
    ) -> eyre::Result<Self> {
        let (genesis, _) = build_l1_anchored_genesis(l1_http_url, portal_address, None).await?;
        Self::launch(ZoneNodeParams {
            l1_ws_url: l1_ws_url.to_string(),
            portal_address,
            genesis: Some(genesis),
            sequencer_signer: Some(l1_dev_signer()),
            ..Default::default()
        })
        .await
    }

    pub(crate) async fn start_from_l1_with_withdrawal_batch_interval(
        l1_http_url: &url::Url,
        l1_ws_url: &url::Url,
        portal_address: Address,
        withdrawal_batch_interval_blocks: u64,
    ) -> eyre::Result<Self> {
        let (genesis, _) = build_l1_anchored_genesis(l1_http_url, portal_address, None).await?;
        Self::launch(ZoneNodeParams {
            l1_ws_url: l1_ws_url.to_string(),
            portal_address,
            genesis: Some(genesis),
            sequencer_signer: Some(l1_dev_signer()),
            withdrawal_batch_interval_blocks,
            ..Default::default()
        })
        .await
    }

    /// Start a zone node connected to a real L1 at an explicit genesis block.
    ///
    /// Unlike [`start_from_l1`], this preserves the full replay gap between the
    /// portal genesis and the current L1 tip, which is useful for long-downtime
    /// catch-up tests.
    pub(crate) async fn start_from_l1_genesis_block(
        l1_http_url: &url::Url,
        l1_ws_url: &url::Url,
        portal_address: Address,
        genesis_block_number: u64,
    ) -> eyre::Result<Self> {
        let (genesis, _) =
            build_l1_anchored_genesis(l1_http_url, portal_address, Some(genesis_block_number))
                .await?;
        Self::launch(ZoneNodeParams {
            l1_ws_url: l1_ws_url.to_string(),
            portal_address,
            genesis: Some(genesis),
            sequencer_signer: Some(l1_dev_signer()),
            ..Default::default()
        })
        .await
    }

    /// Start a self-contained zone node with no real L1 connection.
    ///
    /// The L1Subscriber retries a dummy URL in the background, but the
    /// ZoneEngine is fully functional. Deposits and L1 headers are injected
    /// directly into the `deposit_queue`; the L1 state cache must be seeded
    /// via [`L1Fixture::seed_l1_cache`] for TempoState storage reads.
    pub(crate) async fn start_local() -> eyre::Result<Self> {
        Self::launch(ZoneNodeParams::default()).await
    }

    /// Start a self-contained zone node with a custom chain ID.
    ///
    /// Useful for running multiple zone nodes in a single test — each needs
    /// a unique chain ID to avoid datadir collisions.
    pub(crate) async fn start_local_with_chain_id(chain_id: u64) -> eyre::Result<Self> {
        Self::launch(ZoneNodeParams {
            chain_id,
            ..Default::default()
        })
        .await
    }

    pub(crate) async fn start_local_with_p2p(
        l1_rpc_url: String,
        p2p_config: P2pConfig,
    ) -> eyre::Result<Self> {
        Self::launch(ZoneNodeParams {
            l1_ws_url: l1_rpc_url,
            p2p_config: Some(p2p_config),
            ..Default::default()
        })
        .await
    }

    pub(crate) async fn launch(params: ZoneNodeParams) -> eyre::Result<Self> {
        let ZoneNodeParams {
            l1_ws_url,
            portal_address,
            chain_id,
            genesis: custom_genesis,
            sequencer_signer,
            withdrawal_batch_interval_blocks,
            p2p_config,
            spawn_engine,
        } = params;
        let sequencer_signer = sequencer_signer.unwrap_or_else(|| {
            // Throwaway signer for tests that don't use encrypted deposits.
            let throwaway_key =
                k256::SecretKey::from_slice(&[0x01; 32]).expect("valid throwaway key");
            alloy_signer_local::PrivateKeySigner::from_signing_key(throwaway_key.into())
        });
        let tasks = Runtime::test();
        let is_local_dummy_l1 = l1_ws_url == DUMMY_L1_URL;
        let l1_ws_url = if is_local_dummy_l1 {
            spawn_test_l1_rpc(1337).await?
        } else {
            l1_ws_url
        };

        let mut genesis = custom_genesis.unwrap_or_else(|| {
            serde_json::from_str(zone_node::genesis::GENESIS_TEMPLATE_JSON)
                .expect("valid zone genesis template")
        });
        genesis.config.chain_id = chain_id;
        let chain_spec = ZoneChainSpec::from_genesis(genesis);

        let mut zone_node = ZoneNode::new(
            l1_ws_url,
            portal_address,
            4,
            std::time::Duration::from_millis(100),
        )
        .with_withdrawal_batch_interval_blocks(withdrawal_batch_interval_blocks);
        if is_local_dummy_l1 {
            zone_node = zone_node
                .with_l1_chain_id(1337)
                .with_l1_state_provider_retry_limits(0, NonZeroU32::MIN);
        }
        let p2p_enabled = p2p_config.is_some();
        if p2p_enabled && !is_local_dummy_l1 {
            // Multi-sequencer harness nodes run against a synthetic L1 RPC that cannot serve
            // storage reads: every read must come from the seeded cache. Bounded retries turn
            // a missed seed into a fast, visible failure instead of a silent retry spin.
            zone_node = zone_node.with_l1_state_provider_retry_limits(0, NonZeroU32::MIN);
        }
        let mut leadership = None;
        if let Some(p2p_config) = p2p_config {
            // The finalized L1 subscriber never observes a portal in this harness, so seed
            // the manifest's initial record unless the test pre-published a schedule.
            let schedule = p2p_config.leadership();
            if !schedule.is_initialized() {
                schedule.publish(p2p_config.manifest().bootstrap_leadership())?;
            }
            leadership = Some(schedule);
            // Every multi-sequencer node holds complete sequencer resources; the role
            // controller decides at runtime whether this node's engine and sequencer
            // background tasks are active.
            zone_node = zone_node
                .with_p2p(p2p_config)
                .with_sequencer(ZoneSequencerAddOnsConfig {
                    sequencer_signer: sequencer_signer.clone(),
                    l1_transaction_signer: None,
                    zone_id: 0,
                    zone_poll_interval: Duration::from_secs(1),
                    batch_anchor_config: Default::default(),
                    // Matches spawn_sequencer_with_config so both harness paths
                    // process withdrawals at the same cadence.
                    withdrawal_poll_interval: Duration::from_millis(500),
                    withdrawal_batch_limits: Default::default(),
                });
        }
        // Multi-sequencer nodes run the real role controller, which owns the engine; the
        // harness must not drive a second head writer against the same queue.
        let spawn_engine = spawn_engine && !p2p_enabled;
        if spawn_engine {
            // The harness drives its own ZoneEngine against the shared queue below, so the
            // node must keep enqueueing deposits even without a sequencer or P2P config.
            zone_node = zone_node.with_external_deposit_consumer();
        }

        // Don't use .dev() — it spawns a LocalMiner that conflicts with ZoneEngine.
        // The ZoneEngine is the sole block producer; it advances the chain when L1
        // blocks arrive in the deposit queue.
        let node_config = NodeConfig::new(Arc::new(chain_spec))
            .with_unused_ports()
            .with_rpc(
                RpcServerArgs::default()
                    .with_unused_ports()
                    .with_http()
                    .with_http_api(RpcModuleSelection::All),
            )
            .apply(|mut c| {
                c.network.discovery.disable_discovery = true;
                if p2p_enabled {
                    c.engine.persistence_threshold = 0;
                    c.engine.memory_block_buffer_target = 0;
                }
                c
            });

        let deposit_queue = zone_node.deposit_queue();
        let enabled_tokens = zone_node.enabled_tokens();
        let l1_state_cache = zone_node.l1_state_cache();
        let l1_block_tracker = zone_node.l1_block_tracker();
        if is_local_dummy_l1 {
            let mut cache = l1_state_cache.lock();
            seed_raw_tip403_token_policy(&mut cache, 0, PATH_USD_ADDRESS, ALLOW_ALL_POLICY_ID);
        }

        let node_handle = NodeBuilder::new(node_config)
            .testing_node(tasks.clone())
            .node(zone_node)
            .launch_with_debug_capabilities()
            .await?;

        let mut engine_stop = None;
        if spawn_engine {
            let provider = node_handle.node.provider();
            let last_header = provider
                .sealed_header(provider.best_block_number()?)?
                .ok_or_else(|| eyre::eyre!("no latest block header"))?;
            let stop = CancellationToken::new();
            engine_stop = Some(stop.clone());
            let engine = zone_node::ZoneEngine::new(
                provider.chain_spec(),
                node_handle.node.add_ons_handle.beacon_engine_handle.clone(),
                node_handle.node.payload_builder_handle.clone(),
                deposit_queue.clone(),
                l1_block_tracker.clone(),
                last_header,
                sequencer_signer.address(),
                SecretKey::from(sequencer_signer.credential()),
                portal_address,
            );
            node_handle
                .node
                .task_executor
                .spawn_critical_task("zone-engine", async move {
                    engine.run_until(stop).await;
                });
        }

        let http_url: url::Url = node_handle
            .node
            .rpc_server_handle()
            .http_url()
            .unwrap()
            .parse()
            .unwrap();

        // Build the real private RPC API while the handle is still concrete,
        // before type-erasing it into Box<dyn TestNodeHandle>.
        let eth_handlers = node_handle.node.eth_handlers().clone();
        let rpc_api_factory = Arc::new(move |config: zone_node::rpc::PrivateRpcConfig| {
            let eth_handlers = eth_handlers.clone();
            Box::pin(async move {
                Ok(
                    Arc::new(zone_node::rpc::ZoneRpc::new(eth_handlers, config).await?)
                        as Arc<dyn zone_node::rpc::ZoneRpcApi>,
                )
            })
                as Pin<Box<dyn Future<Output = eyre::Result<Arc<dyn zone_node::rpc::ZoneRpcApi>>>>>
        });

        Ok(Self {
            deposit_queue,
            enabled_tokens,
            http_url,
            l1_state_cache,
            l1_block_tracker,
            rpc_api_factory,
            node_handle: Box::new(node_handle),
            engine_stop,
            leadership,
            _tasks: tasks,
        })
    }
}
