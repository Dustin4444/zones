use alloy_consensus::{Transaction, TransactionEnvelope};
use alloy_eips::Encodable2718;
use alloy_evm::{
    Evm, FromRecoveredTx, FromTxWithEncoded,
    block::{
        BlockExecutionError, BlockExecutionResult, BlockExecutor, ExecutableTx, GasOutput, StateDB,
    },
    eth::{EthBlockExecutionCtx, EthBlockExecutor, EthTxResult, spec::EthExecutorSpec},
};
use alloy_primitives::Log;
use tempo_primitives::{TempoReceipt, TempoTxEnvelope, TempoTxType};

use crate::ZoneTempoReceiptBuilder;

/// no_std Zone block executor built on Alloy's Ethereum block executor.
///
/// Zone execution keeps Ethereum block semantics as the base and specializes the
/// consensus transaction and receipt types to Tempo. A concrete provider still
/// supplies the EVM and registered Zone precompiles; this wrapper delegates
/// transaction execution, receipt construction, gas accounting, system calls,
/// and post-block state handling to Alloy.
#[derive(Debug)]
pub struct ZoneAlloyBlockExecutor<'a, E, Spec> {
    inner: EthBlockExecutor<'a, E, Spec, ZoneTempoReceiptBuilder>,
}

impl<'a, E, Spec> ZoneAlloyBlockExecutor<'a, E, Spec>
where
    Spec: Clone,
{
    pub fn new(evm: E, ctx: EthBlockExecutionCtx<'a>, spec: Spec) -> Self {
        Self {
            inner: EthBlockExecutor::new(evm, ctx, spec, ZoneTempoReceiptBuilder),
        }
    }

    pub const fn inner(&self) -> &EthBlockExecutor<'a, E, Spec, ZoneTempoReceiptBuilder> {
        &self.inner
    }

    pub fn into_inner(self) -> EthBlockExecutor<'a, E, Spec, ZoneTempoReceiptBuilder> {
        self.inner
    }
}

impl<E, Spec> BlockExecutor for ZoneAlloyBlockExecutor<'_, E, Spec>
where
    E: Evm<DB: StateDB, Tx: FromRecoveredTx<TempoTxEnvelope> + FromTxWithEncoded<TempoTxEnvelope>>,
    Spec: EthExecutorSpec,
    TempoTxEnvelope: Transaction + Encodable2718,
    TempoReceipt: alloy_consensus::TxReceipt<Log = Log>,
    <TempoTxEnvelope as TransactionEnvelope>::TxType: Send + 'static,
{
    type Transaction = TempoTxEnvelope;
    type Receipt = TempoReceipt;
    type Evm = E;
    type Result = EthTxResult<E::HaltReason, TempoTxType>;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.inner.apply_pre_execution_changes()
    }

    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<Self::Result, BlockExecutionError> {
        self.inner.execute_transaction_without_commit(tx)
    }

    fn commit_transaction(&mut self, output: Self::Result) -> GasOutput {
        self.inner.commit_transaction(output)
    }

    fn finish(
        self,
    ) -> Result<(Self::Evm, BlockExecutionResult<Self::Receipt>), BlockExecutionError> {
        self.inner.finish()
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        self.inner.evm_mut()
    }

    fn evm(&self) -> &Self::Evm {
        self.inner.evm()
    }

    fn receipts(&self) -> &[Self::Receipt] {
        self.inner.receipts()
    }
}
