//! Zone fee-manager precompile.
//!
//! Zones accept any fee token enabled on their L1 `ZonePortal`. Fees are
//! credited directly in that token; the Tempo FeeAMM path is disabled.

pub mod dispatch;

use alloc::{format, rc::Rc};
use alloy_evm::precompiles::DynPrecompile;
use alloy_primitives::{Address, B256, IntoLogData, U256, keccak256};
use alloy_sol_types::{SolError, SolValue};
use core::cell::RefCell;
use revm::{
    context::CfgEnv,
    precompile::{PrecompileId, PrecompileOutput, PrecompileResult},
};
use tempo_chainspec::hardfork::TempoHardfork;
use tempo_contracts::precompiles::{FeeManagerError, IFeeManager};
use tempo_precompiles::{
    DelegateCallNotAllowed, Precompile as TempoPrecompile, TIP_FEE_MANAGER_ADDRESS,
    error::{Result as TempoResult, TempoPrecompileError},
    storage::{Handler, Slot, StorageActions, StorageCtx, evm::EvmPrecompileStorageProvider},
    storage_credits::NonCreditableSlots,
    tip_fee_manager::{FeeManagerEvent, TipFeeManager},
    tip20_factory::TIP20Factory,
};
use zone_primitives::constants::TEMPO_STATE_ADDRESS;

use crate::{L1StorageReader, tempo_state::slots as tempo_state_slots};

alloy_sol_types::sol! {
    error FeeAmmDisabled();
}

/// L1 portal storage access needed by zone fee-token validation.
pub trait ZonePortalReader: L1StorageReader + core::fmt::Debug {
    /// Address of the Tempo L1 `ZonePortal` backing this zone.
    fn portal_address(&self) -> Address;
}

/// Compute `ZonePortal._tokenConfigs[token]` storage slot.
pub fn portal_token_config_slot(token: Address) -> B256 {
    keccak256((token, U256::from(8)).abi_encode())
}

fn tempo_block_number() -> TempoResult<u64> {
    Slot::<u64>::new(tempo_state_slots::TEMPO_BLOCK_NUMBER, TEMPO_STATE_ADDRESS).read()
}

/// Public FeeManager precompile for zones.
#[derive(Debug, Clone)]
pub struct ZoneFeeManager<P> {
    provider: P,
}

impl<P: ZonePortalReader> ZoneFeeManager<P> {
    /// Create a new zone fee manager backed by an L1 portal reader.
    pub const fn new(provider: P) -> Self {
        Self { provider }
    }

    /// Returns true if `token` is enabled on the L1 portal at the zone's
    /// current Tempo checkpoint.
    pub fn is_token_enabled_current(&self, token: Address) -> TempoResult<bool> {
        self.is_token_enabled_at(token, tempo_block_number()?)
    }

    /// Require that `token` is enabled on the L1 portal at the zone's current
    /// Tempo checkpoint.
    pub fn ensure_token_enabled_current(&self, token: Address) -> TempoResult<()> {
        if self.is_token_enabled_current(token)? {
            Ok(())
        } else {
            Err(FeeManagerError::invalid_token().into())
        }
    }

    fn is_token_enabled_at(&self, token: Address, block_number: u64) -> TempoResult<bool> {
        if self.provider.portal_address().is_zero() {
            return Ok(true);
        }

        let slot = portal_token_config_slot(token);
        let value = self
            .provider
            .read_l1_storage(self.provider.portal_address(), slot, block_number)
            .map_err(|err| TempoPrecompileError::Fatal(format!("{err}")))?;

        // TokenConfig.enabled is the lowest byte of the packed struct.
        Ok(value.as_slice()[31] & 1 != 0)
    }

    fn validate_fee_token(&self, token: Address) -> TempoResult<()> {
        if !TIP20Factory::new().is_tip20(token)? {
            return Err(FeeManagerError::invalid_token().into());
        }

        self.ensure_token_enabled_current(token)
    }

