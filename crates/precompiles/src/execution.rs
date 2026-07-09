//! Execution helpers for zone-native and upstream Tempo precompiles.
//!
//! Each helper installs an EVM-backed [`StorageCtx`], applies zone-specific [`CallRules`], and
//! forwards allowed calls to the supplied implementation without changing the calldata or caller.
//!
//! # Execution modes
//!
//! - [`create_local_precompile`] executes against ordinary zone-local EVM state.
//! - [`create_l1_backed_precompile`] binds execution to the finalized Tempo block recorded in
//!   `TempoState` and overlays selected policy reads from L1. Indirect calls are rejected before
//!   the anchor or any L1 state is read.
//!
//! # Call flow
//!
//! 1. Decode the selector and reject calls that cannot cover a configured fixed gas charge.
//! 2. Construct the storage provider and enter its [`StorageCtx`]. L1-backed calls read their
//!    block anchor once before installing the L1 overlay.
//! 3. Apply [`CallRules`]. Allowed calls execute the supplied implementation; rejected calls
//!    return the rule's result without invoking it.
//! 4. Apply the configured fixed gas charge to successful results.
//!
//! Rule-level rejections include calldata input gas. Calls without a fixed charge retain the
//! provider's normal metering, while successful fixed-price calls report exactly that charge.

use alloc::rc::Rc;
use core::cell::RefCell;

use alloy_evm::precompiles::DynPrecompile;
use alloy_primitives::Address;
use alloy_sol_types::SolError;
use revm::precompile::{PrecompileHalt, PrecompileId, PrecompileOutput, PrecompileResult};
use tempo_chainspec::hardfork::TempoHardfork;
use tempo_precompiles::{
    DelegateCallNotAllowed, charge_input_cost,
    dispatch::selector_from_calldata,
    storage::{StorageCtx, actions::StorageActions, evm::EvmPrecompileStorageProvider},
    storage_credits::NonCreditableSlots,
};

use crate::storage::{
    L1StorageReader, ZonePrecompileStorageProvider, read_l1_anchor, storage_error_result,
};

/// Shared inputs for precompiles that execute against finalized Tempo L1 state.
///
/// Each call combines the zone EVM configuration and accounting state with an L1
/// reader. The call's exact L1 block is resolved from the local `TempoState` anchor.
#[derive(Clone)]
pub(crate) struct L1BackedPrecompileEnv<P> {
    cfg: revm::context::CfgEnv<TempoHardfork>,
    l1_reader: P,
    actions: StorageActions,
    non_creditable_slots: Rc<RefCell<NonCreditableSlots>>,
}

impl<P> L1BackedPrecompileEnv<P> {
    /// Capture the configuration and providers used by L1-backed calls.
    pub(crate) fn new(
        cfg: &revm::context::CfgEnv<TempoHardfork>,
        l1_reader: P,
        actions: StorageActions,
        non_creditable_slots: Rc<RefCell<NonCreditableSlots>>,
    ) -> Self {
        Self {
            cfg: cfg.clone(),
            l1_reader,
            actions,
            non_creditable_slots,
        }
    }
}

/// Call metadata, independent of EVM internals, for [`CallRules`] running in a [`StorageCtx`].
/// Provider-free precompiles can inspect `PrecompileInput` directly.
///
/// **MOTIVATION:** Execution helpers move `PrecompileInput::internals` into the
/// [`EvmPrecompileStorageProvider`] before calling [`CallRules::check_call`]. The full input
/// cannot be borrowed after that partial move, so [`ZoneCall`] carries only the metadata
/// needed by [`CallRules`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct ZoneCall<'a> {
    /// Input calldata.
    pub(crate) data: &'a [u8],
    /// Decoded 4-byte selector, when calldata is long enough.
    pub(crate) selector: Option<[u8; 4]>,
    /// EVM caller.
    pub(crate) caller: Address,
    /// Whether target and bytecode addresses match.
    pub(crate) is_direct: bool,
}

/// Result of applying zone-specific pre-execution rules.
pub(crate) enum CallCheck {
    /// Allow the call and invoke the supplied precompile implementation.
    ///
    /// For L1-backed precompiles, this forwards to upstream Tempo with the zone's
    /// finalized L1 storage overlay active.
    Continue,
    /// Reject the call without invoking the supplied implementation.
    ///
    /// The execution helper charges calldata input gas before returning the result.
    Return(PrecompileResult),
}

/// Selector- and caller-dependent pre-execution rules for a storage-backed precompile.
///
/// Checks receive [`ZoneCall`] because they run inside [`StorageCtx`], after the input's
/// EVM internals have moved into the storage provider.
pub(crate) trait CallRules: 'static {
    /// Return a fixed gas charge for this selector, if one applies.
    fn fixed_gas(&self, _selector: Option<[u8; 4]>) -> Option<u64> {
        None
    }

    /// Decide whether execution may proceed to the supplied implementation.
    fn check_call(&self, _call: ZoneCall<'_>) -> CallCheck {
        CallCheck::Continue
    }
}

/// Precompiles without zone-specific call rules.
pub(crate) struct NoCallRules;

impl CallRules for NoCallRules {}

/// Direct-call-only rule for precompiles whose semantics depend on their own address or storage.
pub(crate) struct DirectCallOnly;

impl CallRules for DirectCallOnly {
    fn check_call(&self, call: ZoneCall<'_>) -> CallCheck {
        if call.is_direct {
            CallCheck::Continue
        } else {
            CallCheck::Return(Ok(StorageCtx::default()
                .revert_output(SolError::abi_encode(&DelegateCallNotAllowed {}).into())))
        }
    }
}

