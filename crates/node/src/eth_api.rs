//! Zone-aware wrapper for Tempo's `eth_` RPC implementation.

use std::{sync::Arc, time::Duration};

use alloy_primitives::{Address, B256, U256};
use alloy_rpc_types_eth::BlockId;
use reth_evm::{
    EvmEnvFor, TxEnvFor,
    revm::{Database, context::result::EVMError},
};
use reth_node_api::FullNodeComponents;
use reth_node_builder::rpc::{EthApiBuilder, EthApiCtx};
use reth_primitives_traits::WithEncoded;
use reth_provider::ProviderError;
use reth_rpc_eth_api::{
    EthApiTypes, FullEthApiServer, RpcNodeCore, RpcNodeCoreExt, RpcTxReq,
    helpers::{
        Call, EthApiSpec, EthBlocks, EthCall, EthFees, EthState, EthTransactions, LoadBlock,
        LoadFee, LoadPendingBlock, LoadReceipt, LoadState, LoadTransaction, SpawnBlocking, Trace,
        bal::GetBlockAccessList, estimate::EstimateCall, pending_block::PendingEnvBuilder,
        spec::SignersForRpc, subscriptions::EthSubscriptions,
    },
};
use reth_rpc_eth_types::{
    EthApiError, EthStateCache, FeeHistoryCache, GasPriceOracle, PendingBlock,
    builder::config::PendingBlockKind,
};
use reth_tasks::{
    Runtime,
    pool::{BlockingTaskGuard, BlockingTaskPool},
};
use reth_transaction_pool::{PoolTx, TransactionOrigin};
use tempo_chainspec::hardfork::TempoHardfork;
use tempo_node::rpc::{TempoEthApi, TempoEthApiBounds, TempoEthApiBuilder};
use tempo_precompiles::storage::StorageActions;
use tempo_primitives::TEMPO_GAS_PRICE_SCALING_FACTOR;
use tempo_revm::TempoStateAccess;
use tokio::sync::Mutex;
use zone_evm::resolve_fee_token;

fn fee_token_gas_allowance<DB>(
    db: &mut DB,
    tx_env: &tempo_revm::TempoTxEnv,
    fee_payer: Address,
    spec: TempoHardfork,
) -> Result<u64, ProviderError>
where
    DB: Database,
{
    let actions = StorageActions::disabled();
    let fee_token =
        resolve_fee_token(db, tx_env, spec, actions.clone()).map_err(ProviderError::other)?;
    let balance = db
        .get_token_balance(fee_token, fee_payer, spec, actions)
        .map_err(ProviderError::other)?;

    Ok(balance
        .saturating_mul(TEMPO_GAS_PRICE_SCALING_FACTOR)
        .checked_div(U256::from(tx_env.inner.gas_price))
        .unwrap_or_default()
        .saturating_to())
}

/// Tempo's RPC implementation with Zone-specific fee-token resolution.
#[derive(Debug, Clone)]
pub struct ZoneEthApi<N>
where
    N: TempoEthApiBounds,
{
    inner: TempoEthApi<N>,
}

impl<N> ZoneEthApi<N>
where
    N: TempoEthApiBounds,
{
    fn new(inner: TempoEthApi<N>) -> Self {
        Self { inner }
    }
}

impl<N> EthApiTypes for ZoneEthApi<N>
where
    N: TempoEthApiBounds,
{
    type Error = <TempoEthApi<N> as EthApiTypes>::Error;
    type NetworkTypes = <TempoEthApi<N> as EthApiTypes>::NetworkTypes;
    type RpcConvert = <TempoEthApi<N> as EthApiTypes>::RpcConvert;

    fn converter(&self) -> &Self::RpcConvert {
        self.inner.converter()
    }
}

impl<N> RpcNodeCore for ZoneEthApi<N>
where
    N: TempoEthApiBounds,
{
    type Primitives = <TempoEthApi<N> as RpcNodeCore>::Primitives;
    type Provider = <TempoEthApi<N> as RpcNodeCore>::Provider;
    type Pool = <TempoEthApi<N> as RpcNodeCore>::Pool;
    type Evm = <TempoEthApi<N> as RpcNodeCore>::Evm;
    type Network = <TempoEthApi<N> as RpcNodeCore>::Network;

    fn pool(&self) -> &Self::Pool {
        self.inner.pool()
    }

    fn evm_config(&self) -> &Self::Evm {
        self.inner.evm_config()
    }

    fn network(&self) -> &Self::Network {
        self.inner.network()
    }

    fn provider(&self) -> &Self::Provider {
        self.inner.provider()
    }
}

