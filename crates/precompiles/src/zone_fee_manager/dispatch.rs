//! ABI dispatch for the [`ZoneFeeManager`] precompile.

use alloy_primitives::Address;
use alloy_sol_types::SolError;
use revm::precompile::PrecompileResult;
use tempo_contracts::precompiles::IFeeManager;
use tempo_precompiles::{
    Precompile as TempoPrecompile, charge_input_cost, dispatch, dispatch::unknown_selector_result,
    mutate_void, storage::StorageCtx, view,
};
use tempo_zone_contracts::Unauthorized;

use super::ZoneFeeManager;

impl TempoPrecompile for ZoneFeeManager {
    fn call(&mut self, calldata: &[u8], sender: Address) -> PrecompileResult {
        if let Some(error) = charge_input_cost(&mut self.storage, calldata) {
            return error;
        }

        dispatch!(calldata, |call| match call {
            IFeeManager::IFeeManagerCalls {
                userTokens(_) => unknown_selector_result(calldata),
                validatorTokens(_) => unknown_selector_result(calldata),
                setUserToken(_) => unknown_selector_result(calldata),
                setValidatorToken(_) => unknown_selector_result(calldata),
                collectedFees(call) => {
                    if sender != call.validator {
                        Ok(StorageCtx.revert_output(Unauthorized {}.abi_encode().into()))
                    } else {
                        view(call, |call| {
                            self.collected_fees(call.validator, call.token)
                        })
                    }
                },
                distributeFees(call) => {
                    if sender != call.validator {
                        Ok(StorageCtx.revert_output(Unauthorized {}.abi_encode().into()))
                    } else {
                        mutate_void(call, sender, |_, call| {
                            self.distribute_fees(call.validator, call.token)
                        })
                    }
                },
            }
        })
    }
}
