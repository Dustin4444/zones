//! [`ZoneRpcApi`] implementation backed by reth's EthApi.
//!
//! Re-exports the standalone `zone-rpc` crate so everything is accessible
//! via `zone_node::rpc::*`.

pub use zone_rpc::*;

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Weak},
    time::Duration,
};

use alloy_network::{ReceiptResponse, TransactionBuilder, TransactionResponse};
use alloy_primitives::{Address, B256, Bloom, Bytes, TxKind, U64, U256};
use alloy_provider::{DynProvider, Provider, ProviderBuilder};
use alloy_rpc_types_eth::{
    Block, BlockId, BlockNumberOrTag, BlockTransactions, Filter, FilterChanges, FilterId, Log,
    TransactionRequest,
    state::{EvmOverrides, StateOverride},
};
use alloy_sol_types::{SolCall, SolEvent, SolEventInterface};
use eyre::WrapErr;
use futures::StreamExt;
use reth_provider::CanonStateSubscriptions;
use reth_rpc::{EthFilter, eth::filter::EthFilterError};
use reth_rpc_builder::EthHandlers;
use reth_rpc_eth_api::{
    EthApiTypes, EthFilterApiServer, RpcConvert,
    helpers::{EthApiSpec, EthBlocks, EthCall, EthFees, EthState, EthTransactions, FullEthApi},
};
use reth_rpc_eth_types::logs_utils;
use tempo_alloy::{
    TempoNetwork,
    rpc::{TempoHeaderResponse, TempoTransactionRequest},
};
use tempo_contracts::precompiles::{
    ACCOUNT_KEYCHAIN_ADDRESS,
    account_keychain::IAccountKeychain::{self, KeyInfo, getKeyCall},
};
use tempo_primitives::{TempoHeader, TempoTxEnvelope};
use tokio::{
    sync::Mutex,
    time::{MissedTickBehavior, interval},
};

use alloy_rpc_client::ConnectionConfig;
use tempo_zone_contracts::{
    DepositType, TEMPO_STATE_ADDRESS, ZONE_INBOX_ADDRESS, ZONE_TOKEN_ADDRESS, ZoneInbox, ZonePortal,
};
use zone_rpc::{
    auth::AuthContext,
    types::{
        AuthorizationTokenInfoResponse, BoxEyreFut, BoxFut, DepositKind, DepositState,
        DepositStatusEntry, DepositStatusResponse, JsonRpcError, ZoneInfoResponse, internal,
        raw_null, raw_zero, to_raw,
    },
};

type RpcBlock = Block<alloy_rpc_types_eth::Transaction<TempoTxEnvelope>, TempoHeaderResponse>;
const FILTER_OWNER_PRUNE_INTERVAL: Duration = Duration::from_secs(60);

fn filter_not_found_error() -> JsonRpcError {
    JsonRpcError::invalid_params("filter not found")
}

fn map_eth_filter_error(err: EthFilterError) -> JsonRpcError {
    match err {
        EthFilterError::FilterNotFound(_) => filter_not_found_error(),
        other => internal(other),
    }
}

fn stale_filter_owner_ids(
    owner_ids: impl IntoIterator<Item = FilterId>,
    active_ids: &HashSet<FilterId>,
) -> Vec<FilterId> {
    owner_ids
        .into_iter()
        .filter(|id| !active_ids.contains(id))
        .collect()
}

fn zone_inbox_refunds_owner(to: Option<Address>, input: Option<&Bytes>) -> Option<Address> {
    if to != Some(ZONE_INBOX_ADDRESS) {
        return None;
    }

    let input = input?;
    if !input.starts_with(&ZoneInbox::refundsCall::SELECTOR) {
        return None;
    }

    ZoneInbox::refundsCall::abi_decode(input)
        .ok()
        .map(|call| call.owner)
}

fn zone_inbox_refunds_owner_mismatch(
    request: &TempoTransactionRequest,
    caller: Address,
) -> Option<Address> {
    if let Some(owner) = zone_inbox_refunds_owner(
        TransactionBuilder::to(request),
        TransactionBuilder::input(request),
    )
    .filter(|owner| *owner != caller)
    {
        return Some(owner);
    }

    request.calls.iter().find_map(|call| {
        let to = match call.to {
            TxKind::Call(to) => Some(to),
            TxKind::Create => None,
        };
        zone_inbox_refunds_owner(to, Some(&call.input)).filter(|owner| *owner != caller)
    })
}

async fn prune_filter_owners<Api: EthApiTypes + 'static>(
    filter: &EthFilter<Api>,
    owners: &Mutex<HashMap<FilterId, Address>>,
    backend_ids: &Mutex<HashMap<FilterId, Vec<FilterId>>>,
) {
    let owner_ids = {
        let owners = owners.lock().await;
        owners.keys().cloned().collect::<Vec<_>>()
    };
    if owner_ids.is_empty() {
        return;
    }

    let active_ids = filter
        .active_filters()
        .ids()
        .await
        .into_iter()
        .collect::<HashSet<_>>();
    let stale_ids = stale_filter_owner_ids(owner_ids, &active_ids);
    if stale_ids.is_empty() {
        return;
    }

    let mut owners = owners.lock().await;
    for id in stale_ids {
        owners.remove(&id);
        backend_ids.lock().await.remove(&id);
    }
}

fn sort_logs_by_chain_order(logs: &mut [Log]) {
    logs.sort_by_key(|log| (log.block_number, log.transaction_index, log.log_index));
}

