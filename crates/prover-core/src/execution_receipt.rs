use alloy_evm::{
    Evm,
    eth::receipt_builder::{ReceiptBuilder, ReceiptBuilderCtx},
};
use tempo_primitives::{TempoReceipt, TempoTxEnvelope, TempoTxType};

/// no_std Tempo receipt builder for Zone stateless execution.
///
/// This is the prover-core adaptation of upstream `tempo-evm`'s receipt
/// builder. It keeps receipt construction on Alloy's block-executor path while
/// avoiding the std-bound Tempo EVM crate in prover-core.
#[derive(Debug, Default, Clone, Copy)]
pub struct ZoneTempoReceiptBuilder;

impl ReceiptBuilder for ZoneTempoReceiptBuilder {
    type Transaction = TempoTxEnvelope;
    type Receipt = TempoReceipt;

    fn build_receipt<E: Evm>(&self, ctx: ReceiptBuilderCtx<'_, TempoTxType, E>) -> Self::Receipt {
        TempoReceipt {
            tx_type: ctx.tx_type,
            success: ctx.result.is_success(),
            cumulative_gas_used: ctx.cumulative_gas_used,
            logs: ctx.result.into_logs(),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy_evm::{
        EvmEnv, EvmFactory,
        eth::{EthEvmFactory, receipt_builder::ReceiptBuilderCtx},
    };
    use revm::{
        context::{
            BlockEnv, CfgEnv,
            result::{ExecutionResult, Output, ResultGas, SuccessReason},
        },
        database_interface::EmptyDB,
        state::EvmState,
    };
    use tempo_primitives::TempoTxType;

    use super::*;

    #[test]
    fn builds_tempo_success_receipt_from_execution_result() {
        let evm = eth_evm();
        let state = EvmState::default();
        let result = ExecutionResult::Success {
            reason: SuccessReason::Stop,
            gas: ResultGas::new_with_state_gas(21_000, 0, 0, 0),
            logs: Vec::new(),
            output: Output::Call(Default::default()),
        };

        let receipt = ZoneTempoReceiptBuilder.build_receipt(ReceiptBuilderCtx {
            tx_type: TempoTxType::AA,
            evm: &evm,
            result,
            state: &state,
            cumulative_gas_used: 21_000,
        });

        assert_eq!(receipt.tx_type, TempoTxType::AA);
        assert!(receipt.success);
        assert_eq!(receipt.cumulative_gas_used, 21_000);
        assert!(receipt.logs.is_empty());
    }

    #[test]
    fn builds_tempo_revert_receipt_from_execution_result() {
        let evm = eth_evm();
        let state = EvmState::default();
        let result = ExecutionResult::Revert {
            gas: ResultGas::new_with_state_gas(30_000, 0, 0, 0),
            logs: Vec::new(),
            output: Default::default(),
        };

        let receipt = ZoneTempoReceiptBuilder.build_receipt(ReceiptBuilderCtx {
            tx_type: TempoTxType::Legacy,
            evm: &evm,
            result,
            state: &state,
            cumulative_gas_used: 30_000,
        });

        assert_eq!(receipt.tx_type, TempoTxType::Legacy);
        assert!(!receipt.success);
        assert_eq!(receipt.cumulative_gas_used, 30_000);
        assert!(receipt.logs.is_empty());
    }

    fn eth_evm() -> <EthEvmFactory as EvmFactory>::Evm<EmptyDB, revm::inspector::NoOpInspector> {
        EthEvmFactory::default().create_evm(
            EmptyDB::default(),
            EvmEnv {
                cfg_env: CfgEnv::default(),
                block_env: BlockEnv::default(),
            },
        )
    }
}