/// Create a precompile with call rules and ordinary zone-local storage.
///
/// The helper installs Tempo's normal [`EvmPrecompileStorageProvider`] before applying
/// the rules. It neither reads an L1 anchor nor installs an L1 storage overlay.
pub(crate) fn create_local_precompile<Rules, Execute>(
    id: &'static str,
    cfg: &revm::context::CfgEnv<TempoHardfork>,
    rules: Rules,
    execute: Execute,
) -> DynPrecompile
where
    Rules: CallRules,
    Execute: Fn(&[u8], Address) -> PrecompileResult + 'static,
{
    let spec = cfg.spec;
    let amsterdam_eip8037_enabled = cfg.enable_amsterdam_eip8037;
    let gas_params = cfg.gas_params.clone();

    DynPrecompile::new_stateful(PrecompileId::Custom(id.into()), move |input| {
        let selector = selector_from_calldata(input.data);
        let fixed_gas = rules.fixed_gas(selector);
        if let Some(gas) = fixed_gas
            && input.gas < gas
        {
            return Ok(PrecompileOutput::halt(
                PrecompileHalt::OutOfGas,
                input.reservoir,
            ));
        }

        let is_direct = input.is_direct_call();
        let mut storage = EvmPrecompileStorageProvider::new(
            input.internals,
            fixed_gas.map_or(input.gas, |_| u64::MAX),
            input.reservoir,
            spec,
            amsterdam_eip8037_enabled,
            input.is_static,
            gas_params.clone(),
        );

        StorageCtx::enter(&mut storage, || {
            let call = ZoneCall {
                data: input.data,
                selector,
                caller: input.caller,
                is_direct,
            };
            let result = match rules.check_call(call) {
                CallCheck::Continue => execute(input.data, input.caller),
                CallCheck::Return(result) => add_input_cost(input.data, result),
            };
            apply_fixed_gas(result, fixed_gas)
        })
    })
}

/// Create an upstream Tempo precompile with zone rules and finalized L1 state.
///
/// The helper rejects indirect calls, applies pre-execution rules and gas overrides,
/// and installs Tempo's normal EVM provider for local state and accounting. It reads
/// the finalized Tempo anchor once, then adds the zone's L1 storage overlay.
///
/// [`CallCheck::Continue`] forwards the original calldata and caller to Tempo. TIP20
/// and TIP403 business logic remains upstream; the zone changes only call admission,
/// gas, and the storage values observed by that implementation.
pub(crate) fn create_l1_backed_precompile<P, Rules, Execute>(
    id: &'static str,
    env: L1BackedPrecompileEnv<P>,
    rules: Rules,
    execute: Execute,
) -> DynPrecompile
where
    P: L1StorageReader,
    Rules: CallRules,
    Execute: Fn(&[u8], Address) -> PrecompileResult + 'static,
{
    let spec = env.cfg.spec;
    let amsterdam_eip8037_enabled = env.cfg.enable_amsterdam_eip8037;
    let gas_params = env.cfg.gas_params;
    let actions = env.actions;
    let non_creditable_slots = env.non_creditable_slots;
    let l1_reader = env.l1_reader;

    DynPrecompile::new_stateful(PrecompileId::Custom(id.into()), move |input| {
        if !input.is_direct_call() {
            return Ok(PrecompileOutput::revert(
                0,
                SolError::abi_encode(&DelegateCallNotAllowed {}).into(),
                input.reservoir,
            ));
        }

        let selector = selector_from_calldata(input.data);
        let fixed_gas_amount = rules.fixed_gas(selector);
        if let Some(gas) = fixed_gas_amount
            && input.gas < gas
        {
            return Ok(PrecompileOutput::halt(
                PrecompileHalt::OutOfGas,
                input.reservoir,
            ));
        }

        let mut inner = EvmPrecompileStorageProvider::new(
            input.internals,
            fixed_gas_amount.map_or(input.gas, |_| u64::MAX),
            input.reservoir,
            spec,
            amsterdam_eip8037_enabled,
            input.is_static,
            gas_params.clone(),
        )
        .with_actions(actions.clone())
        .with_non_creditable_slots(non_creditable_slots.clone());

        let l1_block_number = match read_l1_anchor(&mut inner) {
            Ok(block_number) => block_number,
            Err(err) => return storage_error_result(err, &inner),
        };
        let mut storage =
            ZonePrecompileStorageProvider::new(inner, l1_reader.clone(), l1_block_number);

        StorageCtx::enter(&mut storage, || {
            let call = ZoneCall {
                data: input.data,
                selector,
                caller: input.caller,
                is_direct: true,
            };

            let result = match rules.check_call(call) {
                CallCheck::Continue => execute(input.data, input.caller),
                CallCheck::Return(result) => add_input_cost(input.data, result),
            };
            apply_fixed_gas(result, fixed_gas_amount)
        })
    })
}

pub(crate) fn unauthorized_output() -> PrecompileOutput {
    StorageCtx::default().revert_output(tempo_zone_contracts::Unauthorized {}.abi_encode().into())
}

fn add_input_cost(calldata: &[u8], result: PrecompileResult) -> PrecompileResult {
    let mut storage = StorageCtx::default();
    let gas_before = storage.gas_used();
    if let Some(err) = charge_input_cost(&mut storage, calldata) {
        return err;
    }
    let input_gas = storage.gas_used().saturating_sub(gas_before);

    result.map(|mut output| {
        output.gas_used = output.gas_used.saturating_add(input_gas);
        output
    })
}

fn apply_fixed_gas(result: PrecompileResult, fixed_gas: Option<u64>) -> PrecompileResult {
    match (result, fixed_gas) {
        (Ok(mut output), Some(gas)) => {
            output.gas_used = gas;
            Ok(output)
        }
        (result, _) => result,
    }
}