/// [`ZoneRpcApi`] implementation backed by reth's [`EthHandlers`].
///
/// This is the privacy enforcement layer for the zone's JSON-RPC surface.
/// Only methods explicitly routed through [`ZoneRpcApi`] are reachable —
/// everything else is rejected by the dispatcher's [`classify_method`]
/// whitelist, so this struct effectively acts as an **enforced allowlist**
/// of Ethereum JSON-RPC endpoints.
///
/// For every allowed endpoint it applies typed privacy checks *before*
/// serializing to JSON:
///
/// - **Block redaction** — zeroing `logsBloom` and clearing transaction
///   lists for non-sequencer callers.
/// - **Sender-scoped access** — returning `null` for transactions and
///   receipts not owned by the authenticated caller.
/// - **`from`-enforcement** — `eth_call` / `eth_estimateGas` may only
///   simulate from the authenticated account (`-32004` on mismatch,
///   auto-set when omitted); state overrides are rejected for
///   non-sequencer callers (`-32602`).
/// - **Sender verification** — `eth_sendRawTransaction` checks that the
///   recovered transaction sender matches the authenticated account
///   (`-32003` on mismatch).
///
/// [`classify_method`]: zone_rpc::types::classify_method
pub struct ZoneRpc<Api: EthApiTypes> {
    eth: EthHandlers<Api>,
    config: zone_rpc::PrivateRpcConfig,
    l1_provider: DynProvider<TempoNetwork>,
    zone_provider: DynProvider<TempoNetwork>,
    tempo_state: tempo_zone_contracts::TempoState::TempoStateInstance<
        DynProvider<TempoNetwork>,
        TempoNetwork,
    >,
    /// Maps filter IDs to the authenticated account that created them.
    /// The reth filter registry remains the source of truth for filter liveness.
    filter_owners: Arc<Mutex<HashMap<FilterId, Address>>>,
    /// Maps private log filter IDs to all caller-scoped backend filter IDs.
    filter_backend_ids: Arc<Mutex<HashMap<FilterId, Vec<FilterId>>>>,
}

