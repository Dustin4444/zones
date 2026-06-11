//! Zone `eth_` API wrapper.
//!
//! This mirrors Tempo's `TempoEthApi` pattern: keep reth's `EthApi` as the inner
//! implementation, but override the parts where Tempo fee-token semantics differ
//! from Ethereum's native-balance semantics.

use std::{future::Future, sync::Arc, time::Duration};

use alloy_eips::BlockId;
use alloy_primitives::{B256, U256};
use futures::TryFutureExt;
use reth_evm::{
    EvmEnvFor, TxEnvFor,
    revm::{Database, context::result::EVMError},
};
use reth_node_api::{FullNodeComponents, HeaderTy, PrimitivesTy};
use reth_primitives_traits::{Recovered, WithEncoded};
use reth_provider::ProviderError;
use reth_rpc::{DynRpcConverter, EthApi};
use reth_rpc_eth_api::{
    EthApiTypes, FromEthApiError, IntoEthApiError, RpcNodeCore, RpcNodeCoreExt, RpcTxReq,
    helpers::{
        Call, EthApiSpec, EthBlocks, EthCall, EthFees, EthState, EthTransactions, LoadBlock,
        LoadFee, LoadPendingBlock, LoadReceipt, LoadState, LoadTransaction, SpawnBlocking, Trace,
        bal::GetBlockAccessList, estimate::EstimateCall, pending_block::PendingEnvBuilder,
        spec::SignersForRpc,
    },
};
use reth_rpc_eth_types::{
    EthApiError, EthStateCache, FeeHistoryCache, GasPriceOracle, PendingBlock, SignError,
    builder::config::PendingBlockKind,
};
use reth_tasks::{
    Runtime,
    pool::{BlockingTaskGuard, BlockingTaskPool},
};
use reth_transaction_pool::{PoolPooledTx, TransactionOrigin};
use tempo_alloy::{TempoNetwork, rpc::TempoTransactionRequest};
use tempo_evm::TempoStateAccess;
use tempo_node::rpc::{NATIVE_BALANCE_PLACEHOLDER, error::TempoEthApiError};
use tempo_precompiles::{NONCE_PRECOMPILE_ADDRESS, nonce::NonceManager};
use tempo_primitives::{TEMPO_GAS_PRICE_SCALING_FACTOR, transaction::TEMPO_EXPIRING_NONCE_KEY};
use tokio::sync::Mutex;

use crate::{evm::ZoneEvmConfig, node::ZoneNode};

pub(crate) type ZoneInnerEthApi<N> = EthApi<N, DynRpcConverter<ZoneEvmConfig, TempoNetwork>>;

/// Zone `eth_` API implementation.
#[derive(Debug, Clone)]
pub struct ZoneEthApi<N>
where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>,
{
    inner: ZoneInnerEthApi<N>,
}

impl<N> ZoneEthApi<N>
where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>,
{
    pub(crate) const fn new(inner: ZoneInnerEthApi<N>) -> Self {
        Self { inner }
    }
}

impl<N> EthApiTypes for ZoneEthApi<N>
where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>,
{
    type Error = TempoEthApiError;
    type NetworkTypes = TempoNetwork;
    type RpcConvert = DynRpcConverter<ZoneEvmConfig, TempoNetwork>;

    fn converter(&self) -> &Self::RpcConvert {
        self.inner.converter()
    }
}

impl<N> RpcNodeCore for ZoneEthApi<N>
where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>,
{
    type Primitives = PrimitivesTy<N::Types>;
    type Provider = N::Provider;
    type Pool = N::Pool;
    type Evm = N::Evm;
    type Network = N::Network;

    #[inline]
    fn pool(&self) -> &Self::Pool {
        self.inner.pool()
    }

    #[inline]
    fn evm_config(&self) -> &Self::Evm {
        self.inner.evm_config()
    }

    #[inline]
    fn network(&self) -> &Self::Network {
        self.inner.network()
    }

    #[inline]
    fn provider(&self) -> &Self::Provider {
        self.inner.provider()
    }
}

impl<N> RpcNodeCoreExt for ZoneEthApi<N>
where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>,
{
    #[inline]
    fn cache(&self) -> &EthStateCache<PrimitivesTy<N::Types>> {
        self.inner.cache()
    }
}

impl<N> EthApiSpec for ZoneEthApi<N>
where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>,
{
    #[inline]
    fn starting_block(&self) -> U256 {
        self.inner.starting_block()
    }
}

impl<N> SpawnBlocking for ZoneEthApi<N>
where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>,
{
    #[inline]
    fn io_task_spawner(&self) -> &Runtime {
        self.inner.task_spawner()
    }

    #[inline]
    fn tracing_task_pool(&self) -> &BlockingTaskPool {
        self.inner.blocking_task_pool()
    }

    #[inline]
    fn tracing_task_guard(&self) -> &BlockingTaskGuard {
        self.inner.blocking_task_guard()
    }

    #[inline]
    fn blocking_io_task_guard(&self) -> &Arc<tokio::sync::Semaphore> {
        self.inner.blocking_io_task_guard()
    }
}

impl<N> LoadPendingBlock for ZoneEthApi<N>
where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>,
{
    #[inline]
    fn pending_block(&self) -> &Mutex<Option<PendingBlock<Self::Primitives>>> {
        self.inner.pending_block()
    }

    #[inline]
    fn pending_env_builder(&self) -> &dyn PendingEnvBuilder<Self::Evm> {
        self.inner.pending_env_builder()
    }

    #[inline]
    fn pending_block_kind(&self) -> PendingBlockKind {
        self.inner.pending_block_kind()
    }
}

