//! Adapter between Tempo's protocol fee hooks and the Zone fee manager.

use alloy_evm::{Database, revm::context::Journal};
use alloy_primitives::{Address, U256};
use tempo_chainspec::hardfork::TempoHardfork;
use tempo_evm::{ProtocolFeeContext, ProtocolFeeManager};
use tempo_precompiles::{error::Result, storage::StorageActions};
use tempo_revm::{
    FeeTokenValidation, FeeTokenValidationContext, TempoStateAccess, TempoTx, TempoTxEnv,
};
use zone_precompiles::ZoneFeeManager;

/// Resolves the fee token selected by a Zone transaction against the supplied state view.
///
/// Both protocol execution and RPC gas allowance use this function so omitted fee tokens always
/// resolve through the Zone fee manager at the state being executed or simulated.
pub(crate) fn resolve_fee_token<S, M>(
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
        // Tempo's transaction handler calls this hook. The trait default reads the L1
        // TipFeeManager, so Zones must override it to resolve their genesis-configured default.
        resolve_fee_token(journal, tx, spec, actions)
    }

    fn validate_fee_token(
        &self,
        ctx: &mut FeeTokenValidationContext<'_, DB>,
        fee_token: Address,
    ) -> Result<FeeTokenValidation> {
        Ok(if ctx.is_initialized(fee_token)? {
            FeeTokenValidation::Valid
        } else {
            FeeTokenValidation::Invalid
        })
    }

    fn collect_fee_pre_tx(
        &self,
        ctx: ProtocolFeeContext<'_, DB>,
        fee_payer: Address,
        fee_token: Address,
        max_amount: U256,
        beneficiary: Address,
        _skip_liquidity_check: bool,
    ) -> Result<Address> {
        ctx.enter(|| {
            ZoneFeeManager::new().collect_fee_pre_tx(fee_payer, fee_token, max_amount, beneficiary)
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Bytes, U256, address};
    use revm::{
        context::JournalTr,
        database::{CacheDB, EmptyDB},
        state::{AccountInfo, Bytecode},
    };
    use zone_precompiles::{ZONE_FEE_MANAGER_ADDRESS, zone_fee_manager};

    #[test]
    fn resolves_explicit_token_or_zone_default() {
        let default_token = address!("0x20c00000000000000000000000000000000000d1");
        let explicit_token = address!("0x20c00000000000000000000000000000000000e1");
        let mut db = CacheDB::new(EmptyDB::default());
        db.insert_account_storage(
            ZONE_FEE_MANAGER_ADDRESS,
            zone_fee_manager::slots::DEFAULT_FEE_TOKEN,
            U256::from_be_slice(default_token.as_slice()),
        )
        .unwrap();

        assert_eq!(
            resolve_fee_token(
                &mut db,
                &TempoTxEnv::default(),
                TempoHardfork::T1,
                StorageActions::disabled(),
            )
            .unwrap(),
            default_token,
        );
        assert_eq!(
            resolve_fee_token(
                &mut db,
                &TempoTxEnv {
                    fee_token: Some(explicit_token),
                    ..Default::default()
                },
                TempoHardfork::T1,
                StorageActions::disabled(),
            )
            .unwrap(),
            explicit_token,
        );
    }

    #[test]
    fn accepts_any_initialized_zone_tip20_as_a_fee_token() {
        let initialized_token = address!("0x20c00000000000000000000000000000000000e1");
        let missing_token = address!("0x20c00000000000000000000000000000000000e2");
        let mut db = CacheDB::new(EmptyDB::default());
        db.insert_account_info(
            initialized_token,
            AccountInfo::from_bytecode(Bytecode::new_raw(Bytes::from_static(&[0xef]))),
        );
        let mut journal = Journal::new(db);
        let manager = ZoneProtocolFeeManager::new();

        let mut ctx = FeeTokenValidationContext::new(
            &mut journal,
            TempoHardfork::T9,
            StorageActions::disabled(),
        );
        assert_eq!(
            manager
                .validate_fee_token(&mut ctx, initialized_token)
                .unwrap(),
            FeeTokenValidation::Valid
        );
        assert_eq!(
            manager.validate_fee_token(&mut ctx, missing_token).unwrap(),
            FeeTokenValidation::Invalid
        );
    }
}