    /// Reads the stored fee token preference for `user`.
    pub fn user_tokens(&self, call: IFeeManager::userTokensCall) -> TempoResult<Address> {
        TipFeeManager::new().user_tokens(call)
    }

    /// Reads the validator fee token preference.
    pub fn get_validator_token(&self, validator: Address) -> TempoResult<Address> {
        TipFeeManager::new().get_validator_token(validator)
    }

    /// Reads accumulated fees for `validator` in `token`.
    pub fn collected_fees(&self, validator: Address, token: Address) -> TempoResult<U256> {
        TipFeeManager::new().collected_fees[validator][token].read()
    }

    /// Set caller's preferred fee token after zone enabled-token validation.
    pub fn set_user_token(
        &self,
        sender: Address,
        call: IFeeManager::setUserTokenCall,
    ) -> TempoResult<()> {
        self.validate_fee_token(call.token)?;

        let mut fee_manager = TipFeeManager::new();
        if StorageCtx.spec().is_t3() && fee_manager.user_tokens[sender].read()? == call.token {
            return Ok(());
        }

        fee_manager.user_tokens[sender].write(call.token)?;
        StorageCtx.emit_event(
            TIP_FEE_MANAGER_ADDRESS,
            FeeManagerEvent::user_token_set(sender, call.token).into_log_data(),
        )
    }

    /// Preserve the upstream validator-token preference API for compatibility.
    ///
    /// Zone protocol fee collection will credit fees directly in the user's
    /// token once the protocol hook is wired.
    pub fn set_validator_token(
        &self,
        sender: Address,
        call: IFeeManager::setValidatorTokenCall,
    ) -> TempoResult<()> {
        self.validate_fee_token(call.token)?;

        let mut fee_manager = TipFeeManager::new();
        fee_manager.validator_tokens[sender].write(call.token)?;
        StorageCtx.emit_event(
            TIP_FEE_MANAGER_ADDRESS,
            FeeManagerEvent::validator_token_set(sender, call.token).into_log_data(),
        )
    }

    /// Distributes collected fees for `validator` in `token`.
    pub fn distribute_fees(&self, validator: Address, token: Address) -> TempoResult<()> {
        TipFeeManager::new().distribute_fees(validator, token)
    }

    pub(crate) fn fee_amm_disabled(&self) -> PrecompileResult {
        Ok(StorageCtx.revert_output(FeeAmmDisabled {}.abi_encode().into()))
    }

    /// Create a [`DynPrecompile`] for the zone FeeManager ABI.
    pub fn create(
        provider: P,
        cfg: &CfgEnv<TempoHardfork>,
        actions: StorageActions,
        non_creditable_slots: Rc<RefCell<NonCreditableSlots>>,
    ) -> DynPrecompile
    where
        P: Clone + Send + Sync + 'static,
    {
        let manager = Self::new(provider);
        let spec = cfg.spec;
        let amsterdam_eip8037_enabled = cfg.enable_amsterdam_eip8037;
        let gas_params = cfg.gas_params.clone();

        DynPrecompile::new_stateful(
            PrecompileId::Custom("ZoneFeeManager".into()),
            move |input| {
                if !input.is_direct_call() {
                    return Ok(PrecompileOutput::revert(
                        0,
                        DelegateCallNotAllowed {}.abi_encode().into(),
                        input.reservoir,
                    ));
                }

                let mut storage = EvmPrecompileStorageProvider::new(
                    input.internals,
                    input.gas,
                    input.reservoir,
                    spec,
                    amsterdam_eip8037_enabled,
                    input.is_static,
                    gas_params.clone(),
                )
                .with_actions(actions.clone())
                .with_non_creditable_slots(non_creditable_slots.clone());

                StorageCtx::enter(&mut storage, || {
                    let mut manager = manager.clone();
                    manager.call(input.data, input.caller)
                })
            },
        )
    }
}