impl<N> LoadFee for ZoneEthApi<N>
where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>,
{
    #[inline]
    fn gas_oracle(&self) -> &GasPriceOracle<Self::Provider> {
        self.inner.gas_oracle()
    }

    #[inline]
    fn fee_history_cache(&self) -> &FeeHistoryCache<HeaderTy<N::Types>> {
        self.inner.fee_history_cache()
    }
}

impl<N> LoadState for ZoneEthApi<N>
where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>,
{
    async fn next_available_nonce_for(
        &self,
        request: &RpcTxReq<Self::NetworkTypes>,
    ) -> Result<u64, Self::Error> {
        if let Some(nonce_key) = request.nonce_key
            && !nonce_key.is_zero()
        {
            let nonce = if nonce_key == TEMPO_EXPIRING_NONCE_KEY {
                0
            } else {
                let from = request
                    .from
                    .ok_or_else(|| SignError::NoAccount.into_eth_err::<Self::Error>())?;
                let slot = NonceManager::new().nonces[from][nonce_key].slot();
                self.spawn_blocking_io(move |this| {
                    this.latest_state()?
                        .storage(NONCE_PRECOMPILE_ADDRESS, slot.into())
                        .map_err(Self::Error::from_eth_err)
                })
                .await?
                .unwrap_or_default()
                .saturating_to()
            };

            Ok(nonce)
        } else {
            Ok(self.inner.next_available_nonce_for(request).await?)
        }
    }
}

impl<N> EthState for ZoneEthApi<N>
where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>,
{
    #[inline]
    async fn balance(
        &self,
        _address: alloy_primitives::Address,
        _block_id: Option<BlockId>,
    ) -> Result<U256, Self::Error> {
        Ok(NATIVE_BALANCE_PLACEHOLDER)
    }

    #[inline]
    fn max_proof_window(&self) -> u64 {
        self.inner.eth_proof_window()
    }
}

impl<N> EthFees for ZoneEthApi<N> where N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig> {}

impl<N> Trace for ZoneEthApi<N> where N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig> {}

impl<N> EthCall for ZoneEthApi<N> where N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig> {}

impl<N> GetBlockAccessList for ZoneEthApi<N> where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>
{
}

impl<N> Call for ZoneEthApi<N>
where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>,
{
    #[inline]
    fn call_gas_limit(&self) -> u64 {
        self.inner.gas_cap()
    }

    #[inline]
    fn max_simulate_blocks(&self) -> u64 {
        self.inner.max_simulate_blocks()
    }

    #[inline]
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

        let fee_token = db
            .get_fee_token(tx_env, fee_payer, evm_env.cfg_env.spec)
            .map_err(ProviderError::other)?;
        let fee_token_balance = db
            .get_token_balance(fee_token, fee_payer, evm_env.cfg_env.spec)
            .map_err(ProviderError::other)?;

        Ok(fee_token_balance
            .saturating_mul(TEMPO_GAS_PRICE_SCALING_FACTOR)
            .checked_div(U256::from(tx_env.inner.gas_price))
            .unwrap_or_default()
            .saturating_to())
    }

    fn create_txn_env(
        &self,
        evm_env: &EvmEnvFor<Self::Evm>,
        mut request: TempoTransactionRequest,
        mut db: impl Database<Error: Into<EthApiError>>,
    ) -> Result<TxEnvFor<Self::Evm>, Self::Error> {
        if let Some(nonce_key) = request.nonce_key
            && !nonce_key.is_zero()
            && request.nonce.is_none()
        {
            let nonce = if nonce_key == TEMPO_EXPIRING_NONCE_KEY {
                0
            } else {
                let slot =
                    NonceManager::new().nonces[request.from.unwrap_or_default()][nonce_key].slot();
                db.storage(NONCE_PRECOMPILE_ADDRESS, slot)
                    .map_err(Into::into)?
                    .saturating_to()
            };
            request.nonce = Some(nonce);
        }

        Ok(self.inner.create_txn_env(evm_env, request, db)?)
    }
}

impl<N> EstimateCall for ZoneEthApi<N> where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>
{
}
impl<N> LoadBlock for ZoneEthApi<N> where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>
{
}
impl<N> LoadReceipt for ZoneEthApi<N> where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>
{
}
impl<N> EthBlocks for ZoneEthApi<N> where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>
{
}
impl<N> LoadTransaction for ZoneEthApi<N> where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>
{
}

impl<N> EthTransactions for ZoneEthApi<N>
where
    N: FullNodeComponents<Types = ZoneNode, Evm = ZoneEvmConfig>,
{
    fn signers(&self) -> &SignersForRpc<Self::Provider, Self::NetworkTypes> {
        self.inner.signers()
    }

    fn send_raw_transaction_sync_timeout(&self) -> Duration {
        self.inner.send_raw_transaction_sync_timeout()
    }

    fn send_transaction(
        &self,
        origin: TransactionOrigin,
        tx: WithEncoded<Recovered<PoolPooledTx<Self::Pool>>>,
    ) -> impl Future<Output = Result<B256, Self::Error>> + Send {
        self.inner.send_transaction(origin, tx).map_err(Into::into)
    }
}