impl<Api: EthApiTypes + 'static> ZoneRpc<Api> {
    /// Wrap reth's [`EthHandlers`] (api + filter + pubsub).
    pub async fn new(
        eth: EthHandlers<Api>,
        config: zone_rpc::PrivateRpcConfig,
    ) -> eyre::Result<Self> {
        let l1_rpc_url = config.l1_rpc_url.clone();
        let zone_rpc_url = config.zone_rpc_url.clone();
        let l1_provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect_with_config(
                &l1_rpc_url,
                rpc_connection_config(config.retry_connection_interval),
            )
            .await
            .wrap_err("failed to connect private RPC L1 provider")?
            .erased();
        let zone_provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect_with_config(
                &zone_rpc_url,
                rpc_connection_config(config.retry_connection_interval),
            )
            .await
            .wrap_err("failed to connect private RPC zone provider")?
            .erased();
        let tempo_state =
            tempo_zone_contracts::TempoState::new(TEMPO_STATE_ADDRESS, zone_provider.clone());
        let rpc = Self {
            eth,
            config,
            l1_provider,
            zone_provider,
            tempo_state,
            filter_owners: Arc::new(Mutex::new(HashMap::new())),
            filter_backend_ids: Arc::new(Mutex::new(HashMap::new())),
        };
        rpc.spawn_filter_owner_pruner();
        Ok(rpc)
    }

    /// Returns a reference to the inner [`EthFilter`] handler.
    pub fn filter(&self) -> &EthFilter<Api> {
        &self.eth.filter
    }

    async fn filter_is_active(&self, id: &FilterId) -> bool {
        self.filter().active_filters().contains(id).await
    }

    fn spawn_filter_owner_pruner(&self)
    where
        Api: Send + Sync + 'static,
    {
        let filter = self.filter().clone();
        let owners: Weak<Mutex<HashMap<FilterId, Address>>> = Arc::downgrade(&self.filter_owners);
        let backend_ids: Weak<Mutex<HashMap<FilterId, Vec<FilterId>>>> =
            Arc::downgrade(&self.filter_backend_ids);
        tokio::spawn(async move {
            let mut prune_interval = interval(FILTER_OWNER_PRUNE_INTERVAL);
            prune_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

            loop {
                prune_interval.tick().await;

                let (Some(owners), Some(backend_ids)) = (owners.upgrade(), backend_ids.upgrade())
                else {
                    break;
                };

                prune_filter_owners(&filter, &owners, &backend_ids).await;
            }
        });
    }

    /// Verify that the filter belongs to the authenticated caller.
    ///
    /// Returns `Ok(())` if the caller owns the filter or is the sequencer.
    /// Returns an error indistinguishable from "filter not found" to avoid
    /// leaking filter existence to non-owners.
    async fn ensure_filter_owner(
        &self,
        id: &FilterId,
        auth: &AuthContext,
    ) -> Result<(), JsonRpcError> {
        let owner_matches = {
            let owners = self.filter_owners.lock().await;
            matches!(owners.get(id), Some(owner) if *owner == auth.caller)
        };
        if !owner_matches {
            return Err(filter_not_found_error());
        }
        if self.filter_is_active(id).await {
            Ok(())
        } else {
            self.filter_owners.lock().await.remove(id);
            self.filter_backend_ids.lock().await.remove(id);
            Err(filter_not_found_error())
        }
    }

    async fn filter_backend_ids(&self, id: &FilterId) -> Vec<FilterId> {
        self.filter_backend_ids
            .lock()
            .await
            .get(id)
            .cloned()
            .unwrap_or_else(|| vec![id.clone()])
    }

    async fn remove_filter_tracking(&self, id: &FilterId) {
        self.filter_owners.lock().await.remove(id);
        self.filter_backend_ids.lock().await.remove(id);
    }

    async fn portal_deposits_for_block(
        &self,
        tempo_block_number: u64,
    ) -> Result<Vec<PortalDepositRecord>, JsonRpcError> {
        if self.config.zone_portal.is_zero() {
            return Err(JsonRpcError::internal("zone portal not configured"));
        }

        let filter = Filter::new()
            .address(self.config.zone_portal)
            .from_block(tempo_block_number)
            .to_block(tempo_block_number)
            .event_signature(vec![
                ZonePortal::DepositMade::SIGNATURE_HASH,
                ZonePortal::EncryptedDepositMade::SIGNATURE_HASH,
            ]);

        let logs = self.l1_provider.get_logs(&filter).await.map_err(internal)?;
        let mut deposits = Vec::with_capacity(logs.len());

        for log in logs {
            match ZonePortal::ZonePortalEvents::decode_log(&log.inner)
                .map_err(internal)?
                .data
            {
                ZonePortal::ZonePortalEvents::DepositMade(event) => {
                    deposits.push(PortalDepositRecord::Regular {
                        deposit_hash: event.newCurrentDepositQueueHash,
                        sender: event.sender,
                        recipient: event.to,
                        token: event.token,
                        amount: event.netAmount,
                        memo: event.memo,
                    });
                }
                ZonePortal::ZonePortalEvents::EncryptedDepositMade(event) => {
                    deposits.push(PortalDepositRecord::Encrypted {
                        deposit_hash: event.newCurrentDepositQueueHash,
                        sender: event.sender,
                        token: event.token,
                        amount: event.netAmount,
                    });
                }
                _ => {}
            }
        }

        Ok(deposits)
    }

    async fn zone_tokens(&self) -> Result<Vec<Address>, JsonRpcError> {
        if self.config.zone_portal.is_zero() {
            return Ok(vec![ZONE_TOKEN_ADDRESS]);
        }

        ZonePortal::new(self.config.zone_portal, &self.l1_provider)
            .enabled_tokens()
            .await
            .map_err(internal)
    }

    async fn zone_sequencer(&self) -> Result<Address, JsonRpcError> {
        if self.config.zone_portal.is_zero() {
            return Ok(Address::ZERO);
        }

        ZonePortal::new(self.config.zone_portal, &self.l1_provider)
            .sequencer()
            .call()
            .await
            .map_err(internal)
    }

    async fn enforce_zone_inbox_refund_call_privacy(
        &self,
        request: &TempoTransactionRequest,
        auth: &AuthContext,
    ) -> Result<(), JsonRpcError> {
        if zone_inbox_refunds_owner_mismatch(request, auth.caller).is_none() {
            return Ok(());
        }

        if self.zone_sequencer().await? == auth.caller {
            return Ok(());
        }

        Err(JsonRpcError::account_mismatch())
    }

    async fn terminal_event_for_deposit(
        &self,
        deposit_hash: B256,
    ) -> Result<Option<TerminalDepositEvent>, JsonRpcError> {
        let filter = Filter::new()
            .address(ZONE_INBOX_ADDRESS)
            .from_block(0)
            .event_signature(vec![
                ZoneInbox::DepositProcessed::SIGNATURE_HASH,
                ZoneInbox::DepositFailed::SIGNATURE_HASH,
                ZoneInbox::EncryptedDepositProcessed::SIGNATURE_HASH,
                ZoneInbox::EncryptedDepositFailed::SIGNATURE_HASH,
                ZoneInbox::DepositRejected::SIGNATURE_HASH,
            ])
            .topic1(deposit_hash);

        let logs = self
            .zone_provider
            .get_logs(&filter)
            .await
            .map_err(internal)?;
        let Some(log) = logs.last() else {
            return Ok(None);
        };

        let Some(signature) = log.topics().first().copied() else {
            return Ok(None);
        };

        if signature == ZoneInbox::DepositProcessed::SIGNATURE_HASH {
            ZoneInbox::DepositProcessed::decode_log(&log.inner).map_err(internal)?;
            return Ok(Some(TerminalDepositEvent::RegularProcessed));
        }

        if signature == ZoneInbox::DepositFailed::SIGNATURE_HASH {
            ZoneInbox::DepositFailed::decode_log(&log.inner).map_err(internal)?;
            return Ok(Some(TerminalDepositEvent::RegularFailed));
        }

        if signature == ZoneInbox::EncryptedDepositProcessed::SIGNATURE_HASH {
            let event =
                ZoneInbox::EncryptedDepositProcessed::decode_log(&log.inner).map_err(internal)?;
            return Ok(Some(TerminalDepositEvent::EncryptedProcessed {
                recipient: event.to,
                memo: event.memo,
            }));
        }

        if signature == ZoneInbox::EncryptedDepositFailed::SIGNATURE_HASH {
            ZoneInbox::EncryptedDepositFailed::decode_log(&log.inner).map_err(internal)?;
            return Ok(Some(TerminalDepositEvent::EncryptedFailed));
        }

        if signature == ZoneInbox::DepositRejected::SIGNATURE_HASH {
            let event = ZoneInbox::DepositRejected::decode_log(&log.inner).map_err(internal)?;
            return match event.depositType {
                DepositType::Regular => Ok(Some(TerminalDepositEvent::RegularRejected)),
                DepositType::Encrypted => Ok(Some(TerminalDepositEvent::EncryptedRejected)),
                _ => Ok(None),
            };
        }

        Ok(None)
    }
}

