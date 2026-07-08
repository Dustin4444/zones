//! Zone fee-manager precompile and protocol fee logic.
//!
//! Zones accept any fee token that is enabled on their L1 `ZonePortal`.
//! Fees are collected and credited directly in the user's fee token; the
//! Tempo FeeAMM routing path is intentionally disabled.

use alloc::string::ToString;

use alloy_evm::precompiles::DynPrecompile;
use alloy_primitives::{Address, B256, U256, keccak256};
use alloy_sol_types::{SolError, SolInterface, SolValue};
use core::fmt::Debug;
use revm::{
    Database,
    context::{Journal, Transaction as _},
    precompile::{PrecompileError, PrecompileId, PrecompileOutput, PrecompileResult},
};
use tempo_chainspec::hardfork::TempoHardfork;
use tempo_contracts::precompiles::{FeeManagerError, IFeeManager, ITIPFeeAMM};
use tempo_precompiles::{
    DelegateCallNotAllowed, Precompile as TempoPrecompile, charge_input_cost,
    error::Result as TempoResult,
    mutate_void,
    storage::{Handler, StorageCtx, actions::StorageActions, evm::EvmPrecompileStorageProvider},
    tip20::{TIP20Token, validate_usd_currency},
    tip20_factory::TIP20Factory,
    view,
};
use tempo_revm::{ProtocolFeeManager, TempoStateAccess, TempoTxEnv};
use zone_primitives::constants::{
    PORTAL_TOKEN_CONFIGS_SLOT, TEMPO_PACKED_SLOT, TEMPO_STATE_ADDRESS,
};

alloy_sol_types::sol! {
    error FeeAmmDisabled();
}

/// L1 storage access needed by zone precompiles.
pub trait L1StorageReader {
    /// Read one L1 storage slot at `block_number`.
    fn read_l1_storage(
        &self,
        account: Address,
        slot: B256,
        block_number: u64,
    ) -> Result<B256, PrecompileError>;
}

/// L1 portal storage access needed by zone fee-token validation.
pub trait ZonePortalReader: L1StorageReader {
    /// Address of the Tempo L1 `ZonePortal` backing this zone.
    fn portal_address(&self) -> Address;
}

/// Compute `ZonePortal._tokenConfigs[token]` storage slot.
pub fn portal_token_config_slot(token: Address) -> B256 {
    keccak256((token, PORTAL_TOKEN_CONFIGS_SLOT).abi_encode())
}