impl<N> RpcNodeCoreExt for ZoneEthApi<N>
where
    N: TempoEthApiBounds,
{
    fn cache(&self) -> &EthStateCache<Self::Primitives> {
        self.inner.cache()
    }
}

impl<N> EthApiSpec for ZoneEthApi<N>
where
    N: TempoEthApiBounds,
{
    fn starting_block(&self) -> U256 {
        self.inner.starting_block()
    }
}

impl<N> SpawnBlocking for ZoneEthApi<N>
where
    N: TempoEthApiBounds,
{
    fn io_task_spawner(&self) -> &Runtime {
        self.inner.io_task_spawner()
    }

    fn tracing_task_pool(&self) -> &BlockingTaskPool {
        self.inner.tracing_task_pool()
    }

    fn tracing_task_guard(&self) -> &BlockingTaskGuard {
        self.inner.tracing_task_guard()
    }

    fn blocking_io_task_guard(&self) -> &Arc<tokio::sync::Semaphore> {
        self.inner.blocking_io_task_guard()
    }
}

impl<N> LoadPendingBlock for ZoneEthApi<N>
where
    N: TempoEthApiBounds,
{
    fn pending_block(&self) -> &Mutex<Option<PendingBlock<Self::Primitives>>> {
        self.inner.pending_block()
    }

    fn pending_env_builder(&self) -> &dyn PendingEnvBuilder<Self::Evm> {
        self.inner.pending_env_builder()
    }

    fn pending_block_kind(&self) -> PendingBlockKind {
        self.inner.pending_block_kind()
    }
}

impl<N> LoadFee for ZoneEthApi<N>
where
    N: TempoEthApiBounds,
{
    fn gas_oracle(&self) -> &GasPriceOracle<Self::Provider> {
        self.inner.gas_oracle()
    }

    fn fee_history_cache(
        &self,
    ) -> &FeeHistoryCache<reth_primitives_traits::HeaderTy<Self::Primitives>> {
        self.inner.fee_history_cache()
    }
}

impl<N> LoadState for ZoneEthApi<N>
where
    N: TempoEthApiBounds,
{
    async fn next_available_nonce_for(
        &self,
        request: &RpcTxReq<Self::NetworkTypes>,
    ) -> Result<u64, Self::Error> {
        self.inner.next_available_nonce_for(request).await
    }
}

impl<N> EthState for ZoneEthApi<N>
where
    N: TempoEthApiBounds,
{
    async fn balance(
        &self,
        address: Address,
        block_id: Option<BlockId>,
    ) -> Result<U256, Self::Error> {
        self.inner.balance(address, block_id).await
    }

    fn max_proof_window(&self) -> u64 {
        self.inner.max_proof_window()
    }
}

impl<N> EthFees for ZoneEthApi<N> where N: TempoEthApiBounds {}
impl<N> Trace for ZoneEthApi<N> where N: TempoEthApiBounds {}
impl<N> EthCall for ZoneEthApi<N> where N: TempoEthApiBounds {}
impl<N> GetBlockAccessList for ZoneEthApi<N> where N: TempoEthApiBounds {}
impl<N> EthSubscriptions for ZoneEthApi<N> where N: TempoEthApiBounds {}

impl<N> Call for ZoneEthApi<N>
where
    N: TempoEthApiBounds,
{
    fn call_gas_limit(&self) -> u64 {
        self.inner.call_gas_limit()
    }

    fn max_simulate_blocks(&self) -> u64 {
        self.inner.max_simulate_blocks()
    }

    fn compute_state_root_for_eth_simulate(&self) -> bool {
        self.inner.compute_state_root_for_eth_simulate()
    }

    fn evm_memory_limit(&self) -> u64 {
        self.inner.evm_memory_limit()
    }

    fn caller_gas_allowance(
        &self,
        mut db: impl Database<Error: Into<EthApiError>>,
        evm_env: &EvmEnvFor<Self::Evm>,
        tx_env: &TxEnvFor<Self::Evm>,
    ) -> Result<u64, Self::Error> {
        let fee_payer = tx_env
            .fee_payer()
            .map_err(EVMError::<ProviderError, _>::from)?;
        fee_token_gas_allowance(&mut db, tx_env, fee_payer, evm_env.cfg_env.spec)
            .map_err(Into::into)
    }

    fn create_txn_env(
        &self,
        evm_env: &EvmEnvFor<Self::Evm>,
        request: tempo_alloy::rpc::TempoTransactionRequest,
        db: impl Database<Error: Into<EthApiError>>,
    ) -> Result<TxEnvFor<Self::Evm>, Self::Error> {
        self.inner.create_txn_env(evm_env, request, db)
    }
}