impl<Api> zone_rpc::ZoneRpcApi for ZoneRpc<Api>
where
    Api: FullEthApi + EthApiTypes<NetworkTypes = TempoNetwork> + Send + Sync + 'static,
{
    fn get_keychain_key(&self, account: Address, key_id: Address) -> BoxEyreFut<'_, KeyInfo> {
        Box::pin(async move {
            let request = TempoTransactionRequest {
                inner: TransactionRequest {
                    to: Some(ACCOUNT_KEYCHAIN_ADDRESS.into()),
                    input: getKeyCall {
                        account,
                        keyId: key_id,
                    }
                    .abi_encode()
                    .into(),
                    ..Default::default()
                },
                ..Default::default()
            };

            let output = EthCall::call(&self.eth.api, request, None, EvmOverrides::default())
                .await
                .wrap_err("AccountKeychain.getKey eth_call failed")?;

            IAccountKeychain::getKeyCall::abi_decode_returns(output.as_ref()).map_err(Into::into)
        })
    }

    fn block_number(&self) -> BoxFut<'_> {
        Box::pin(async move {
            let info = EthApiSpec::chain_info(&self.eth.api).map_err(internal)?;
            to_raw(&U256::from(info.best_number))
        })
    }

    fn chain_id(&self) -> BoxFut<'_> {
        Box::pin(async move {
            let chain_id = EthApiSpec::chain_id(&self.eth.api);
            to_raw(&Some(chain_id))
        })
    }

    fn net_version(&self) -> BoxFut<'_> {
        Box::pin(async move {
            let chain_id = EthApiSpec::chain_id(&self.eth.api);
            to_raw(&chain_id.to_string())
        })
    }

    fn syncing(&self) -> BoxFut<'_> {
        Box::pin(async move {
            let status = EthApiSpec::sync_status(&self.eth.api).map_err(internal)?;
            to_raw(&status)
        })
    }

    fn coinbase(&self) -> BoxFut<'_> {
        Box::pin(async move { to_raw(&self.zone_sequencer().await?) })
    }

    fn gas_price(&self) -> BoxFut<'_> {
        Box::pin(async move {
            let price = EthFees::gas_price(&self.eth.api).await.map_err(internal)?;
            to_raw(&price)
        })
    }

    fn max_priority_fee_per_gas(&self) -> BoxFut<'_> {
        Box::pin(async move {
            let fee = EthFees::suggested_priority_fee(&self.eth.api)
                .await
                .map_err(internal)?;
            to_raw(&fee)
        })
    }

    fn fee_history(
        &self,
        block_count: u64,
        newest_block: BlockNumberOrTag,
        reward_percentiles: Option<Vec<f64>>,
    ) -> BoxFut<'_> {
        Box::pin(async move {
            let history =
                EthFees::fee_history(&self.eth.api, block_count, newest_block, reward_percentiles)
                    .await
                    .map_err(internal)?;
            to_raw(&history)
        })
    }

    fn get_balance(
        &self,
        address: Address,
        block: Option<BlockId>,
        auth: AuthContext,
    ) -> BoxFut<'_> {
        Box::pin(async move {
            // Silent dummy: non-caller addresses get "0x0" to avoid leaking account existence.
            if address != auth.caller {
                return Ok(raw_zero());
            }
            let balance = EthState::balance(&self.eth.api, address, block)
                .await
                .map_err(internal)?;
            to_raw(&balance)
        })
    }

    fn get_transaction_count(
        &self,
        address: Address,
        block: Option<BlockId>,
        auth: AuthContext,
    ) -> BoxFut<'_> {
        Box::pin(async move {
            // Silent dummy: non-caller addresses get "0x0" to avoid leaking account existence.
            if address != auth.caller {
                return Ok(raw_zero());
            }
            let count = EthState::transaction_count(&self.eth.api, address, block)
                .await
                .map_err(internal)?;
            to_raw(&count)
        })
    }

    fn block_by_number(
        &self,
        number: BlockNumberOrTag,
        full: bool,
        _auth: AuthContext,
    ) -> BoxFut<'_> {
        Box::pin(async move {
            let block = EthBlocks::rpc_block(&self.eth.api, number.into(), full)
                .await
                .map_err(internal)?;

            let Some(mut block) = block else {
                return Ok(raw_null());
            };

            redact_block(&mut block);

            to_raw(&block)
        })
    }

    fn block_by_hash(&self, hash: B256, full: bool, _auth: AuthContext) -> BoxFut<'_> {
        Box::pin(async move {
            let block = EthBlocks::rpc_block(&self.eth.api, hash.into(), full)
                .await
                .map_err(internal)?;

            let Some(mut block) = block else {
                return Ok(raw_null());
            };

            redact_block(&mut block);

            to_raw(&block)
        })
    }

    fn transaction_by_hash(&self, hash: B256, auth: AuthContext) -> BoxFut<'_> {
        Box::pin(async move {
            let tx = EthTransactions::transaction_by_hash(&self.eth.api, hash)
                .await
                .map_err(internal)?
                .map(|src| src.into_transaction(self.eth.api.converter()))
                .transpose()
                .map_err(internal)?;

            let Some(tx) = tx else { return Ok(raw_null()) };

            if tx.from() != auth.caller {
                return Ok(raw_null());
            }

            to_raw(&tx)
        })
    }

    fn transaction_receipt(&self, hash: B256, auth: AuthContext) -> BoxFut<'_> {
        Box::pin(async move {
            let receipt = EthTransactions::transaction_receipt(&self.eth.api, hash)
                .await
                .map_err(internal)?;

            let Some(mut receipt) = receipt else {
                return Ok(raw_null());
            };

            if receipt.from() != auth.caller {
                return Ok(raw_null());
            }

            receipt = zone_rpc::filter::filter_receipt_logs(receipt);

            to_raw(&receipt)
        })
    }

    fn call(
        &self,
        mut request: TempoTransactionRequest,
        block: Option<BlockId>,
        state_override: Option<StateOverride>,
        auth: AuthContext,
    ) -> BoxFut<'_> {
        Box::pin(async move {
            if state_override.is_some() {
                return Err(JsonRpcError::invalid_params("state overrides not allowed"));
            }

            zone_rpc::policy::enforce_from(&mut request, &auth)?;
            zone_rpc::policy::enforce_no_contract_creation(&request)?;
            self.enforce_zone_inbox_refund_call_privacy(&request, &auth)
                .await?;

            let result = EthCall::call(
                &self.eth.api,
                request,
                block,
                EvmOverrides::state(state_override),
            )
            .await
            .map_err(internal)?;
            to_raw(&result)
        })
    }

    fn estimate_gas(
        &self,
        mut request: TempoTransactionRequest,
        block: Option<BlockId>,
        state_override: Option<StateOverride>,
        auth: AuthContext,
    ) -> BoxFut<'_> {
        Box::pin(async move {
            if state_override.is_some() {
                return Err(JsonRpcError::invalid_params("state overrides not allowed"));
            }

            zone_rpc::policy::enforce_from(&mut request, &auth)?;

            zone_rpc::policy::enforce_no_contract_creation(&request)?;

            let result = EthCall::estimate_gas_at(
                &self.eth.api,
                request,
                block.unwrap_or_default(),
                EvmOverrides::state(state_override),
            )
            .await
            .map_err(internal)?;
            to_raw(&result)
        })
    }

    fn send_raw_transaction(&self, data: Bytes, auth: AuthContext) -> BoxFut<'_> {
        Box::pin(async move {
            zone_rpc::policy::verify_raw_tx_sender(&data, &auth)?;

            let hash = EthTransactions::send_raw_transaction(&self.eth.api, data)
                .await
                .map_err(internal)?;
            to_raw(&hash)
        })
    }

    fn send_raw_transaction_sync(&self, data: Bytes, auth: AuthContext) -> BoxFut<'_> {
        Box::pin(async move {
            zone_rpc::policy::verify_raw_tx_sender(&data, &auth)?;

            let mut receipt = EthTransactions::send_raw_transaction_sync(&self.eth.api, data)
                .await
                .map_err(internal)?;

            receipt = zone_rpc::filter::filter_receipt_logs(receipt);

            to_raw(&receipt)
        })
    }

    fn fill_transaction(
        &self,
        mut request: TempoTransactionRequest,
        auth: AuthContext,
    ) -> BoxFut<'_> {
        Box::pin(async move {
            zone_rpc::policy::enforce_from(&mut request, &auth)?;
            zone_rpc::policy::enforce_no_contract_creation(&request)?;

            let result = EthTransactions::fill_transaction(&self.eth.api, request)
                .await
                .map_err(internal)?;
            to_raw(&result)
        })
    }

    fn get_logs(&self, mut filter: Filter, auth: AuthContext) -> BoxFut<'_> {
        Box::pin(async move {
            let zone_tokens = self.zone_tokens().await?;
            zone_rpc::filter::scope_filter_addresses(&mut filter, &zone_tokens)?;
            let scoped_filters = zone_rpc::filter::scope_filters_for_caller(&filter, &auth.caller);
            let mut logs = Vec::new();
            for scoped_filter in scoped_filters {
                logs.extend(
                    EthFilterApiServer::logs(&self.eth.filter, scoped_filter)
                        .await
                        .map_err(internal)?,
                );
            }
            sort_logs_by_chain_order(&mut logs);
            let logs = zone_rpc::filter::dedup_logs(logs);
            let filtered = zone_rpc::filter::filter_logs(logs, &auth.caller);
            to_raw(&filtered)
        })
    }

    fn new_filter(&self, mut filter: Filter, auth: AuthContext) -> BoxFut<'_> {
        Box::pin(async move {
            let zone_tokens = self.zone_tokens().await?;
            zone_rpc::filter::scope_filter_addresses(&mut filter, &zone_tokens)?;
            let scoped_filters = zone_rpc::filter::scope_filters_for_caller(&filter, &auth.caller);
            let mut backend_ids = Vec::with_capacity(scoped_filters.len());
            for scoped_filter in scoped_filters {
                match EthFilterApiServer::new_filter(&self.eth.filter, scoped_filter).await {
                    Ok(id) => backend_ids.push(id),
                    Err(err) => {
                        for id in backend_ids {
                            let _ =
                                EthFilterApiServer::uninstall_filter(&self.eth.filter, id).await;
                        }
                        return Err(internal(err));
                    }
                }
            }
            let id = backend_ids
                .first()
                .cloned()
                .ok_or_else(|| JsonRpcError::internal("no backend filters created"))?;
            self.filter_owners
                .lock()
                .await
                .insert(id.clone(), auth.caller);
            if backend_ids.len() > 1 {
                self.filter_backend_ids
                    .lock()
                    .await
                    .insert(id.clone(), backend_ids);
            }
            to_raw(&id)
        })
    }

    fn get_filter_logs(&self, id: FilterId, auth: AuthContext) -> BoxFut<'_> {
        Box::pin(async move {
            self.ensure_filter_owner(&id, &auth).await?;

            let backend_ids = self.filter_backend_ids(&id).await;
            let mut logs = Vec::new();
            for backend_id in backend_ids {
                logs.extend(
                    self.filter()
                        .filter_logs(backend_id)
                        .await
                        .map_err(map_eth_filter_error)?,
                );
            }

            sort_logs_by_chain_order(&mut logs);
            let logs = zone_rpc::filter::dedup_logs(logs);
            let filtered = zone_rpc::filter::filter_logs(logs, &auth.caller);
            to_raw(&filtered)
        })
    }

    fn get_filter_changes(&self, id: FilterId, auth: AuthContext) -> BoxFut<'_> {
        Box::pin(async move {
            self.ensure_filter_owner(&id, &auth).await?;

            let backend_ids = self.filter_backend_ids(&id).await;
            let mut logs = Vec::new();
            let mut hashes = Vec::new();
            let mut saw_logs = false;
            let mut saw_hashes = false;

            for backend_id in backend_ids {
                let changes = self
                    .filter()
                    .filter_changes(backend_id)
                    .await
                    .map_err(map_eth_filter_error)?;

                match changes {
                    FilterChanges::Logs(new_logs) => {
                        saw_logs = true;
                        logs.extend(new_logs);
                    }
                    FilterChanges::Hashes(new_hashes) => {
                        saw_hashes = true;
                        hashes.extend(new_hashes);
                    }
                    // Pending transaction filters are disabled — return empty if one somehow exists
                    FilterChanges::Transactions(_) | FilterChanges::Empty => {}
                }
            }

            if saw_logs {
                sort_logs_by_chain_order(&mut logs);
                let logs = zone_rpc::filter::dedup_logs(logs);
                let filtered = zone_rpc::filter::filter_logs(logs, &auth.caller);
                to_raw(&FilterChanges::<
                    alloy_rpc_types_eth::Transaction<TempoTxEnvelope>,
                >::Logs(filtered))
            } else if saw_hashes {
                hashes.sort();
                hashes.dedup();
                to_raw(&FilterChanges::<
                    alloy_rpc_types_eth::Transaction<TempoTxEnvelope>,
                >::Hashes(hashes))
            } else {
                to_raw(&FilterChanges::<alloy_rpc_types_eth::Transaction<TempoTxEnvelope>>::Empty)
            }
        })
    }

    fn new_block_filter(&self, auth: AuthContext) -> BoxFut<'_> {
        Box::pin(async move {
            let id = EthFilterApiServer::new_block_filter(&self.eth.filter)
                .await
                .map_err(internal)?;
            self.filter_owners
                .lock()
                .await
                .insert(id.clone(), auth.caller);
            to_raw(&id)
        })
    }

    fn uninstall_filter(&self, id: FilterId, auth: AuthContext) -> BoxFut<'_> {
        Box::pin(async move {
            self.ensure_filter_owner(&id, &auth).await?;

            let backend_ids = self.filter_backend_ids(&id).await;
            let mut result = false;
            for backend_id in &backend_ids {
                result |=
                    EthFilterApiServer::uninstall_filter(&self.eth.filter, backend_id.clone())
                        .await
                        .map_err(internal)?;
            }

            if result || !self.filter_is_active(&id).await {
                self.remove_filter_tracking(&id).await;
            }

            to_raw(&result)
        })
    }

    fn ws_subscribe_new_heads(&self, _auth: AuthContext) -> BoxWsSubscriptionFut<'_> {
        Box::pin(async move {
            let api = self.eth.api.clone();
            let provider = self.eth.api.provider().clone();
            let stream = provider
                .canonical_state_stream()
                .flat_map(move |new_chain| {
                    let api = api.clone();
                    let headers = new_chain
                        .committed()
                        .blocks_iter()
                        .filter_map(move |block| {
                            match api
                                .converter()
                                .convert_header(block.clone_sealed_header(), block.rlp_length())
                            {
                                Ok(header) => Some(header),
                                Err(err) => {
                                    tracing::error!(
                                        target: "rpc",
                                        %err,
                                        "Failed to convert header"
                                    );
                                    None
                                }
                            }
                        })
                        .collect::<Vec<_>>();
                    futures::stream::iter(headers)
                })
                .map(move |mut header| {
                    redact_ws_header(&mut header);
                    to_raw(&header)
                });
            let stream: zone_rpc::WsSubscriptionStream = Box::pin(stream);
            Ok(stream)
        })
    }

    fn ws_subscribe_logs(&self, mut filter: Filter, auth: AuthContext) -> BoxWsSubscriptionFut<'_> {
        Box::pin(async move {
            let provider = self.eth.api.provider().clone();
            let caller = auth.caller;

            let zone_tokens = self.zone_tokens().await?;
            zone_rpc::filter::scope_filter_addresses(&mut filter, &zone_tokens)?;
            let scoped_filters = zone_rpc::filter::scope_filters_for_caller(&filter, &caller);

            let stream = provider
                .canonical_state_stream()
                .flat_map(|canon_state| futures::stream::iter(canon_state.block_receipts()))
                .flat_map(move |(block_receipts, removed)| {
                    let mut all_logs = scoped_filters
                        .iter()
                        .flat_map(|filter| {
                            logs_utils::matching_block_logs_with_tx_hashes(
                                filter,
                                block_receipts.block,
                                block_receipts.timestamp,
                                block_receipts
                                    .tx_receipts
                                    .iter()
                                    .map(|(tx, receipt)| (*tx, receipt)),
                                removed,
                            )
                        })
                        .collect::<Vec<_>>();
                    sort_logs_by_chain_order(&mut all_logs);
                    futures::stream::iter(zone_rpc::filter::dedup_logs(all_logs))
                });

            let stream = stream.filter_map(move |log| {
                std::future::ready(
                    zone_rpc::filter::is_log_visible(&log, &caller).then(|| to_raw(&log)),
                )
            });
            let stream: zone_rpc::WsSubscriptionStream = Box::pin(stream);
            Ok(stream)
        })
    }

    fn zone_get_authorization_token_info(&self, auth: AuthContext) -> BoxFut<'_> {
        Box::pin(async move {
            to_raw(&AuthorizationTokenInfoResponse {
                account: auth.caller,
                expires_at: U64::from(auth.expires_at),
            })
        })
    }

    fn zone_get_zone_info(&self, _auth: AuthContext) -> BoxFut<'_> {
        Box::pin(async move {
            let zone_tokens = self.zone_tokens().await?;
            let sequencer = self.zone_sequencer().await?;
            to_raw(&ZoneInfoResponse {
                zone_id: U64::from(self.config.zone_id),
                zone_tokens,
                sequencer,
                chain_id: U64::from(self.config.chain_id),
            })
        })
    }

    fn zone_get_deposit_status(&self, tempo_block_number: u64, auth: AuthContext) -> BoxFut<'_> {
        Box::pin(async move {
            let zone_processed_through = self
                .tempo_state
                .tempoBlockNumber()
                .call()
                .await
                .map_err(internal)?;
            let portal_deposits = self.portal_deposits_for_block(tempo_block_number).await?;

            let mut deposits = Vec::new();
            for deposit in portal_deposits {
                match deposit {
                    PortalDepositRecord::Regular {
                        deposit_hash,
                        sender,
                        recipient,
                        token,
                        amount,
                        memo,
                    } => {
                        if sender != auth.caller && recipient != auth.caller {
                            continue;
                        }

                        let terminal = self.terminal_event_for_deposit(deposit_hash).await?;
                        let status = regular_deposit_status(terminal)?;

                        deposits.push(DepositStatusEntry {
                            deposit_hash,
                            kind: DepositKind::Regular,
                            token,
                            sender,
                            recipient: Some(recipient),
                            amount: U256::from(amount),
                            memo: Some(memo),
                            status,
                        });
                    }
                    PortalDepositRecord::Encrypted {
                        deposit_hash,
                        sender,
                        token,
                        amount,
                    } => {
                        let terminal = self.terminal_event_for_deposit(deposit_hash).await?;

                        let include = match (&terminal, sender == auth.caller) {
                            (_, true) => true,
                            (
                                Some(TerminalDepositEvent::EncryptedProcessed {
                                    recipient, ..
                                }),
                                false,
                            ) => *recipient == auth.caller,
                            _ => false,
                        };

                        if !include {
                            continue;
                        }

                        let (recipient, memo, status) = encrypted_deposit_details(terminal)?;

                        deposits.push(DepositStatusEntry {
                            deposit_hash,
                            kind: DepositKind::Encrypted,
                            token,
                            sender,
                            recipient,
                            amount: U256::from(amount),
                            memo,
                            status,
                        });
                    }
                }
            }

            let processed = zone_processed_through >= tempo_block_number
                && deposits
                    .iter()
                    .all(|deposit| deposit.status != DepositState::Pending);

            to_raw(&DepositStatusResponse {
                tempo_block_number: U64::from(tempo_block_number),
                zone_processed_through: U64::from(zone_processed_through),
                processed,
                deposits,
            })
        })
    }
}

