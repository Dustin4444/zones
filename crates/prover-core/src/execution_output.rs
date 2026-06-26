use alloc::vec::Vec;

use alloy_primitives::B256;
use tempo_zone_contracts::{BlockTransition, DepositQueueTransition};
use zone_primitives::ZoneHeader;

use crate::{
    BatchOutput, DepositQueueState, LastBatchCommitment, PreparedStatelessExecution, ProverError,
};

pub trait StatelessZoneExecutor {
    fn execute(
        &mut self,
        prepared: &PreparedStatelessExecution,
    ) -> Result<StatelessExecutionOutput, ProverError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatelessExecutionOutput {
    pub blocks: Vec<ExecutedZoneBlock>,
    pub final_commitments: ExecutedBatchCommitments,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutedZoneBlock {
    pub header: ZoneHeader,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutedBatchCommitments {
    pub final_deposit_queue: DepositQueueState,
    pub last_batch: LastBatchCommitment,
    pub final_tempo: TempoExecutionCommitment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TempoExecutionCommitment {
    pub block_number: u64,
    pub block_hash: B256,
    pub state_root: B256,
}

pub fn prove_zone_batch_with_executor(
    witness: crate::BatchWitness,
    executor: &mut impl StatelessZoneExecutor,
) -> Result<BatchOutput, ProverError> {
    let prepared = crate::prepare_stateless_execution(&witness)?;
    let execution = executor.execute(&prepared)?;
    batch_output_from_execution(&prepared, &execution)
}

pub fn batch_output_from_execution(
    prepared: &PreparedStatelessExecution,
    execution: &StatelessExecutionOutput,
) -> Result<BatchOutput, ProverError> {
    let expected_blocks = prepared.zone_blocks.len();
    let actual_blocks = execution.blocks.len();
    if actual_blocks != expected_blocks {
        return Err(ProverError::ExecutionBlockCountMismatch {
            expected: expected_blocks,
            actual: actual_blocks,
        });
    }

    let mut previous_hash = prepared.public_inputs.prev_block_hash;
    for (index, (expected, executed)) in prepared
        .zone_blocks
        .iter()
        .zip(execution.blocks.iter())
        .enumerate()
    {
        let header = &executed.header;
        if header.parent_hash != previous_hash {
            return Err(ProverError::ExecutionBlockParentHashMismatch {
                index,
                expected: previous_hash,
                actual: header.parent_hash,
            });
        }
        if header.number != expected.number {
            return Err(ProverError::ExecutionBlockNumberMismatch {
                index,
                expected: expected.number,
                actual: header.number,
            });
        }
        if header.timestamp != expected.timestamp {
            return Err(ProverError::ExecutionBlockTimestampMismatch {
                index,
                expected: expected.timestamp,
                actual: header.timestamp,
            });
        }
        if header.beneficiary != expected.beneficiary {
            return Err(ProverError::ExecutionBlockBeneficiaryMismatch {
                index,
                expected: expected.beneficiary,
                actual: header.beneficiary,
            });
        }
        if header.protocol_version != expected.protocol_version {
            return Err(ProverError::ExecutionBlockProtocolVersionMismatch {
                index,
                expected: expected.protocol_version,
                actual: header.protocol_version,
            });
        }

        previous_hash = header.hash();
    }

    let final_tempo = execution.final_commitments.final_tempo;
    if final_tempo.block_number != prepared.commitments.final_tempo_block_number
        || final_tempo.block_hash != prepared.commitments.final_tempo_block_hash
        || final_tempo.state_root != prepared.commitments.final_tempo_state_root
    {
        return Err(ProverError::ExecutionFinalTempoMismatch {
            expected: TempoExecutionCommitment {
                block_number: prepared.commitments.final_tempo_block_number,
                block_hash: prepared.commitments.final_tempo_block_hash,
                state_root: prepared.commitments.final_tempo_state_root,
            },
            actual: final_tempo,
        });
    }

    let expected_withdrawal_batch_index = prepared.public_inputs.expected_withdrawal_batch_index;
    if execution
        .final_commitments
        .last_batch
        .withdrawal_batch_index
        != expected_withdrawal_batch_index
    {
        return Err(ProverError::ExecutionWithdrawalBatchIndexMismatch {
            expected: expected_withdrawal_batch_index,
            actual: execution
                .final_commitments
                .last_batch
                .withdrawal_batch_index,
        });
    }

    Ok(BatchOutput {
        block_transition: BlockTransition {
            prevBlockHash: prepared.public_inputs.prev_block_hash,
            nextBlockHash: previous_hash,
        },
        deposit_queue_transition: DepositQueueTransition {
            prevProcessedHash: prepared.commitments.initial_deposit_queue.processed_hash,
            nextProcessedHash: execution
                .final_commitments
                .final_deposit_queue
                .processed_hash,
            prevDepositNumber: prepared.commitments.initial_deposit_queue.processed_number,
            nextDepositNumber: execution
                .final_commitments
                .final_deposit_queue
                .processed_number,
        },
        withdrawal_queue_hash: execution.final_commitments.last_batch.withdrawal_queue_hash,
        last_batch_commitment: execution.final_commitments.last_batch,
    })
}
