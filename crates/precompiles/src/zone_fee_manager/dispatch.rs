//! ABI dispatch for the [`ZoneFeeManager`] precompile.

use alloy_primitives::Address;
use alloy_sol_types::SolError;
use revm::precompile::PrecompileResult;
use tempo_precompiles::{
    Precompile as TempoPrecompile, charge_input_cost, dispatch, mutate_void, storage::StorageCtx,
    view,
};
use tempo_zone_contracts::{IZoneFeeManager, Unauthorized};

use super::ZoneFeeManager;

impl TempoPrecompile for ZoneFeeManager {
    fn call(&mut self, calldata: &[u8], sender: Address) -> PrecompileResult {
        if let Some(error) = charge_input_cost(&mut self.storage, calldata) {
            return error;
        }

        dispatch!(calldata, |call| match call {
            IZoneFeeManager::IZoneFeeManagerCalls {
                collectedFees(call) => {
                    if sender != call.beneficiary {
                        Ok(StorageCtx.revert_output(Unauthorized {}.abi_encode().into()))
                    } else {
                        view(call, |call| {
                            self.collected_fees(call.beneficiary, call.token)
                        })
                    }
                },
                distributeFees(call) => {
                    if sender != call.beneficiary {
                        Ok(StorageCtx.revert_output(Unauthorized {}.abi_encode().into()))
                    } else {
                        mutate_void(call, sender, |_, call| {
                            self.distribute_fees(call.beneficiary, call.token)
                        })
                    }
                },
            }
        })
    }
}