#[derive(Debug, Clone)]
enum PortalDepositRecord {
    Regular {
        deposit_hash: B256,
        sender: Address,
        recipient: Address,
        token: Address,
        amount: u128,
        memo: B256,
    },
    Encrypted {
        deposit_hash: B256,
        sender: Address,
        token: Address,
        amount: u128,
    },
}

#[derive(Debug, Clone)]
enum TerminalDepositEvent {
    RegularProcessed,
    RegularFailed,
    RegularRejected,
    EncryptedProcessed { recipient: Address, memo: B256 },
    EncryptedFailed,
    EncryptedRejected,
}

fn regular_deposit_status(
    terminal: Option<TerminalDepositEvent>,
) -> Result<DepositState, JsonRpcError> {
    match terminal {
        Some(TerminalDepositEvent::RegularProcessed) => Ok(DepositState::Processed),
        Some(TerminalDepositEvent::RegularFailed | TerminalDepositEvent::RegularRejected) => {
            Ok(DepositState::Failed)
        }
        Some(TerminalDepositEvent::EncryptedProcessed { .. }) => Err(JsonRpcError::internal(
            "encrypted deposit event matched regular deposit hash",
        )),
        Some(TerminalDepositEvent::EncryptedFailed | TerminalDepositEvent::EncryptedRejected) => {
            Err(JsonRpcError::internal(
                "encrypted deposit failure matched regular deposit hash",
            ))
        }
        None => Ok(DepositState::Pending),
    }
}

