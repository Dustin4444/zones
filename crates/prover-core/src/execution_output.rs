use alloc::vec::Vec;

use alloy_primitives::{Address, B256, U256};
use tempo_zone_contracts::{BlockTransition, DepositQueueTransition};
use zone_primitives::{
    ZoneHeader,
    constants::{
        TEMPO_BLOCK_HASH_SLOT, TEMPO_PACKED_SLOT, TEMPO_STATE_ADDRESS, TEMPO_STATE_ROOT_SLOT,
        ZONE_INBOX_ADDRESS, ZONE_INBOX_PROCESSED_HASH_SLOT, ZONE_INBOX_PROCESSED_NUMBER_SLOT,
        ZONE_OUTBOX_ADDRESS, ZONE_OUTBOX_LAST_BATCH_HASH_SLOT, ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT,
    },
};

use crate::{
    BatchOutput, DepositQueueState, ExecutionPostState, LastBatchCommitment,
    PlannedZoneTransaction, PlannedZoneTransactionKind, PreparedStatelessExecution,
    PreparedZoneBlock, ProverError, RecoveredTempoTx, WitnessTempoStateReader,
    ZoneBlockExecutionContext, ZoneEvmEnv, ZoneTxEnv,
};

pub trait StatelessZoneExecutor {
    fn execute(
        &mut self,
        prepared: &PreparedStatelessExecution,
    ) -> Result<StatelessExecutionOutput, ProverError>;
}

pub trait StatelessZoneBlockExecutor {
    fn execute_block(
        &mut self,
        input: ZoneBlockExecutionInput<'_>,
    ) -> Result<ExecutedZoneBlock, ProverError>;

    fn finish(&mut self) -> Result<ExecutionPostState, ProverError>;
}

#[derive(Debug, Clone)]
pub struct ZoneBlockExecutionInput<'a> {
    pub block_index: usize,
    pub block: &'a PreparedZoneBlock,
    pub evm_env: ZoneEvmEnv,
    pub execution_context: ZoneBlockExecutionContext,
    pub transactions: Vec<ZoneExecutableTransaction<'a>>,
    pub tempo_state_reader: WitnessTempoStateReader<'a>,
}

#[derive(Debug, Clone)]
pub struct ZoneExecutableTransaction<'a> {
    pub kind: PlannedZoneTransactionKind,
    pub tx_env: ZoneTxEnv,
    pub recovered: &'a RecoveredTempoTx,
}

