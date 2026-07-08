//! ABI dispatch for the [`ZoneOutbox`] precompile.

use alloy_primitives::Address;
use revm::precompile::PrecompileResult;
use tempo_precompiles::{Precompile, charge_input_cost, dispatch};
use tempo_zone_contracts::{IZoneOutbox, ZoneOutbox as ZoneOutboxAbi, ZoneOutboxLegacy};

use super::ZoneOutbox;

impl Precompile for ZoneOutbox {
    fn call(&mut self, calldata: &[u8], _msg_sender: Address) -> PrecompileResult {
        if let Some(err) = charge_input_cost(&mut self.storage, calldata) {
            return err;
        }

        dispatch!(
            calldata,
            |call| match call {
                IZoneOutbox::IZoneOutboxCalls {
                    config(_) => todo!(),
                    tempoGasRate(_) => todo!(),
                    maxWithdrawalsPerBlock(_) => todo!(),
                    lastBatch(_) => todo!(),
                    withdrawalBatchIndex(_) => todo!(),
                    lastFinalizedTimestamp(_) => todo!(),
                    nextWithdrawalIndex(_) => todo!(),
                    pendingWithdrawalsCount(_) => todo!(),
                    getPendingWithdrawals(_) => todo!(),
                    calculateWithdrawalFee(_) => todo!(),
                    MAX_CALLBACK_DATA_SIZE(_) => todo!(),
                    MAX_WITHDRAWAL_GAS_LIMIT(_) => todo!(),
                    MAX_GAS_FEE_RATE(_) => todo!(),
                    WITHDRAWAL_BASE_GAS(_) => todo!(),
                    REVEAL_TO_KEY_LENGTH(_) => todo!(),
                    AUTHENTICATED_WITHDRAWAL_CIPHERTEXT_LENGTH(_) => todo!(),
                    setTempoGasRate(_) => todo!(),
                    setMaxWithdrawalsPerBlock(_) => todo!(),
                    requestWithdrawal(_) => todo!(),
                    enqueueDepositBounceBack(_) => todo!(),
                    finalizeWithdrawalBatch(_) => todo!(),
                }
                ZoneOutboxLegacy::ZoneOutboxLegacyCalls {
                    requestWithdrawal(_) => todo!(),
                }
            },
        )
    }
}