fn encrypted_deposit_details(
    terminal: Option<TerminalDepositEvent>,
) -> Result<(Option<Address>, Option<B256>, DepositState), JsonRpcError> {
    match terminal {
        Some(TerminalDepositEvent::EncryptedProcessed { recipient, memo }) => {
            Ok((Some(recipient), Some(memo), DepositState::Processed))
        }
        Some(TerminalDepositEvent::EncryptedFailed | TerminalDepositEvent::EncryptedRejected) => {
            Ok((None, None, DepositState::Failed))
        }
        Some(
            TerminalDepositEvent::RegularProcessed
            | TerminalDepositEvent::RegularFailed
            | TerminalDepositEvent::RegularRejected,
        ) => Err(JsonRpcError::internal(
            "regular deposit event matched encrypted deposit hash",
        )),
        None => Ok((None, None, DepositState::Pending)),
    }
}

fn redact_tempo_header(header: &mut TempoHeader) {
    header.inner.logs_bloom = Bloom::ZERO;
}

fn redact_ws_header(header: &mut TempoHeaderResponse) {
    redact_tempo_header(&mut header.inner.inner);
}

/// Strip privacy-sensitive fields from a block for non-sequencer callers.
fn redact_block(block: &mut RpcBlock) {
    redact_tempo_header(&mut block.header.inner);
    block.transactions = BlockTransactions::Hashes(Vec::new());
}

