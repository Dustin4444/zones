use alloc::vec::Vec;

use alloy_consensus::{
    TxReceipt,
    proofs::{calculate_receipt_root, calculate_transaction_root},
};
use alloy_eips::eip2718::Encodable2718;
use alloy_evm::block::BlockExecutionResult;
use alloy_primitives::{Address, B256, U256};
use revm_database::{BundleState, states::bundle_state::BundleRetention};
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
    BatchOutput, CalculatedStateRoot, DepositQueueState, ExecutionPostState, LastBatchCommitment,
    PlannedZoneTransaction, PlannedZoneTransactionKind, PreparedStatelessExecution,
    PreparedZoneBlock, ProverError, RecoveredTempoTx, SparseStateRootCalculator,
    WitnessTempoStateReader, ZoneBlockExecutionContext, ZoneEvmEnv, ZoneExecutionState, ZoneTxEnv,
    execution_post_state_from_state,
};

pub trait StatelessZoneExecutor {
    fn execute(
        &mut self,
        prepared: &PreparedStatelessExecution,
    ) -> Result<StatelessExecutionOutput, ProverError>;
}

pub trait StatelessZoneBlockExecutor {
    type Transaction: Encodable2718;
    type Receipt: TxReceipt;

    /// Execute one prepared Zone block and leave the block's revm transitions
    /// pending in `state`. The prover driver merges those transitions only
    /// after it derives the block state root from the sparse trie witness.
    fn execute_block(
        &mut self,
        state: &mut ZoneExecutionState,
        input: ZoneBlockExecutionInput<'_>,
    ) -> Result<ExecutedZoneBlockArtifacts<Self::Transaction, Self::Receipt>, ProverError>;
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
    state_root: B256,
    transactions_root: B256,
    receipts_root: B256,
}

impl ExecutedZoneBlock {
    pub fn from_alloy_block_execution<Tx, Receipt>(
        block_index: usize,
        state_root: CalculatedStateRoot,
        transactions: &[Tx],
        result: &BlockExecutionResult<Receipt>,
    ) -> Result<Self, ProverError>
    where
        Tx: Encodable2718,
        Receipt: TxReceipt,
        for<'receipt> alloy_consensus::ReceiptWithBloom<&'receipt Receipt>: Encodable2718,
    {
        let expected = transactions.len();
        let actual = result.receipts.len();
        if actual != expected {
            return Err(ProverError::ExecutionReceiptCountMismatch {
                block_index,
                expected,
                actual,
            });
        }

        Ok(Self {
            state_root: state_root.get(),
            transactions_root: calculate_transaction_root(transactions),
            receipts_root: calculate_receipt_root(
                &result
                    .receipts
                    .iter()
                    .map(TxReceipt::with_bloom_ref)
                    .collect::<Vec<_>>(),
            ),
        })
    }

    pub const fn state_root(&self) -> B256 {
        self.state_root
    }

    pub const fn transactions_root(&self) -> B256 {
        self.transactions_root
    }

    pub const fn receipts_root(&self) -> B256 {
        self.receipts_root
    }

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

#[derive(Debug, Clone)]
pub struct ExecutedZoneBlockArtifacts<Tx, Receipt> {
    pub transactions: Vec<Tx>,
    pub result: BlockExecutionResult<Receipt>,
}

#[derive(Debug, Clone, Copy)]
pub struct ZoneBlockExecutionArtifacts<'a, Tx, Receipt> {
    pub state_root: CalculatedStateRoot,
    pub transactions: &'a [Tx],
    pub result: &'a BlockExecutionResult<Receipt>,
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

impl StatelessExecutionOutput {
    pub fn from_alloy_execution<Tx, Receipt>(
        blocks: &[ZoneBlockExecutionArtifacts<'_, Tx, Receipt>],
        bundle_state: &BundleState,
    ) -> Result<Self, ProverError>
    where
        Tx: Encodable2718,
        Receipt: TxReceipt,
        for<'receipt> alloy_consensus::ReceiptWithBloom<&'receipt Receipt>: Encodable2718,
    {
        let blocks = blocks
            .iter()
            .enumerate()
            .map(|(block_index, block)| {
                ExecutedZoneBlock::from_alloy_block_execution(
                    block_index,
                    block.state_root,
                    block.transactions,
                    block.result,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            blocks,
            post_state: ExecutionPostState::from_bundle_state(bundle_state),
        })
    }
}

pub fn prove_zone_batch_with_executor(
    witness: crate::BatchWitness,
    executor: &mut impl StatelessZoneExecutor,
) -> Result<BatchOutput, ProverError> {
    let prepared = crate::prepare_stateless_execution(&witness)?;
    let execution = executor.execute(&prepared)?;
    batch_output_from_execution(&prepared, &execution)
}

pub fn execute_prepared_blocks<E>(
    prepared: &PreparedStatelessExecution,
    executor: &mut E,
    state_root_calculator: &mut SparseStateRootCalculator,
) -> Result<StatelessExecutionOutput, ProverError>
where
    E: StatelessZoneBlockExecutor,
    for<'receipt> alloy_consensus::ReceiptWithBloom<&'receipt E::Receipt>: Encodable2718,
{
    let mut blocks = Vec::with_capacity(prepared.zone_blocks.len());
    let mut state = prepared.execution_state();
    for block_index in 0..prepared.zone_blocks.len() {
        let input = prepared.block_execution_input(block_index)?;
        let artifacts = executor.execute_block(&mut state, input)?;
        let block_post_state = execution_post_state_from_pending_transitions(&state);
        let state_root = state_root_calculator.calculate(block_post_state.into_hashed())?;
        state.merge_transitions(BundleRetention::PlainState);
        blocks.push(ExecutedZoneBlock::from_alloy_block_execution(
            block_index,
            state_root,
            &artifacts.transactions,
            &artifacts.result,
        )?);
    }

    Ok(StatelessExecutionOutput {
        blocks,
        post_state: execution_post_state_from_state(state),
    })
}

fn execution_post_state_from_pending_transitions(state: &ZoneExecutionState) -> ExecutionPostState {
    let Some(transition_state) = state.transition_state.as_ref() else {
        return ExecutionPostState::default();
    };

    let mut bundle = BundleState::default();
    bundle.apply_transitions_and_create_reverts(
        transition_state.clone(),
        BundleRetention::PlainState,
    );
    ExecutionPostState::from_bundle_state(&bundle)
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