/// Zone fee manager.
///
/// This uses the upstream [`tempo_precompiles::tip_fee_manager::TipFeeManager`]
/// storage layout for user preferences and collected-fee ledgers so external
/// `IFeeManager` reads and `distributeFees` stay ABI-compatible.
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
    pub fn is_token_enabled_current(&self, token: Address) -> tempo_precompiles::Result<bool> {
        let block_number = current_tempo_block_number()?;
        self.is_token_enabled_at(token, block_number)
    }

    /// Require that `token` is enabled on the L1 portal at the zone's current
    /// Tempo checkpoint.
    pub fn ensure_token_enabled_current(&self, token: Address) -> tempo_precompiles::Result<()> {
        if self.is_token_enabled_current(token)? {
            Ok(())
        } else {
            Err(FeeManagerError::invalid_token().into())
        }
    }

    fn is_token_enabled_at(
        &self,
        token: Address,
        block_number: u64,
    ) -> tempo_precompiles::Result<bool> {
        if self.provider.portal_address().is_zero() {
            return Ok(true);
        }

        let slot = portal_token_config_slot(token);
        let value = self
            .provider
            .read_l1_storage(self.provider.portal_address(), slot, block_number)
            .map_err(|err| {
                tempo_precompiles::error::TempoPrecompileError::Fatal(err.to_string())
            })?;

        // TokenConfig.enabled is the lowest byte of the packed struct.
        Ok(value.as_slice()[31] & 1 != 0)
    }

    fn validate_fee_token(&self, token: Address) -> tempo_precompiles::Result<()> {
        if !TIP20Factory::new().is_tip20(token)? {
            return Err(FeeManagerError::invalid_token().into());
        }

        validate_usd_currency(token)?;
        self.ensure_token_enabled_current(token)
    }

    /// Set caller's user fee-token preference after zone enabled-token validation.
    pub fn set_user_token(
        &self,
        sender: Address,
        call: IFeeManager::setUserTokenCall,
    ) -> tempo_precompiles::Result<()> {
        self.validate_fee_token(call.token)?;
        tempo_precompiles::tip_fee_manager::TipFeeManager::new().set_user_token(sender, call)
    }

    /// Preserve the upstream validator-token preference API for compatibility.
    ///
    /// Zone protocol fee collection ignores validator preferences and credits
    /// fees directly in the user's token.
    pub fn set_validator_token(
        &self,
        sender: Address,
        call: IFeeManager::setValidatorTokenCall,
    ) -> tempo_precompiles::Result<()> {
        self.validate_fee_token(call.token)?;
        let beneficiary = StorageCtx.beneficiary();
        tempo_precompiles::tip_fee_manager::TipFeeManager::new().set_validator_token(
            sender,
            call,
            beneficiary,
        )
    }

    /// Collect the maximum possible fee before transaction execution.
    ///
    /// Unlike the Tempo L1 fee manager, this never checks or reserves FeeAMM
    /// liquidity because zones settle in the user's fee token directly.
    pub fn collect_fee_pre_tx(
        &self,
        fee_payer: Address,
        user_token: Address,
        max_amount: U256,
        _beneficiary: Address,
        _skip_liquidity_check: bool,
    ) -> tempo_precompiles::Result<Address> {
        self.validate_fee_token(user_token)?;

        let mut token = TIP20Token::from_address(user_token)?;
        token.ensure_transfer_authorized(
            fee_payer,
            tempo_precompiles::tip_fee_manager::TIP_FEE_MANAGER_ADDRESS,
        )?;
        token.transfer_fee_pre_tx(fee_payer, max_amount)?;

        Ok(user_token)
    }

    /// Settle the actual fee after transaction execution.
    ///
    /// Refunds unused fee tokens and credits the validator in the same token.
    pub fn collect_fee_post_tx(
        &self,
        fee_payer: Address,
        actual_spending: U256,
        refund_amount: U256,
        fee_token: Address,
        beneficiary: Address,
    ) -> tempo_precompiles::Result<U256> {
        self.validate_fee_token(fee_token)?;

        let mut token = TIP20Token::from_address(fee_token)?;
        token.transfer_fee_post_tx(fee_payer, refund_amount, actual_spending)?;

        if !actual_spending.is_zero() {
            tempo_precompiles::tip_fee_manager::TipFeeManager::new().collected_fees[beneficiary]
                [fee_token]
                .sinc(actual_spending)?;
        }

        Ok(actual_spending)
    }

    fn fee_amm_disabled(&self) -> PrecompileResult {
        Ok(StorageCtx.revert_output(FeeAmmDisabled {}.abi_encode().into()))
    }

    /// Create a [`DynPrecompile`] for the zone fee-manager ABI.
    pub fn create(
        provider: P,
        cfg: &revm::context::CfgEnv<tempo_chainspec::hardfork::TempoHardfork>,
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
                );

                StorageCtx::enter(&mut storage, || {
                    let mut manager = manager.clone();
                    manager.call(input.data, input.caller)
                })
            },
        )
    }
}

impl<P: ZonePortalReader> TempoPrecompile for ZoneFeeManager<P> {
    fn call(&mut self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        if let Some(err) = charge_input_cost(&mut StorageCtx::default(), calldata) {
            return err;
        }

        let Some(selector) = tempo_precompiles::dispatch::selector_from_calldata(calldata) else {
            return tempo_precompiles::dispatch::missing_selector_result();
        };

        if IFeeManager::IFeeManagerCalls::valid_selector(selector) {
            return tempo_precompiles::dispatch::dispatch_call(
                calldata,
                IFeeManager::IFeeManagerCalls::abi_decode,
                |call| self.call_fee_manager(call, msg_sender),
            );
        }

        if ITIPFeeAMM::ITIPFeeAMMCalls::valid_selector(selector) {
            return tempo_precompiles::dispatch::dispatch_call(
                calldata,
                ITIPFeeAMM::ITIPFeeAMMCalls::abi_decode,
                |call| self.call_fee_amm(call),
            );
        }

        tempo_precompiles::dispatch::unknown_selector_result(calldata)
    }
}