pub(crate) fn rpc_connection_config(retry_connection_interval: Duration) -> ConnectionConfig {
    ConnectionConfig::new()
        .with_max_retries(u32::MAX)
        .with_retry_interval(retry_connection_interval)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_rpc_types_eth::TransactionInput;
    use tempo_primitives::transaction::Call;

    fn zone_inbox_refunds_request(owner: Address) -> TempoTransactionRequest {
        TempoTransactionRequest {
            inner: TransactionRequest {
                to: Some(TxKind::Call(ZONE_INBOX_ADDRESS)),
                input: TransactionInput::new(
                    ZoneInbox::refundsCall {
                        token: ZONE_TOKEN_ADDRESS,
                        owner,
                    }
                    .abi_encode()
                    .into(),
                ),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn regular_deposit_status_maps_terminal_events() {
        assert_eq!(
            regular_deposit_status(Some(TerminalDepositEvent::RegularProcessed)).unwrap(),
            DepositState::Processed
        );
        assert_eq!(regular_deposit_status(None).unwrap(), DepositState::Pending);
    }

    #[test]
    fn regular_deposit_status_rejects_encrypted_terminal_events() {
        let err = regular_deposit_status(Some(TerminalDepositEvent::EncryptedFailed)).unwrap_err();
        assert_eq!(
            err.message,
            "encrypted deposit failure matched regular deposit hash"
        );
    }

    #[test]
    fn encrypted_deposit_details_maps_terminal_events() {
        let recipient = Address::repeat_byte(0x11);
        let memo = B256::from([0x22; 32]);

        assert_eq!(
            encrypted_deposit_details(Some(TerminalDepositEvent::EncryptedProcessed {
                recipient,
                memo,
            }))
            .unwrap(),
            (Some(recipient), Some(memo), DepositState::Processed)
        );
        assert_eq!(
            encrypted_deposit_details(Some(TerminalDepositEvent::EncryptedFailed)).unwrap(),
            (None, None, DepositState::Failed)
        );
        assert_eq!(
            encrypted_deposit_details(None).unwrap(),
            (None, None, DepositState::Pending)
        );
    }

    #[test]
    fn encrypted_deposit_details_rejects_regular_terminal_events() {
        let err =
            encrypted_deposit_details(Some(TerminalDepositEvent::RegularProcessed)).unwrap_err();
        assert_eq!(
            err.message,
            "regular deposit event matched encrypted deposit hash"
        );
    }

    #[test]
    fn stale_filter_owner_ids_removes_only_inactive_entries() {
        let active_ids = HashSet::from([
            FilterId::Str("0xactive".to_string()),
            FilterId::Str("0xkeep".to_string()),
        ]);
        let owner_ids = vec![
            FilterId::Str("0xactive".to_string()),
            FilterId::Str("0xstale".to_string()),
            FilterId::Str("0xkeep".to_string()),
        ];

        let stale_ids = stale_filter_owner_ids(owner_ids, &active_ids);

        assert_eq!(stale_ids, vec![FilterId::Str("0xstale".to_string())]);
    }

    #[test]
    fn stale_filter_owner_ids_is_noop_for_empty_owner_set() {
        let stale_ids = stale_filter_owner_ids(Vec::new(), &HashSet::new());

        assert!(stale_ids.is_empty());
    }

    #[test]
    fn zone_inbox_refunds_owner_mismatch_detects_outer_call() {
        let caller = Address::repeat_byte(0x11);
        let owner = Address::repeat_byte(0x22);
        let request = zone_inbox_refunds_request(owner);

        assert_eq!(
            zone_inbox_refunds_owner_mismatch(&request, caller),
            Some(owner)
        );
    }

    #[test]
    fn zone_inbox_refunds_owner_mismatch_allows_own_outer_call() {
        let caller = Address::repeat_byte(0x11);
        let request = zone_inbox_refunds_request(caller);

        assert_eq!(zone_inbox_refunds_owner_mismatch(&request, caller), None);
    }

    #[test]
    fn zone_inbox_refunds_owner_mismatch_detects_nested_tempo_call() {
        let caller = Address::repeat_byte(0x11);
        let owner = Address::repeat_byte(0x22);
        let mut request = TempoTransactionRequest {
            inner: TransactionRequest {
                to: Some(TxKind::Call(Address::repeat_byte(0x33))),
                ..Default::default()
            },
            ..Default::default()
        };
        request.calls.push(Call {
            to: TxKind::Call(ZONE_INBOX_ADDRESS),
            value: U256::ZERO,
            input: ZoneInbox::refundsCall {
                token: ZONE_TOKEN_ADDRESS,
                owner,
            }
            .abi_encode()
            .into(),
        });

        assert_eq!(
            zone_inbox_refunds_owner_mismatch(&request, caller),
            Some(owner)
        );
    }

    #[test]
    fn zone_inbox_refunds_owner_mismatch_ignores_other_calls() {
        let caller = Address::repeat_byte(0x11);
        let mut request = zone_inbox_refunds_request(Address::repeat_byte(0x22));
        request.inner.to = Some(TxKind::Call(Address::repeat_byte(0x33)));

        assert_eq!(zone_inbox_refunds_owner_mismatch(&request, caller), None);
    }
}