impl<N> EstimateCall for ZoneEthApi<N> where N: TempoEthApiBounds {}
impl<N> LoadBlock for ZoneEthApi<N> where N: TempoEthApiBounds {}
impl<N> LoadReceipt for ZoneEthApi<N> where N: TempoEthApiBounds {}
impl<N> EthBlocks for ZoneEthApi<N> where N: TempoEthApiBounds {}
impl<N> LoadTransaction for ZoneEthApi<N> where N: TempoEthApiBounds {}

impl<N> EthTransactions for ZoneEthApi<N>
where
    N: TempoEthApiBounds,
{
    fn signers(&self) -> &SignersForRpc<Self::Provider, Self::NetworkTypes> {
        self.inner.signers()
    }

    fn send_raw_transaction_sync_timeout(&self) -> Duration {
        self.inner.send_raw_transaction_sync_timeout()
    }

    fn send_pool_transaction(
        &self,
        origin: TransactionOrigin,
        tx: WithEncoded<PoolTx<Self::Pool>>,
    ) -> impl Future<Output = Result<B256, Self::Error>> + Send {
        self.inner.send_pool_transaction(origin, tx)
    }
}

/// Builds [`ZoneEthApi`] on top of Tempo's standard RPC implementation.
#[derive(Debug)]
pub struct ZoneEthApiBuilder<N = ()> {
    inner: TempoEthApiBuilder<N>,
}

impl<N> Default for ZoneEthApiBuilder<N> {
    fn default() -> Self {
        Self {
            inner: TempoEthApiBuilder::default(),
        }
    }
}

impl<N> EthApiBuilder<N> for ZoneEthApiBuilder<N>
where
    N: FullNodeComponents + TempoEthApiBounds,
    TempoEthApiBuilder<N>: EthApiBuilder<N, EthApi = TempoEthApi<N>>,
    ZoneEthApi<N>: FullEthApiServer<
            Provider = <N as reth_node_api::FullNodeTypes>::Provider,
            Pool = <N as FullNodeComponents>::Pool,
        >,
{
    type EthApi = ZoneEthApi<N>;

    async fn build_eth_api(self, ctx: EthApiCtx<'_, N>) -> eyre::Result<Self::EthApi> {
        let inner = self.inner.build_eth_api(ctx).await?;
        Ok(ZoneEthApi::new(inner))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;
    use reth_evm::revm::{
        context::TxEnv,
        database::{CacheDB, EmptyDB},
    };
    use tempo_precompiles::{storage::StorageKey, tip20::tip20_slots};
    use tempo_revm::TempoTxEnv;
    use zone_precompiles::{ZONE_FEE_MANAGER_ADDRESS, zone_fee_manager};

    #[test]
    fn gas_allowance_uses_non_path_zone_default_when_fee_token_is_omitted() {
        let fee_payer = address!("0x00000000000000000000000000000000000000a1");
        let default_fee_token = address!("0x20c00000000000000000000000000000000000d1");
        let balance = U256::from(50_000);
        let gas_price = 2_000_000_000u128;
        let mut db = CacheDB::new(EmptyDB::default());
        db.insert_account_storage(
            ZONE_FEE_MANAGER_ADDRESS,
            zone_fee_manager::slots::DEFAULT_FEE_TOKEN,
            U256::from_be_slice(default_fee_token.as_slice()),
        )
        .unwrap();
        db.insert_account_storage(
            default_fee_token,
            fee_payer.mapping_slot(tip20_slots::BALANCES),
            balance,
        )
        .unwrap();
        let tx_env = TempoTxEnv {
            inner: TxEnv {
                caller: fee_payer,
                gas_price,
                ..Default::default()
            },
            fee_token: None,
            ..Default::default()
        };

        let allowance =
            fee_token_gas_allowance(&mut db, &tx_env, fee_payer, TempoHardfork::T1).unwrap();

        assert_eq!(
            allowance,
            balance
                .saturating_mul(TEMPO_GAS_PRICE_SCALING_FACTOR)
                .checked_div(U256::from(gas_price))
                .unwrap()
                .saturating_to::<u64>()
        );
    }
}