impl<P: ZonePortalReader> ZoneFeeManager<P> {
    fn call_fee_manager(
        &mut self,
        call: IFeeManager::IFeeManagerCalls,
        msg_sender: Address,
    ) -> PrecompileResult {
        match call {
            IFeeManager::IFeeManagerCalls::userTokens(call) => view(call, |c| {
                tempo_precompiles::tip_fee_manager::TipFeeManager::new().user_tokens(c)
            }),
            IFeeManager::IFeeManagerCalls::validatorTokens(call) => view(call, |c| {
                tempo_precompiles::tip_fee_manager::TipFeeManager::new()
                    .get_validator_token(c.validator)
            }),
            IFeeManager::IFeeManagerCalls::collectedFees(call) => view(call, |c| {
                tempo_precompiles::tip_fee_manager::TipFeeManager::new().collected_fees[c.validator]
                    [c.token]
                    .read()
            }),
            IFeeManager::IFeeManagerCalls::setValidatorToken(call) => {
                mutate_void(call, msg_sender, |s, c| self.set_validator_token(s, c))
            }
            IFeeManager::IFeeManagerCalls::setUserToken(call) => {
                mutate_void(call, msg_sender, |s, c| self.set_user_token(s, c))
            }
            IFeeManager::IFeeManagerCalls::distributeFees(call) => {
                mutate_void(call, msg_sender, |_, c| {
                    tempo_precompiles::tip_fee_manager::TipFeeManager::new()
                        .distribute_fees(c.validator, c.token)
                })
            }
        }
    }

    fn call_fee_amm(&self, call: ITIPFeeAMM::ITIPFeeAMMCalls) -> PrecompileResult {
        match call {
            ITIPFeeAMM::ITIPFeeAMMCalls::M(_)
            | ITIPFeeAMM::ITIPFeeAMMCalls::N(_)
            | ITIPFeeAMM::ITIPFeeAMMCalls::SCALE(_)
            | ITIPFeeAMM::ITIPFeeAMMCalls::MIN_LIQUIDITY(_)
            | ITIPFeeAMM::ITIPFeeAMMCalls::getPoolId(_)
            | ITIPFeeAMM::ITIPFeeAMMCalls::getPool(_)
            | ITIPFeeAMM::ITIPFeeAMMCalls::pools(_)
            | ITIPFeeAMM::ITIPFeeAMMCalls::totalSupply(_)
            | ITIPFeeAMM::ITIPFeeAMMCalls::liquidityBalances(_)
            | ITIPFeeAMM::ITIPFeeAMMCalls::mint(_)
            | ITIPFeeAMM::ITIPFeeAMMCalls::burn(_)
            | ITIPFeeAMM::ITIPFeeAMMCalls::rebalanceSwap(_) => self.fee_amm_disabled(),
        }
    }
}

impl<DB, P> ProtocolFeeManager<DB> for ZoneFeeManager<P>
where
    DB: Database,
    P: ZonePortalReader + Debug,
{
    fn get_fee_token(
        &self,
        journal: &mut Journal<DB>,
        tx: &TempoTxEnv,
        fee_payer: Address,
        spec: TempoHardfork,
        actions: StorageActions,
    ) -> TempoResult<Address> {
        let fee_token = <Journal<DB> as TempoStateAccess<((), ())>>::get_fee_token(
            journal,
            tx,
            fee_payer,
            spec,
            actions.clone(),
        )?;

        let charges_fees = !tx
            .max_balance_spending()
            .map_err(|err| tempo_precompiles::error::TempoPrecompileError::Fatal(err.to_string()))?
            .is_zero()
            || tx.is_subblock_transaction();
        if charges_fees {
            <Journal<DB> as TempoStateAccess<((), ())>>::with_read_only_storage_ctx(
                journal,
                spec,
                actions,
                || self.ensure_token_enabled_current(fee_token),
            )?;
        }

        Ok(fee_token)
    }

    fn collect_fee_pre_tx(
        &self,
        fee_payer: Address,
        user_token: Address,
        max_amount: U256,
        beneficiary: Address,
        skip_liquidity_check: bool,
    ) -> TempoResult<Address> {
        ZoneFeeManager::collect_fee_pre_tx(
            self,
            fee_payer,
            user_token,
            max_amount,
            beneficiary,
            skip_liquidity_check,
        )
    }

    fn collect_fee_post_tx(
        &self,
        fee_payer: Address,
        actual_spending: U256,
        refund_amount: U256,
        fee_token: Address,
        beneficiary: Address,
    ) -> TempoResult<U256> {
        ZoneFeeManager::collect_fee_post_tx(
            self,
            fee_payer,
            actual_spending,
            refund_amount,
            fee_token,
            beneficiary,
        )
    }
}

fn current_tempo_block_number() -> tempo_precompiles::Result<u64> {
    let packed = StorageCtx::default().sload(
        TEMPO_STATE_ADDRESS,
        U256::from_be_bytes(TEMPO_PACKED_SLOT.0),
    )?;
    Ok((packed & U256::from(u64::MAX)).to::<u64>())
}