impl<'a> ZoneExecutableTransaction<'a> {
    pub fn from_planned(planned: &'a PlannedZoneTransaction) -> Self {
        Self {
            kind: planned.kind,
            tx_env: planned.tx_env(),
            recovered: &planned.tx,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatelessExecutionOutput {
    pub blocks: Vec<ExecutedZoneBlock>,
    pub post_state: ExecutionPostState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutedZoneBlock {
    pub state_root: B256,
    pub transactions_root: B256,
    pub receipts_root: B256,
}

impl ExecutedZoneBlock {
    pub fn header(&self, parent_hash: B256, block: &PreparedZoneBlock) -> ZoneHeader {
        ZoneHeader {
            parent_hash,
            beneficiary: block.beneficiary,
            state_root: self.state_root,
            transactions_root: self.transactions_root,
            receipts_root: self.receipts_root,
            number: block.number,
            timestamp: block.timestamp,
            protocol_version: block.protocol_version,
        }
    }
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

pub fn execute_prepared_blocks(
    prepared: &PreparedStatelessExecution,
    executor: &mut impl StatelessZoneBlockExecutor,
) -> Result<StatelessExecutionOutput, ProverError> {
    let mut blocks = Vec::with_capacity(prepared.zone_blocks.len());
    for block_index in 0..prepared.zone_blocks.len() {
        let input = prepared.block_execution_input(block_index)?;
        blocks.push(executor.execute_block(input)?);
    }

    Ok(StatelessExecutionOutput {
        blocks,
        post_state: executor.finish()?,
    })
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
        if expected.parent_hash != previous_hash {
            return Err(ProverError::ExecutionBlockParentHashMismatch {
                index,
                expected: previous_hash,
                actual: expected.parent_hash,
            });
        }

        let header = executed.header(previous_hash, expected);
        previous_hash = header.hash();
    }

    let final_commitments = execution_commitments_from_post_state(prepared, &execution.post_state);

    let final_tempo = final_commitments.final_tempo;
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
    if final_commitments.last_batch.withdrawal_batch_index != expected_withdrawal_batch_index {
        return Err(ProverError::ExecutionWithdrawalBatchIndexMismatch {
            expected: expected_withdrawal_batch_index,
            actual: final_commitments.last_batch.withdrawal_batch_index,
        });
    }

    Ok(BatchOutput {
        block_transition: BlockTransition {
            prevBlockHash: prepared.public_inputs.prev_block_hash,
            nextBlockHash: previous_hash,
        },
        deposit_queue_transition: DepositQueueTransition {
            prevProcessedHash: prepared.commitments.initial_deposit_queue.processed_hash,
            nextProcessedHash: final_commitments.final_deposit_queue.processed_hash,
            prevDepositNumber: prepared.commitments.initial_deposit_queue.processed_number,
            nextDepositNumber: final_commitments.final_deposit_queue.processed_number,
        },
        withdrawal_queue_hash: final_commitments.last_batch.withdrawal_queue_hash,
        last_batch_commitment: final_commitments.last_batch,
    })
}

pub fn execution_commitments_from_post_state(
    prepared: &PreparedStatelessExecution,
    post_state: &ExecutionPostState,
) -> ExecutedBatchCommitments {
    ExecutedBatchCommitments {
        final_deposit_queue: final_deposit_queue_state(prepared, post_state),
        last_batch: final_last_batch_commitment(prepared, post_state),
        final_tempo: final_tempo_commitment(prepared, post_state),
    }
}

fn final_deposit_queue_state(
    prepared: &PreparedStatelessExecution,
    post_state: &ExecutionPostState,
) -> DepositQueueState {
    let processed_hash = final_storage_word(
        post_state,
        ZONE_INBOX_ADDRESS,
        ZONE_INBOX_PROCESSED_HASH_SLOT,
        word_from_b256(prepared.commitments.initial_deposit_queue.processed_hash),
    );
    let processed_number = final_storage_word(
        post_state,
        ZONE_INBOX_ADDRESS,
        ZONE_INBOX_PROCESSED_NUMBER_SLOT,
        U256::from(prepared.commitments.initial_deposit_queue.processed_number),
    );

    DepositQueueState {
        processed_hash: B256::from(processed_hash),
        processed_number: low_u64(processed_number),
    }
}

fn final_last_batch_commitment(
    prepared: &PreparedStatelessExecution,
    post_state: &ExecutionPostState,
) -> LastBatchCommitment {
    let withdrawal_queue_hash = final_storage_word(
        post_state,
        ZONE_OUTBOX_ADDRESS,
        ZONE_OUTBOX_LAST_BATCH_HASH_SLOT,
        word_from_b256(
            prepared
                .commitments
                .previous_last_batch
                .withdrawal_queue_hash,
        ),
    );
    let withdrawal_batch_index = final_storage_word(
        post_state,
        ZONE_OUTBOX_ADDRESS,
        ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT,
        U256::from(
            prepared
                .commitments
                .previous_last_batch
                .withdrawal_batch_index,
        ),
    );

    LastBatchCommitment {
        withdrawal_queue_hash: B256::from(withdrawal_queue_hash),
        withdrawal_batch_index: low_u64(withdrawal_batch_index),
    }
}

fn final_tempo_commitment(
    prepared: &PreparedStatelessExecution,
    post_state: &ExecutionPostState,
) -> TempoExecutionCommitment {
    let block_hash = final_storage_word(
        post_state,
        TEMPO_STATE_ADDRESS,
        storage_slot_u256(TEMPO_BLOCK_HASH_SLOT),
        word_from_b256(prepared.commitments.initial_tempo_block_hash),
    );
    let state_root = final_storage_word(
        post_state,
        TEMPO_STATE_ADDRESS,
        storage_slot_u256(TEMPO_STATE_ROOT_SLOT),
        word_from_b256(prepared.commitments.initial_tempo_state_root),
    );
    let packed = final_storage_word(
        post_state,
        TEMPO_STATE_ADDRESS,
        storage_slot_u256(TEMPO_PACKED_SLOT),
        U256::from(prepared.commitments.initial_tempo_block_number),
    );

    TempoExecutionCommitment {
        block_number: low_u64(packed),
        block_hash: B256::from(block_hash),
        state_root: B256::from(state_root),
    }
}

fn final_storage_word(
    post_state: &ExecutionPostState,
    account: Address,
    slot: U256,
    initial: U256,
) -> U256 {
    post_state.storage(account, slot).unwrap_or(initial)
}

fn storage_slot_u256(slot: B256) -> U256 {
    slot.into()
}

fn word_from_b256(value: B256) -> U256 {
    U256::from_be_bytes(value.0)
}

fn low_u64(value: U256) -> u64 {
    (value & U256::from(u64::MAX)).to::<u64>()
}
