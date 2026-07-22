//! Adapter between Tempo's protocol fee hooks and the Zone fee manager.

use alloy_evm::{Database, revm::context::Journal};
use alloy_primitives::{Address, U256};
use tempo_chainspec::hardfork::TempoHardfork;
use tempo_evm::{ProtocolFeeContext, ProtocolFeeManager};
use tempo_precompiles::{error::Result, storage::StorageActions};
use tempo_revm::{TempoStateAccess, TempoTx, TempoTxEnv};
use zone_precompiles::ZoneFeeManager;

/// Resolves the fee token selected by a Zone transaction against the supplied state view.
///
/// Both protocol execution and RPC gas allowance use this function so omitted fee tokens always
/// resolve through the Zone fee manager at the state being executed or simulated.
pub fn resolve_fee_token<S, M>(
    state: &mut S,
    tx: &TempoTxEnv,
    spec: TempoHardfork,
    actions: StorageActions,
) -> Result<Address>
where
    S: TempoStateAccess<M>,
{
    if let Some(token) = tx.fee_token() {
        return Ok(token);
    }

    state.with_read_only_storage_ctx(spec, actions, || ZoneFeeManager::new().default_fee_token())
}

/// Resolves and collects fees without Tempo token preferences or FeeAMM settlement.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ZoneProtocolFeeManager;

impl ZoneProtocolFeeManager {
    pub(crate) const fn new() -> Self {
        Self
    }
}

impl<DB> ProtocolFeeManager<DB> for ZoneProtocolFeeManager
where
    DB: Database,
{
    fn get_fee_token(
        &self,
        journal: &mut Journal<DB>,
        tx: &TempoTxEnv,
        _fee_payer: Address,
        spec: TempoHardfork,
        actions: StorageActions,
    ) -> Result<Address> {
        resolve_fee_token(journal, tx, spec, actions)
    }

    fn collect_fee_pre_tx(
        &self,
        ctx: ProtocolFeeContext<'_, DB>,
        fee_payer: Address,
        fee_token: Address,
        max_amount: U256,
        _beneficiary: Address,
        _skip_liquidity_check: bool,
    ) -> Result<Address> {
        ctx.enter(|| ZoneFeeManager::new().collect_fee_pre_tx(fee_payer, fee_token, max_amount))
    }

    fn collect_fee_post_tx(
        &self,
        ctx: ProtocolFeeContext<'_, DB>,
        fee_payer: Address,
        actual_spending: U256,
        refund_amount: U256,
        fee_token: Address,
        beneficiary: Address,
    ) -> Result<U256> {
        ctx.enter(|| {
            ZoneFeeManager::new().collect_fee_post_tx(
                fee_payer,
                actual_spending,
                refund_amount,
                fee_token,
                beneficiary,
            )
        })
    }
}
