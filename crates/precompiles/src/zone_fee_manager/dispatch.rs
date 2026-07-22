//! ABI dispatch for the [`ZoneFeeManager`] precompile.

use alloy_primitives::Address;
use revm::precompile::PrecompileResult;
use tempo_precompiles::{
    Precompile as TempoPrecompile, charge_input_cost, dispatch::unknown_selector_result,
};

use super::ZoneFeeManager;

impl TempoPrecompile for ZoneFeeManager {
    fn call(&mut self, calldata: &[u8], _sender: Address) -> PrecompileResult {
        if let Some(error) = charge_input_cost(&mut self.storage, calldata) {
            return error;
        }

        unknown_selector_result(calldata)
    }
}
