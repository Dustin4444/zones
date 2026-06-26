//! Portable Zone batch prover core.
//!
//! This crate is the backend-agnostic state transition entry point. It is
//! intentionally `no_std` compatible: the Nitro enclave proof server and local
//! native-signature proof path should serialize inputs, call
//! [`prove_zone_batch`], and wrap the resulting [`BatchOutput`] without
//! embedding backend-specific behavior in this crate.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use alloc::{collections::BTreeMap, vec::Vec};
use core::fmt;

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_rlp::Decodable;
use alloy_sol_types::SolValue;
use alloy_trie::EMPTY_ROOT_HASH;
use tempo_primitives::TempoHeader;
use tempo_zone_contracts::{
    BlockTransition, DecryptionData, DepositQueueTransition, EnabledToken, QueuedDeposit,
};
use zone_primitives::{
    ZoneHeader,
    constants::{
        TEMPO_BLOCK_HASH_SLOT, TEMPO_PACKED_SLOT, TEMPO_STATE_ADDRESS, TEMPO_STATE_ROOT_SLOT,
        ZONE_INBOX_ADDRESS, ZONE_INBOX_PROCESSED_HASH_SLOT, ZONE_INBOX_PROCESSED_NUMBER_SLOT,
        ZONE_OUTBOX_ADDRESS, ZONE_OUTBOX_LAST_BATCH_HASH_SLOT, ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT,
    },
};

mod ancestry;
mod execution_block;
mod execution_env;
mod execution_output;
mod execution_plan;
mod execution_receipt;
mod execution_state;
mod execution_tx;
mod post_state;
mod state_root;
mod tempo;
mod tempo_reader;
mod trie;
mod witness_db;

pub use execution_block::ZoneAlloyBlockExecutor;
pub use execution_env::{
    BlobExcessGasAndPrice, ZoneBlockEnv, ZoneBlockEnvConfig, ZoneBlockExecutionContext,
    ZoneBlockExecutionContextConfig, ZoneCfgEnv, ZoneCfgEnvConfig, ZoneEvmEnv, tempo_gas_params,
    tempo_gas_params_with_amsterdam, zone_general_gas_limit, zone_shared_gas_limit,
};
pub use execution_output::{
    AlloyZoneBlockExecutor, AlloyZoneBlockExecutorProvider, ExecutedBatchCommitments,
    ExecutedZoneBlock, StatelessExecutionOutput, StatelessZoneBlockExecutor,
    TempoExecutionCommitment, ZoneBlockExecutionArtifacts, ZoneBlockExecutionInput,
    ZoneExecutableTransaction, batch_output_from_execution, execute_prepared_blocks,
    execution_commitments_from_post_state, prove_zone_batch_with_executor,
};
pub use execution_plan::{
    PlannedZoneTransaction, PlannedZoneTransactionKind, RecoveredTempoTx, ZoneBlockExecutionPlan,
    ZoneExecutionPlan,
};
pub use execution_receipt::ZoneTempoReceiptBuilder;
pub use execution_state::{
    ZoneExecutionState, execution_post_state_from_state, zone_execution_state,
};
pub use execution_tx::{ZoneBatchCallEnv, ZoneInvalidTransaction, ZoneTxEnv};
pub use post_state::ExecutionPostState;
pub use state_root::{
    CalculatedStateRoot, SparseStateRootCalculator, calculate_state_root, empty_state_root,
};
pub use tempo_chainspec::hardfork::TempoHardfork;
pub use tempo_reader::{
    TEMPO_STATE_READER_BASE_GAS, TEMPO_STATE_READER_PER_SLOT_GAS, TempoStateReaderCallResult,
    WitnessTempoStateReader, tempo_state_reader_gas,
};
pub use witness_db::{WitnessDatabase, WitnessDbError};

/// Ethereum's canonical empty trie root.
pub const EMPTY_TRIE_ROOT: B256 = EMPTY_ROOT_HASH;

/// Prover-side public inputs that must match the values passed by `ZonePortal`
/// into `IVerifier.verify(...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct PublicInputs {
    pub prev_block_hash: B256,
    pub tempo_block_number: u64,
    pub anchor_block_number: u64,
    pub anchor_block_hash: B256,
    pub expected_withdrawal_batch_index: u64,
    pub sequencer: Address,
}

/// Top-level witness consumed by [`prove_zone_batch`].
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct BatchWitness {
    pub public_inputs: PublicInputs,
    pub prev_block_header: ZoneHeader,
    pub zone_ancestry_headers: Vec<Bytes>,
    pub zone_blocks: Vec<ZoneBlock>,
    pub initial_zone_state: ZoneStateWitness,
    pub tempo_state_proofs: BatchStateProof,
    pub tempo_ancestry_headers: Vec<Bytes>,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct ZoneBlock {
    pub number: u64,
    pub parent_hash: B256,
    pub timestamp: u64,
    pub beneficiary: Address,
    pub protocol_version: u64,
    pub cfg_env: ZoneCfgEnvWitness,
    pub execution_context: ZoneBlockExecutionContextWitness,
    pub block_env: ZoneBlockEnvWitness,
    pub tempo_header_rlp: Option<Bytes>,
    pub deposits: Vec<QueuedDeposit>,
    pub decryptions: Vec<DecryptionData>,
    pub enabled_tokens: Vec<EnabledToken>,
    pub finalize_withdrawal_batch_count: Option<U256>,
    pub finalize_withdrawal_encrypted_senders: Vec<Bytes>,
    /// Raw transaction bytes. Full EVM re-execution is not yet implemented; any
    /// non-empty transaction list is rejected by the current prover core.
    pub transactions: Vec<Bytes>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct ZoneBlockExecutionContextWitness {
    pub parent_beacon_block_root: Option<B256>,
    pub extra_data: Bytes,
}

impl ZoneBlockExecutionContextWitness {
    pub fn config(&self) -> ZoneBlockExecutionContextConfig {
        ZoneBlockExecutionContextConfig {
            parent_beacon_block_root: self.parent_beacon_block_root,
            extra_data: self.extra_data.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct ZoneCfgEnvWitness {
    pub chain_id: u64,
    pub spec: TempoHardfork,
    pub enable_amsterdam_eip8037: bool,
}

impl ZoneCfgEnvWitness {
    pub fn config(self) -> ZoneCfgEnvConfig {
        ZoneCfgEnvConfig {
            chain_id: self.chain_id,
            spec: self.spec,
            enable_amsterdam_eip8037: self.enable_amsterdam_eip8037,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct ZoneBlockEnvWitness {
    pub gas_limit: u64,
    pub basefee: u64,
    pub difficulty: U256,
    pub prevrandao: Option<B256>,
    pub slot_num: u64,
    pub timestamp_millis_part: u64,
}

impl ZoneBlockEnvWitness {
    pub fn config(self) -> ZoneBlockEnvConfig {
        ZoneBlockEnvConfig {
            gas_limit: self.gas_limit,
            basefee: self.basefee,
            difficulty: self.difficulty,
            prevrandao: self.prevrandao,
            blob_excess_gas_and_price: None,
            slot_num: self.slot_num,
            timestamp_millis_part: self.timestamp_millis_part,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct ZoneStateWitness {
    pub state_root: B256,
    pub node_pool: BTreeMap<B256, Bytes>,
    pub account_reads: Vec<ZoneAccountRead>,
    pub storage_reads: Vec<ZoneStorageRead>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct ZoneAccountRead {
    pub account: Address,
    pub nonce: u64,
    pub balance: U256,
    pub storage_root: B256,
    pub code_hash: B256,
    pub code: Option<Bytes>,
    pub proof_node_hashes: Vec<B256>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct ZoneStorageRead {
    pub account: Address,
    pub slot: U256,
    pub value: U256,
    pub proof_node_hashes: Vec<B256>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct BatchStateProof {
    pub node_pool: BTreeMap<B256, Bytes>,
    pub reads: Vec<L1StateRead>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct L1StateRead {
    pub zone_block_index: u64,
    pub tempo_block_number: u64,
    pub account: Address,
    pub account_nonce: u64,
    pub account_balance: U256,
    pub account_storage_root: B256,
    pub account_code_hash: B256,
    pub account_proof_node_hashes: Vec<B256>,
    pub slot: U256,
    pub value: U256,
    pub storage_proof_node_hashes: Vec<B256>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct LastBatchCommitment {
    pub withdrawal_queue_hash: B256,
    pub withdrawal_batch_index: u64,
}

/// Public outputs returned by the state transition function and submitted to
/// `ZonePortal.submitBatch`.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct BatchOutput {
    pub block_transition: BlockTransition,
    pub deposit_queue_transition: DepositQueueTransition,
    pub withdrawal_queue_hash: B256,
    pub last_batch_commitment: LastBatchCommitment,
}

impl BatchOutput {
    pub fn digest(&self) -> B256 {
        keccak256(
            (
                self.block_transition.prevBlockHash,
                self.block_transition.nextBlockHash,
                self.deposit_queue_transition.prevProcessedHash,
                self.deposit_queue_transition.nextProcessedHash,
                self.deposit_queue_transition.prevDepositNumber,
                self.deposit_queue_transition.nextDepositNumber,
                self.withdrawal_queue_hash,
                self.last_batch_commitment.withdrawal_queue_hash,
                self.last_batch_commitment.withdrawal_batch_index,
            )
                .abi_encode_params(),
        )
    }
}

/// Verified inputs prepared for stateless Zone execution.
///
/// This mirrors the upstream `stateless` flow: verify and materialize witness
/// data first, then pass a strict witness-backed database and recovered
/// transaction plan into the execution engine.
#[derive(Debug, Clone)]
pub struct PreparedStatelessExecution {
    pub public_inputs: PublicInputs,
    pub prev_block_header: ZoneHeader,
    pub zone_blocks: Vec<PreparedZoneBlock>,
    pub initial_zone_state: ZoneStateWitness,
    pub execution_db: WitnessDatabase,
    pub execution_plan: ZoneExecutionPlan,
    pub tempo_witness_provider: TempoWitnessProvider,
    pub commitments: PreparedWitnessCommitments,
}

impl PreparedStatelessExecution {
    pub fn execution_state(&self) -> ZoneExecutionState {
        zone_execution_state(self.execution_db.clone())
    }

    pub fn into_execution_state(self) -> ZoneExecutionState {
        zone_execution_state(self.execution_db)
    }

    pub fn state_root_calculator(&self) -> Result<SparseStateRootCalculator, ProverError> {
        SparseStateRootCalculator::from_zone_state_witness(&self.initial_zone_state)
    }

    pub fn block_execution_input(
        &self,
        block_index: usize,
    ) -> Result<ZoneBlockExecutionInput<'_>, ProverError> {
        if self.execution_plan.blocks.len() != self.zone_blocks.len() {
            return Err(ProverError::ExecutionPlanBlockCountMismatch {
                expected: self.zone_blocks.len(),
                actual: self.execution_plan.blocks.len(),
            });
        }

        let block = self.zone_blocks.get(block_index).ok_or(
            ProverError::TempoReaderBlockIndexOutOfRange {
                zone_block_index: block_index,
                block_count: self.zone_blocks.len(),
            },
        )?;
        let block_env = ZoneBlockEnv::from_prepared_block(block, block.block_env);
        let evm_env = ZoneEvmEnv::new(block.cfg_env.cfg_env(), block_env);
        let transaction_count = self.execution_plan.blocks[block_index].transactions.len();
        let execution_context = block
            .execution_context
            .execution_context(block, transaction_count);
        let transactions = self.execution_plan.blocks[block_index]
            .transactions
            .iter()
            .map(ZoneExecutableTransaction::from_planned)
            .collect();
        let tempo_state_reader = self.tempo_state_reader(block_index)?;

        Ok(ZoneBlockExecutionInput {
            block_index,
            block,
            evm_env,
            execution_context,
            transactions,
            tempo_state_reader,
        })
    }

    pub fn tempo_state_reader(
        &self,
        zone_block_index: usize,
    ) -> Result<WitnessTempoStateReader<'_>, ProverError> {
        if zone_block_index >= self.zone_blocks.len() {
            return Err(ProverError::TempoReaderBlockIndexOutOfRange {
                zone_block_index,
                block_count: self.zone_blocks.len(),
            });
        }

        let zone_block_index = u64::try_from(zone_block_index).map_err(|_| {
            ProverError::TempoReaderBlockIndexOutOfRange {
                zone_block_index,
                block_count: self.zone_blocks.len(),
            }
        })?;

        Ok(WitnessTempoStateReader::new(
            &self.tempo_witness_provider,
            zone_block_index,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedZoneBlock {
    pub number: u64,
    pub parent_hash: B256,
    pub timestamp: u64,
    pub beneficiary: Address,
    pub protocol_version: u64,
    pub cfg_env: ZoneCfgEnvConfig,
    pub execution_context: ZoneBlockExecutionContextConfig,
    pub block_env: ZoneBlockEnvConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedWitnessCommitments {
    pub initial_tempo_block_number: u64,
    pub initial_tempo_block_hash: B256,
    pub initial_tempo_state_root: B256,
    pub final_tempo_block_number: u64,
    pub final_tempo_block_hash: B256,
    pub final_tempo_state_root: B256,
    pub initial_deposit_queue: DepositQueueState,
    pub previous_last_batch: LastBatchCommitment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProverError {
    FullStatelessExecutionUnsupported,
    PrevHeaderHashMismatch,
    InitialStateRootMismatch,
    ZoneStateNodeHashMismatch(B256),
    TempoStateNodeHashMismatch(B256),
    TempoStateReadBlockIndexOutOfRange {
        read_index: usize,
        zone_block_index: u64,
    },
    TempoStateReadTempoBlockMismatch {
        read_index: usize,
        expected: u64,
        actual: u64,
    },
    TempoStateReadUnboundTempoRoot {
        read_index: usize,
        tempo_block_number: u64,
    },
    DuplicateTempoRootBinding {
        root_index: usize,
        tempo_block_number: u64,
    },
    DuplicateTempoStateRead {
        read_index: usize,
    },
    TempoReaderBlockIndexOutOfRange {
        zone_block_index: usize,
        block_count: usize,
    },
    MissingTempoStateRead {
        zone_block_index: u64,
        tempo_block_number: u64,
        account: Address,
        slot: U256,
    },
    TempoStateAccountProofMissing {
        read_index: usize,
        account: Address,
        node_hash: B256,
    },
    TempoStateAccountProofInvalid {
        read_index: usize,
        account: Address,
    },
    TempoStateAccountReadMismatch {
        read_index: usize,
        account: Address,
    },
    TempoStateStorageProofMissing {
        read_index: usize,
        account: Address,
        slot: U256,
        node_hash: B256,
    },
    TempoStateStorageProofInvalid {
        read_index: usize,
        account: Address,
        slot: U256,
    },
    TempoStateStorageReadMismatch {
        read_index: usize,
        account: Address,
        slot: U256,
    },
    AccountCodeHashMismatch(Address),
    MissingAccountCode(Address),
    AccountProofMissing {
        account: Address,
        node_hash: B256,
    },
    AccountProofInvalid(Address),
    AccountRlpInvalid(Address),
    AccountReadMismatch(Address),
    MissingAccountRead(Address),
    StorageProofMissing {
        account: Address,
        slot: U256,
        node_hash: B256,
    },
    StorageProofInvalid {
        account: Address,
        slot: U256,
    },
    StorageRlpInvalid {
        account: Address,
        slot: U256,
    },
    StorageReadMismatch {
        account: Address,
        slot: U256,
    },
    MissingSystemStorageRead {
        account: Address,
        slot: U256,
    },
    MissingTempoBindingRead {
        slot: U256,
    },
    TempoBlockNumberMismatch {
        expected: u64,
        actual: u64,
    },
    TempoAnchorHashMismatch {
        expected: B256,
        actual: B256,
    },
    TempoAncestryLengthMismatch {
        expected: u64,
        actual: u64,
    },
    TempoAncestryTooLong,
    TempoAncestryHeaderInvalid {
        index: usize,
    },
    TempoAncestryBlockNumberOverflow {
        index: usize,
    },
    TempoAncestryParentHashMismatch {
        index: usize,
        expected: B256,
        actual: B256,
    },
    TempoAncestryBlockNumberMismatch {
        index: usize,
        expected: u64,
        actual: u64,
    },
    ExpectedWithdrawalBatchIndexZero,
    WithdrawalBatchIndexMismatch {
        expected_previous: u64,
        actual_previous: u64,
    },
    AnchorBeforeTempo,
    EmptyBatch,
    BlockParentHashMismatch {
        index: usize,
    },
    BlockNumberOverflow {
        index: usize,
    },
    BlockNumberMismatch {
        index: usize,
        expected: u64,
        actual: u64,
    },
    BlockTimestampRegression {
        index: usize,
    },
    BlockTimestampMillisPartOutOfRange {
        index: usize,
        actual: u64,
    },
    BlockBeneficiaryMismatch {
        index: usize,
    },
    IntermediateWithdrawalFinalization {
        index: usize,
    },
    MissingFinalWithdrawalFinalization,
    ZoneAncestryHeaderLimitExceeded {
        count: usize,
        limit: usize,
    },
    ZoneAncestryHeaderInvalid {
        index: usize,
    },
    ZoneAncestryBlockNumberUnderflow {
        index: usize,
    },
    ZoneAncestryParentHashMismatch {
        index: usize,
        expected: B256,
        actual: B256,
    },
    ZoneAncestryBlockNumberMismatch {
        index: usize,
        expected: u64,
        actual: u64,
    },
    UserTransactionDecodeFailed {
        block_index: usize,
        transaction_index: usize,
    },
    UserTransactionSenderRecoveryFailed {
        block_index: usize,
        transaction_index: usize,
    },
    TempoImportHeaderInvalid {
        index: usize,
    },
    TempoImportParentHashMismatch {
        index: usize,
        expected: B256,
        actual: B256,
    },
    TempoImportBlockNumberMismatch {
        index: usize,
        expected: u64,
        actual: u64,
    },
    TempoImportUnsupported {
        index: usize,
    },
    DepositProcessingUnsupported {
        index: usize,
    },
    UserTransactionsUnsupported {
        index: usize,
    },
    NonZeroWithdrawalFinalizationUnsupported,
    TempoStateReaderGasOverflow,
    StateRootCalculationFailed,
    ExecutionBlockCountMismatch {
        expected: usize,
        actual: usize,
    },
    ExecutionPlanBlockCountMismatch {
        expected: usize,
        actual: usize,
    },
    ExecutionReceiptCountMismatch {
        block_index: usize,
        expected: usize,
        actual: usize,
    },
    ExecutionBlockFailed {
        index: usize,
    },
    ExecutionBlockParentHashMismatch {
        index: usize,
        expected: B256,
        actual: B256,
    },
    ExecutionBlockNumberMismatch {
        index: usize,
        expected: u64,
        actual: u64,
    },
    ExecutionBlockTimestampMismatch {
        index: usize,
        expected: u64,
        actual: u64,
    },
    ExecutionBlockBeneficiaryMismatch {
        index: usize,
        expected: Address,
        actual: Address,
    },
    ExecutionBlockProtocolVersionMismatch {
        index: usize,
        expected: u64,
        actual: u64,
    },
    ExecutionFinalTempoMismatch {
        expected: TempoExecutionCommitment,
        actual: TempoExecutionCommitment,
    },
    ExecutionWithdrawalBatchIndexMismatch {
        expected: u64,
        actual: u64,
    },
}

impl fmt::Display for ProverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FullStatelessExecutionUnsupported => f.write_str(
                "full stateless zone execution is not implemented in the production prover",
            ),
            Self::PrevHeaderHashMismatch => {
                f.write_str("previous header hash does not match public prev_block_hash")
            }
            Self::InitialStateRootMismatch => {
                f.write_str("previous header state root does not match initial zone state root")
            }
            Self::ZoneStateNodeHashMismatch(hash) => {
                write!(f, "zone state proof node hash mismatch for {hash}")
            }
            Self::TempoStateNodeHashMismatch(hash) => {
                write!(f, "tempo state proof node hash mismatch for {hash}")
            }
            Self::TempoStateReadBlockIndexOutOfRange {
                read_index,
                zone_block_index,
            } => {
                write!(
                    f,
                    "Tempo state read {read_index} references out-of-range zone block {zone_block_index}"
                )
            }
            Self::TempoStateReadTempoBlockMismatch {
                read_index,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "Tempo state read {read_index} block mismatch: expected {expected}, got {actual}"
                )
            }
            Self::TempoStateReadUnboundTempoRoot {
                read_index,
                tempo_block_number,
            } => {
                write!(
                    f,
                    "Tempo state read {read_index} references unbound Tempo block {tempo_block_number}"
                )
            }
            Self::DuplicateTempoRootBinding {
                root_index,
                tempo_block_number,
            } => {
                write!(
                    f,
                    "Tempo root binding {root_index} duplicates Tempo block {tempo_block_number}"
                )
            }
            Self::DuplicateTempoStateRead { read_index } => {
                write!(
                    f,
                    "Tempo state read {read_index} duplicates an earlier read"
                )
            }
            Self::TempoReaderBlockIndexOutOfRange {
                zone_block_index,
                block_count,
            } => {
                write!(
                    f,
                    "TempoStateReader requested out-of-range zone block {zone_block_index}; witness has {block_count} blocks"
                )
            }
            Self::MissingTempoStateRead {
                zone_block_index,
                tempo_block_number,
                account,
                slot,
            } => {
                write!(
                    f,
                    "missing proved Tempo state read for zone block {zone_block_index}, Tempo block {tempo_block_number}, account {account}, slot {slot}"
                )
            }
            Self::TempoStateAccountProofMissing {
                read_index,
                account,
                node_hash,
            } => {
                write!(
                    f,
                    "Tempo state read {read_index} account {account} proof is missing trie node {node_hash}"
                )
            }
            Self::TempoStateAccountProofInvalid {
                read_index,
                account,
            } => {
                write!(
                    f,
                    "Tempo state read {read_index} account {account} proof is invalid"
                )
            }
            Self::TempoStateAccountReadMismatch {
                read_index,
                account,
            } => {
                write!(
                    f,
                    "Tempo state read {read_index} account {account} does not match the proved Tempo root"
                )
            }
            Self::TempoStateStorageProofMissing {
                read_index,
                account,
                slot,
                node_hash,
            } => {
                write!(
                    f,
                    "Tempo state read {read_index} account {account} slot {slot} proof is missing trie node {node_hash}"
                )
            }
            Self::TempoStateStorageProofInvalid {
                read_index,
                account,
                slot,
            } => {
                write!(
                    f,
                    "Tempo state read {read_index} account {account} slot {slot} proof is invalid"
                )
            }
            Self::TempoStateStorageReadMismatch {
                read_index,
                account,
                slot,
            } => {
                write!(
                    f,
                    "Tempo state read {read_index} account {account} slot {slot} does not match the proved Tempo root"
                )
            }
            Self::AccountCodeHashMismatch(address) => {
                write!(f, "account code preimage hash mismatch for {address}")
            }
            Self::MissingAccountCode(address) => {
                write!(f, "account code preimage is missing for {address}")
            }
            Self::AccountProofMissing { account, node_hash } => {
                write!(
                    f,
                    "account {account} proof is missing trie node {node_hash}"
                )
            }
            Self::AccountProofInvalid(account) => {
                write!(f, "account {account} proof is invalid")
            }
            Self::AccountRlpInvalid(account) => {
                write!(f, "account {account} proof value is not a valid account")
            }
            Self::AccountReadMismatch(account) => {
                write!(
                    f,
                    "account {account} read does not match the proved state root"
                )
            }
            Self::MissingAccountRead(account) => {
                write!(
                    f,
                    "storage read for account {account} is missing a proved account read"
                )
            }
            Self::StorageProofMissing {
                account,
                slot,
                node_hash,
            } => {
                write!(
                    f,
                    "storage proof for account {account} slot {slot} is missing trie node {node_hash}"
                )
            }
            Self::StorageProofInvalid { account, slot } => {
                write!(
                    f,
                    "storage proof for account {account} slot {slot} is invalid"
                )
            }
            Self::StorageRlpInvalid { account, slot } => {
                write!(
                    f,
                    "storage proof value for account {account} slot {slot} is not a valid storage value"
                )
            }
            Self::StorageReadMismatch { account, slot } => {
                write!(
                    f,
                    "storage read for account {account} slot {slot} does not match the proved storage root"
                )
            }
            Self::MissingSystemStorageRead { account, slot } => {
                write!(
                    f,
                    "initial system commitment is missing account {account} storage slot {slot}"
                )
            }
            Self::MissingTempoBindingRead { slot } => {
                write!(
                    f,
                    "initial TempoState binding is missing storage slot {slot}"
                )
            }
            Self::TempoBlockNumberMismatch { expected, actual } => {
                write!(
                    f,
                    "TempoState block number mismatch: expected {expected}, got {actual}"
                )
            }
            Self::TempoAnchorHashMismatch { expected, actual } => {
                write!(
                    f,
                    "Tempo ancestry anchor hash mismatch: expected {expected}, got {actual}"
                )
            }
            Self::TempoAncestryLengthMismatch { expected, actual } => {
                write!(
                    f,
                    "Tempo ancestry header count mismatch: expected {expected}, got {actual}"
                )
            }
            Self::TempoAncestryTooLong => {
                f.write_str("Tempo ancestry header count does not fit in u64")
            }
            Self::TempoAncestryHeaderInvalid { index } => {
                write!(f, "Tempo ancestry header {index} is malformed")
            }
            Self::TempoAncestryBlockNumberOverflow { index } => {
                write!(f, "Tempo ancestry header {index} block number overflow")
            }
            Self::TempoAncestryParentHashMismatch {
                index,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "Tempo ancestry header {index} parent hash mismatch: expected {expected}, got {actual}"
                )
            }
            Self::TempoAncestryBlockNumberMismatch {
                index,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "Tempo ancestry header {index} block number mismatch: expected {expected}, got {actual}"
                )
            }
            Self::ExpectedWithdrawalBatchIndexZero => {
                f.write_str("expected withdrawal batch index must be non-zero")
            }
            Self::WithdrawalBatchIndexMismatch {
                expected_previous,
                actual_previous,
            } => {
                write!(
                    f,
                    "previous withdrawal batch index mismatch: expected {expected_previous}, got {actual_previous}"
                )
            }
            Self::AnchorBeforeTempo => {
                f.write_str("anchor block number is below tempo block number")
            }
            Self::EmptyBatch => f.write_str("batch must contain at least one zone block"),
            Self::BlockParentHashMismatch { index } => {
                write!(f, "zone block {index} parent hash mismatch")
            }
            Self::BlockNumberOverflow { index } => {
                write!(f, "zone block {index} number overflow")
            }
            Self::BlockNumberMismatch {
                index,
                expected,
                actual,
            } => write!(
                f,
                "zone block {index} number mismatch: expected {expected}, got {actual}"
            ),
            Self::BlockTimestampRegression { index } => {
                write!(f, "zone block {index} timestamp regressed")
            }
            Self::BlockTimestampMillisPartOutOfRange { index, actual } => write!(
                f,
                "zone block {index} timestamp_millis_part {actual} is outside 0..1000"
            ),
            Self::BlockBeneficiaryMismatch { index } => {
                write!(f, "zone block {index} beneficiary does not match sequencer")
            }
            Self::IntermediateWithdrawalFinalization { index } => {
                write!(
                    f,
                    "zone block {index} finalized withdrawals before final block"
                )
            }
            Self::MissingFinalWithdrawalFinalization => {
                f.write_str("final zone block is missing withdrawal finalization")
            }
            Self::ZoneAncestryHeaderLimitExceeded { count, limit } => write!(
                f,
                "zone ancestry header count {count} exceeds BLOCKHASH ancestor limit {limit}"
            ),
            Self::ZoneAncestryHeaderInvalid { index } => {
                write!(
                    f,
                    "zone ancestry header {index} is not valid ZoneHeader RLP"
                )
            }
            Self::ZoneAncestryBlockNumberUnderflow { index } => write!(
                f,
                "zone ancestry header {index} cannot precede genesis block"
            ),
            Self::ZoneAncestryParentHashMismatch {
                index,
                expected,
                actual,
            } => write!(
                f,
                "zone ancestry header {index} hash {actual} does not match child parent hash {expected}"
            ),
            Self::ZoneAncestryBlockNumberMismatch {
                index,
                expected,
                actual,
            } => write!(
                f,
                "zone ancestry header {index} has block number {actual}, expected {expected}"
            ),
            Self::UserTransactionDecodeFailed {
                block_index,
                transaction_index,
            } => write!(
                f,
                "zone block {block_index} user transaction {transaction_index} is not valid EIP-2718 Tempo transaction bytes"
            ),
            Self::UserTransactionSenderRecoveryFailed {
                block_index,
                transaction_index,
            } => write!(
                f,
                "zone block {block_index} user transaction {transaction_index} signer recovery failed"
            ),
            Self::TempoImportHeaderInvalid { index } => {
                write!(f, "zone block {index} Tempo import header is not valid RLP")
            }
            Self::TempoImportParentHashMismatch {
                index,
                expected,
                actual,
            } => write!(
                f,
                "zone block {index} imports Tempo parent hash {actual}, expected {expected}"
            ),
            Self::TempoImportBlockNumberMismatch {
                index,
                expected,
                actual,
            } => write!(
                f,
                "zone block {index} imports Tempo block number {actual}, expected {expected}"
            ),
            Self::TempoImportUnsupported { index } => {
                write!(f, "zone block {index} tempo import is not implemented yet")
            }
            Self::DepositProcessingUnsupported { index } => {
                write!(
                    f,
                    "zone block {index} deposit processing is not implemented yet"
                )
            }
            Self::UserTransactionsUnsupported { index } => {
                write!(
                    f,
                    "zone block {index} user transaction execution is not implemented yet"
                )
            }
            Self::NonZeroWithdrawalFinalizationUnsupported => {
                f.write_str("non-zero withdrawal finalization is not implemented yet")
            }
            Self::TempoStateReaderGasOverflow => {
                f.write_str("TempoStateReader gas calculation overflow")
            }
            Self::StateRootCalculationFailed => {
                f.write_str("stateless sparse-trie state root calculation failed")
            }
            Self::ExecutionBlockCountMismatch { expected, actual } => write!(
                f,
                "execution returned {actual} zone blocks, expected {expected}"
            ),
            Self::ExecutionPlanBlockCountMismatch { expected, actual } => write!(
                f,
                "execution plan has {actual} blocks, expected {expected} prepared blocks"
            ),
            Self::ExecutionReceiptCountMismatch {
                block_index,
                expected,
                actual,
            } => write!(
                f,
                "executed zone block {block_index} produced {actual} receipts, expected {expected}"
            ),
            Self::ExecutionBlockFailed { index } => {
                write!(f, "alloy block execution failed for zone block {index}")
            }
            Self::ExecutionBlockParentHashMismatch {
                index,
                expected,
                actual,
            } => write!(
                f,
                "executed zone block {index} parent hash {actual} does not match expected {expected}"
            ),
            Self::ExecutionBlockNumberMismatch {
                index,
                expected,
                actual,
            } => write!(
                f,
                "executed zone block {index} number {actual} does not match expected {expected}"
            ),
            Self::ExecutionBlockTimestampMismatch {
                index,
                expected,
                actual,
            } => write!(
                f,
                "executed zone block {index} timestamp {actual} does not match expected {expected}"
            ),
            Self::ExecutionBlockBeneficiaryMismatch {
                index,
                expected,
                actual,
            } => write!(
                f,
                "executed zone block {index} beneficiary {actual} does not match expected {expected}"
            ),
            Self::ExecutionBlockProtocolVersionMismatch {
                index,
                expected,
                actual,
            } => write!(
                f,
                "executed zone block {index} protocol version {actual} does not match expected {expected}"
            ),
            Self::ExecutionFinalTempoMismatch { expected, actual } => write!(
                f,
                "executed final Tempo binding {actual:?} does not match expected {expected:?}"
            ),
            Self::ExecutionWithdrawalBatchIndexMismatch { expected, actual } => write!(
                f,
                "executed withdrawal batch index {actual} does not match expected {expected}"
            ),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ProverError {}

/// Execute the production Zone batch transition function.
///
/// The Nitro enclave proof server and local native-signature proof path call
/// this entrypoint before signing or attesting a [`BatchOutput`]. Until the full
/// no-std stateless executor is wired in, production proving fails closed
/// instead of falling back to the legacy empty-block placeholder.
pub fn prove_zone_batch(witness: BatchWitness) -> Result<BatchOutput, ProverError> {
    let _prepared = prepare_stateless_execution(&witness)?;
    Err(ProverError::FullStatelessExecutionUnsupported)
}

pub fn prepare_stateless_execution(
    witness: &BatchWitness,
) -> Result<PreparedStatelessExecution, ProverError> {
    let public = &witness.public_inputs;

    if witness.prev_block_header.hash() != public.prev_block_hash {
        return Err(ProverError::PrevHeaderHashMismatch);
    }

    if witness.prev_block_header.state_root != witness.initial_zone_state.state_root {
        return Err(ProverError::InitialStateRootMismatch);
    }

    let final_index = witness
        .zone_blocks
        .len()
        .checked_sub(1)
        .ok_or(ProverError::EmptyBatch)?;

    validate_block_envelopes(witness, final_index)?;

    let zone_block_hashes = ancestry::verify_zone_ancestry_headers(
        &witness.prev_block_header,
        &witness.zone_ancestry_headers,
    )?;
    let execution_db = WitnessDatabase::new(&witness.initial_zone_state, zone_block_hashes)?;
    let initial_zone_state = validate_initial_zone_state(&witness.initial_zone_state)?;

    let (tempo_root_bindings, final_tempo_binding) =
        tempo_root_bindings_for_witness(initial_zone_state.tempo_binding, &witness.zone_blocks)?;
    validate_node_pool(
        &witness.tempo_state_proofs.node_pool,
        ProverError::TempoStateNodeHashMismatch,
    )?;
    let tempo_witness_provider = TempoWitnessProvider::new(
        &witness.tempo_state_proofs,
        witness.zone_blocks.len(),
        &tempo_root_bindings,
    )?;

    let expected_previous_withdrawal_batch_index = public
        .expected_withdrawal_batch_index
        .checked_sub(1)
        .ok_or(ProverError::ExpectedWithdrawalBatchIndexZero)?;
    if initial_zone_state.last_batch.withdrawal_batch_index
        != expected_previous_withdrawal_batch_index
    {
        return Err(ProverError::WithdrawalBatchIndexMismatch {
            expected_previous: expected_previous_withdrawal_batch_index,
            actual_previous: initial_zone_state.last_batch.withdrawal_batch_index,
        });
    }

    tempo::verify_tempo_ancestry(public, final_tempo_binding, &witness.tempo_ancestry_headers)?;

    let execution_plan = ZoneExecutionPlan::from_witness(witness)?;

    Ok(PreparedStatelessExecution {
        public_inputs: public.clone(),
        prev_block_header: witness.prev_block_header.clone(),
        zone_blocks: prepared_zone_blocks(&witness.zone_blocks),
        initial_zone_state: witness.initial_zone_state.clone(),
        execution_db,
        execution_plan,
        tempo_witness_provider,
        commitments: PreparedWitnessCommitments {
            initial_tempo_block_number: initial_zone_state.tempo_binding.block_number,
            initial_tempo_block_hash: initial_zone_state.tempo_binding.block_hash,
            initial_tempo_state_root: initial_zone_state.tempo_binding.state_root,
            final_tempo_block_number: final_tempo_binding.block_number,
            final_tempo_block_hash: final_tempo_binding.block_hash,
            final_tempo_state_root: final_tempo_binding.state_root,
            initial_deposit_queue: initial_zone_state.deposit_queue,
            previous_last_batch: initial_zone_state.last_batch,
        },
    })
}

fn prepared_zone_blocks(blocks: &[ZoneBlock]) -> Vec<PreparedZoneBlock> {
    blocks
        .iter()
        .map(|block| PreparedZoneBlock {
            number: block.number,
            parent_hash: block.parent_hash,
            timestamp: block.timestamp,
            beneficiary: block.beneficiary,
            protocol_version: block.protocol_version,
            cfg_env: block.cfg_env.config(),
            execution_context: block.execution_context.config(),
            block_env: block.block_env.config(),
        })
        .collect()
}

/// Execute the legacy deterministic empty-block transition.
///
/// This implementation is deliberately narrower than the final spec: it
/// executes deterministic empty blocks, computes real successor zone header
/// hashes, and fails closed for not-yet-implemented state, Tempo import,
/// deposit, withdrawal, and user-transaction paths. It exists only to preserve
/// focused tests while [`prove_zone_batch`] moves to full stateless execution.
pub fn prove_empty_zone_batch(witness: BatchWitness) -> Result<BatchOutput, ProverError> {
    let public = &witness.public_inputs;

    if witness.prev_block_header.hash() != public.prev_block_hash {
        return Err(ProverError::PrevHeaderHashMismatch);
    }

    if witness.prev_block_header.state_root != witness.initial_zone_state.state_root {
        return Err(ProverError::InitialStateRootMismatch);
    }
    let zone_block_hashes = ancestry::verify_zone_ancestry_headers(
        &witness.prev_block_header,
        &witness.zone_ancestry_headers,
    )?;

    let _execution_db = WitnessDatabase::new(&witness.initial_zone_state, zone_block_hashes)?;
    let initial_zone_state = validate_initial_zone_state(&witness.initial_zone_state)?;
    validate_node_pool(
        &witness.tempo_state_proofs.node_pool,
        ProverError::TempoStateNodeHashMismatch,
    )?;
    let _tempo_state_proofs = validate_tempo_state_proofs(
        &witness.tempo_state_proofs,
        witness.zone_blocks.len(),
        initial_zone_state.tempo_binding,
    )?;
    let expected_previous_withdrawal_batch_index = public
        .expected_withdrawal_batch_index
        .checked_sub(1)
        .ok_or(ProverError::ExpectedWithdrawalBatchIndexZero)?;
    if initial_zone_state.last_batch.withdrawal_batch_index
        != expected_previous_withdrawal_batch_index
    {
        return Err(ProverError::WithdrawalBatchIndexMismatch {
            expected_previous: expected_previous_withdrawal_batch_index,
            actual_previous: initial_zone_state.last_batch.withdrawal_batch_index,
        });
    }

    tempo::verify_tempo_ancestry(
        public,
        initial_zone_state.tempo_binding,
        &witness.tempo_ancestry_headers,
    )?;

    let mut prev_header = witness.prev_block_header;
    let mut prev_block_hash = public.prev_block_hash;
    let final_index = witness
        .zone_blocks
        .len()
        .checked_sub(1)
        .ok_or(ProverError::EmptyBatch)?;

    for (index, block) in witness.zone_blocks.iter().enumerate() {
        validate_block(
            index,
            final_index,
            &prev_header,
            prev_block_hash,
            public,
            block,
        )?;

        let finalized_count = block
            .finalize_withdrawal_batch_count
            .filter(|_| index == final_index);
        if let Some(count) = finalized_count
            && !count.is_zero()
        {
            return Err(ProverError::NonZeroWithdrawalFinalizationUnsupported);
        }

        let header = ZoneHeader {
            parent_hash: prev_block_hash,
            beneficiary: block.beneficiary,
            state_root: prev_header.state_root,
            transactions_root: EMPTY_TRIE_ROOT,
            receipts_root: EMPTY_TRIE_ROOT,
            number: block.number,
            timestamp: block.timestamp,
            protocol_version: block.protocol_version,
        };
        prev_block_hash = header.hash();
        prev_header = header;
    }

    Ok(BatchOutput {
        block_transition: BlockTransition {
            prevBlockHash: public.prev_block_hash,
            nextBlockHash: prev_block_hash,
        },
        deposit_queue_transition: DepositQueueTransition {
            prevProcessedHash: initial_zone_state.deposit_queue.processed_hash,
            nextProcessedHash: initial_zone_state.deposit_queue.processed_hash,
            prevDepositNumber: initial_zone_state.deposit_queue.processed_number,
            nextDepositNumber: initial_zone_state.deposit_queue.processed_number,
        },
        withdrawal_queue_hash: B256::ZERO,
        last_batch_commitment: LastBatchCommitment {
            withdrawal_queue_hash: B256::ZERO,
            withdrawal_batch_index: public.expected_withdrawal_batch_index,
        },
    })
}

fn validate_block_envelopes(witness: &BatchWitness, final_index: usize) -> Result<(), ProverError> {
    let mut prev_number = witness.prev_block_header.number;
    let mut prev_timestamp = witness.prev_block_header.timestamp;

    for (index, block) in witness.zone_blocks.iter().enumerate() {
        if index == 0 && block.parent_hash != witness.public_inputs.prev_block_hash {
            return Err(ProverError::BlockParentHashMismatch { index });
        }

        let expected_number = prev_number
            .checked_add(1)
            .ok_or(ProverError::BlockNumberOverflow { index })?;
        if block.number != expected_number {
            return Err(ProverError::BlockNumberMismatch {
                index,
                expected: expected_number,
                actual: block.number,
            });
        }
        if block.timestamp < prev_timestamp {
            return Err(ProverError::BlockTimestampRegression { index });
        }
        if block.block_env.timestamp_millis_part >= 1000 {
            return Err(ProverError::BlockTimestampMillisPartOutOfRange {
                index,
                actual: block.block_env.timestamp_millis_part,
            });
        }
        if block.beneficiary != witness.public_inputs.sequencer {
            return Err(ProverError::BlockBeneficiaryMismatch { index });
        }

        let is_final = index == final_index;
        match (is_final, block.finalize_withdrawal_batch_count.is_some()) {
            (false, true) => return Err(ProverError::IntermediateWithdrawalFinalization { index }),
            (true, false) => return Err(ProverError::MissingFinalWithdrawalFinalization),
            _ => {}
        }

        if block.tempo_header_rlp.is_none()
            && (!block.deposits.is_empty()
                || !block.decryptions.is_empty()
                || !block.enabled_tokens.is_empty())
        {
            return Err(ProverError::DepositProcessingUnsupported { index });
        }
        if block.finalize_withdrawal_batch_count.is_none()
            && !block.finalize_withdrawal_encrypted_senders.is_empty()
        {
            return Err(ProverError::NonZeroWithdrawalFinalizationUnsupported);
        }

        prev_number = block.number;
        prev_timestamp = block.timestamp;
    }

    Ok(())
}

fn tempo_root_bindings_for_witness(
    initial_binding: tempo::TempoBinding,
    blocks: &[ZoneBlock],
) -> Result<(Vec<TempoRootBinding>, tempo::TempoBinding), ProverError> {
    let mut bindings = Vec::new();
    bindings.push(TempoRootBinding::from_tempo_binding(initial_binding));
    let mut current = initial_binding;

    for (index, block) in blocks.iter().enumerate() {
        let Some(encoded) = &block.tempo_header_rlp else {
            continue;
        };
        let header = decode_tempo_import_header(index, encoded)?;
        if header.inner.parent_hash != current.block_hash {
            return Err(ProverError::TempoImportParentHashMismatch {
                index,
                expected: current.block_hash,
                actual: header.inner.parent_hash,
            });
        }

        let expected_number = current
            .block_number
            .checked_add(1)
            .ok_or(ProverError::TempoAncestryBlockNumberOverflow { index })?;
        if header.inner.number != expected_number {
            return Err(ProverError::TempoImportBlockNumberMismatch {
                index,
                expected: expected_number,
                actual: header.inner.number,
            });
        }

        current = tempo::TempoBinding {
            block_number: header.inner.number,
            block_hash: keccak256(encoded.as_ref()),
            state_root: header.inner.state_root,
        };
        bindings.push(TempoRootBinding::from_tempo_binding(current));
    }

    Ok((bindings, current))
}

fn decode_tempo_import_header(index: usize, encoded: &Bytes) -> Result<TempoHeader, ProverError> {
    let mut cursor = encoded.as_ref();
    let header = TempoHeader::decode(&mut cursor)
        .map_err(|_| ProverError::TempoImportHeaderInvalid { index })?;
    if !cursor.is_empty() {
        return Err(ProverError::TempoImportHeaderInvalid { index });
    }
    Ok(header)
}

fn validate_block(
    index: usize,
    final_index: usize,
    prev_header: &ZoneHeader,
    prev_block_hash: B256,
    public: &PublicInputs,
    block: &ZoneBlock,
) -> Result<(), ProverError> {
    if block.parent_hash != prev_block_hash {
        return Err(ProverError::BlockParentHashMismatch { index });
    }
    let expected_number = prev_header
        .number
        .checked_add(1)
        .ok_or(ProverError::BlockNumberOverflow { index })?;
    if block.number != expected_number {
        return Err(ProverError::BlockNumberMismatch {
            index,
            expected: expected_number,
            actual: block.number,
        });
    }
    if block.timestamp < prev_header.timestamp {
        return Err(ProverError::BlockTimestampRegression { index });
    }
    if block.beneficiary != public.sequencer {
        return Err(ProverError::BlockBeneficiaryMismatch { index });
    }

    let is_final = index == final_index;
    match (is_final, block.finalize_withdrawal_batch_count.is_some()) {
        (false, true) => return Err(ProverError::IntermediateWithdrawalFinalization { index }),
        (true, false) => return Err(ProverError::MissingFinalWithdrawalFinalization),
        _ => {}
    }

    if block.tempo_header_rlp.is_some() {
        return Err(ProverError::TempoImportUnsupported { index });
    }
    if !block.deposits.is_empty()
        || !block.decryptions.is_empty()
        || !block.enabled_tokens.is_empty()
    {
        return Err(ProverError::DepositProcessingUnsupported { index });
    }
    if !block.finalize_withdrawal_encrypted_senders.is_empty() {
        return Err(ProverError::NonZeroWithdrawalFinalizationUnsupported);
    }
    if !block.transactions.is_empty() {
        return Err(ProverError::UserTransactionsUnsupported { index });
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VerifiedInitialZoneState {
    tempo_binding: tempo::TempoBinding,
    deposit_queue: DepositQueueState,
    last_batch: LastBatchCommitment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DepositQueueState {
    pub processed_hash: B256,
    pub processed_number: u64,
}

fn validate_initial_zone_state(
    state: &ZoneStateWitness,
) -> Result<VerifiedInitialZoneState, ProverError> {
    let mut proven_accounts = BTreeMap::new();
    for account in &state.account_reads {
        if let Some(code) = &account.code
            && keccak256(code.as_ref()) != account.code_hash
        {
            return Err(ProverError::AccountCodeHashMismatch(account.account));
        }

        let proven_account =
            trie::verify_account_read(state.state_root, &state.node_pool, account)?;
        proven_accounts.insert(account.account, proven_account);
    }

    for storage in &state.storage_reads {
        let account = proven_accounts
            .get(&storage.account)
            .ok_or(ProverError::MissingAccountRead(storage.account))?;
        trie::verify_storage_read(account.storage_root, &state.node_pool, storage)?;
    }

    Ok(VerifiedInitialZoneState {
        tempo_binding: extract_tempo_binding(state)?,
        deposit_queue: extract_deposit_queue_state(state)?,
        last_batch: extract_last_batch(state)?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TempoStateReadKey {
    zone_block_index: u64,
    tempo_block_number: u64,
    account: Address,
    slot: U256,
}

/// Tempo L1 state root that witness-backed reads may prove against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TempoRootBinding {
    pub block_number: u64,
    pub state_root: B256,
}

impl TempoRootBinding {
    fn from_tempo_binding(binding: tempo::TempoBinding) -> Self {
        Self {
            block_number: binding.block_number,
            state_root: binding.state_root,
        }
    }
}

/// Proof-backed provider for Tempo L1 storage reads needed during Zone execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TempoWitnessProvider {
    reads: BTreeMap<TempoStateReadKey, U256>,
}

impl TempoWitnessProvider {
    pub fn new(
        proofs: &BatchStateProof,
        zone_block_count: usize,
        root_bindings: &[TempoRootBinding],
    ) -> Result<Self, ProverError> {
        let mut roots = BTreeMap::new();
        for (root_index, binding) in root_bindings.iter().enumerate() {
            if roots
                .insert(binding.block_number, binding.state_root)
                .is_some()
            {
                return Err(ProverError::DuplicateTempoRootBinding {
                    root_index,
                    tempo_block_number: binding.block_number,
                });
            }
        }

        let mut verified_reads = BTreeMap::new();

        for (read_index, read) in proofs.reads.iter().enumerate() {
            if usize::try_from(read.zone_block_index)
                .map_or(true, |index| index >= zone_block_count)
            {
                return Err(ProverError::TempoStateReadBlockIndexOutOfRange {
                    read_index,
                    zone_block_index: read.zone_block_index,
                });
            }

            let Some(state_root) = roots.get(&read.tempo_block_number).copied() else {
                return Err(ProverError::TempoStateReadUnboundTempoRoot {
                    read_index,
                    tempo_block_number: read.tempo_block_number,
                });
            };

            let account = trie::verify_account_proof(
                state_root,
                &proofs.node_pool,
                trie::AccountProof {
                    account: read.account,
                    nonce: read.account_nonce,
                    balance: read.account_balance,
                    storage_root: read.account_storage_root,
                    code_hash: read.account_code_hash,
                    proof_node_hashes: &read.account_proof_node_hashes,
                },
            )
            .map_err(|err| tempo_state_account_error(read_index, read, err))?;

            trie::verify_storage_proof(
                account.storage_root,
                &proofs.node_pool,
                trie::StorageProof {
                    slot: read.slot,
                    value: read.value,
                    proof_node_hashes: &read.storage_proof_node_hashes,
                },
            )
            .map_err(|err| tempo_state_storage_error(read_index, read, err))?;

            let key = TempoStateReadKey {
                zone_block_index: read.zone_block_index,
                tempo_block_number: read.tempo_block_number,
                account: read.account,
                slot: read.slot,
            };
            if verified_reads.insert(key, read.value).is_some() {
                return Err(ProverError::DuplicateTempoStateRead { read_index });
            }
        }

        Ok(Self {
            reads: verified_reads,
        })
    }

    pub fn read_storage_word(
        &self,
        zone_block_index: u64,
        tempo_block_number: u64,
        account: Address,
        slot: U256,
    ) -> Result<U256, ProverError> {
        let key = TempoStateReadKey {
            zone_block_index,
            tempo_block_number,
            account,
            slot,
        };
        self.reads
            .get(&key)
            .copied()
            .ok_or(ProverError::MissingTempoStateRead {
                zone_block_index,
                tempo_block_number,
                account,
                slot,
            })
    }

    pub fn read_storage_at(
        &self,
        zone_block_index: u64,
        tempo_block_number: u64,
        account: Address,
        slot: B256,
    ) -> Result<B256, ProverError> {
        Ok(B256::from(self.read_storage_word(
            zone_block_index,
            tempo_block_number,
            account,
            storage_slot_u256(slot),
        )?))
    }

    fn into_verified_reads(self) -> BTreeMap<TempoStateReadKey, U256> {
        self.reads
    }
}

fn validate_tempo_state_proofs(
    proofs: &BatchStateProof,
    zone_block_count: usize,
    binding: tempo::TempoBinding,
) -> Result<BTreeMap<TempoStateReadKey, U256>, ProverError> {
    let roots = [TempoRootBinding::from_tempo_binding(binding)];
    TempoWitnessProvider::new(proofs, zone_block_count, &roots)
        .map(TempoWitnessProvider::into_verified_reads)
        .map_err(|err| match err {
            ProverError::TempoStateReadUnboundTempoRoot {
                read_index,
                tempo_block_number,
            } => ProverError::TempoStateReadTempoBlockMismatch {
                read_index,
                expected: binding.block_number,
                actual: tempo_block_number,
            },
            err => err,
        })
}

fn tempo_state_account_error(
    read_index: usize,
    read: &L1StateRead,
    err: trie::TrieProofError,
) -> ProverError {
    match err {
        trie::TrieProofError::MissingNode(node_hash) => {
            ProverError::TempoStateAccountProofMissing {
                read_index,
                account: read.account,
                node_hash,
            }
        }
        trie::TrieProofError::Invalid => ProverError::TempoStateAccountProofInvalid {
            read_index,
            account: read.account,
        },
        trie::TrieProofError::ValueMismatch => ProverError::TempoStateAccountReadMismatch {
            read_index,
            account: read.account,
        },
    }
}

fn tempo_state_storage_error(
    read_index: usize,
    read: &L1StateRead,
    err: trie::TrieProofError,
) -> ProverError {
    match err {
        trie::TrieProofError::MissingNode(node_hash) => {
            ProverError::TempoStateStorageProofMissing {
                read_index,
                account: read.account,
                slot: read.slot,
                node_hash,
            }
        }
        trie::TrieProofError::Invalid => ProverError::TempoStateStorageProofInvalid {
            read_index,
            account: read.account,
            slot: read.slot,
        },
        trie::TrieProofError::ValueMismatch => ProverError::TempoStateStorageReadMismatch {
            read_index,
            account: read.account,
            slot: read.slot,
        },
    }
}

fn extract_tempo_binding(state: &ZoneStateWitness) -> Result<tempo::TempoBinding, ProverError> {
    let block_hash_slot = storage_slot_u256(TEMPO_BLOCK_HASH_SLOT);
    let state_root_slot = storage_slot_u256(TEMPO_STATE_ROOT_SLOT);
    let packed_slot = storage_slot_u256(TEMPO_PACKED_SLOT);
    let block_hash = tempo_state_storage_read(state, block_hash_slot)?;
    let state_root = tempo_state_storage_read(state, state_root_slot)?;
    let packed = tempo_state_storage_read(state, packed_slot)?;

    Ok(tempo::TempoBinding {
        block_number: (packed & U256::from(u64::MAX)).to::<u64>(),
        block_hash: B256::from(block_hash),
        state_root: B256::from(state_root),
    })
}

fn extract_deposit_queue_state(state: &ZoneStateWitness) -> Result<DepositQueueState, ProverError> {
    let processed_hash =
        system_storage_read(state, ZONE_INBOX_ADDRESS, ZONE_INBOX_PROCESSED_HASH_SLOT)?;
    let processed_number =
        system_storage_read(state, ZONE_INBOX_ADDRESS, ZONE_INBOX_PROCESSED_NUMBER_SLOT)?;

    Ok(DepositQueueState {
        processed_hash: B256::from(processed_hash),
        processed_number: low_u64(processed_number),
    })
}

fn extract_last_batch(state: &ZoneStateWitness) -> Result<LastBatchCommitment, ProverError> {
    let withdrawal_queue_hash =
        system_storage_read(state, ZONE_OUTBOX_ADDRESS, ZONE_OUTBOX_LAST_BATCH_HASH_SLOT)?;
    let withdrawal_batch_index = system_storage_read(
        state,
        ZONE_OUTBOX_ADDRESS,
        ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT,
    )?;

    Ok(LastBatchCommitment {
        withdrawal_queue_hash: B256::from(withdrawal_queue_hash),
        withdrawal_batch_index: low_u64(withdrawal_batch_index),
    })
}

fn system_storage_read(
    state: &ZoneStateWitness,
    account: Address,
    slot: U256,
) -> Result<U256, ProverError> {
    storage_read(state, account, slot)
        .ok_or(ProverError::MissingSystemStorageRead { account, slot })
}

fn tempo_state_storage_read(state: &ZoneStateWitness, slot: U256) -> Result<U256, ProverError> {
    storage_read(state, TEMPO_STATE_ADDRESS, slot)
        .ok_or(ProverError::MissingTempoBindingRead { slot })
}

fn storage_read(state: &ZoneStateWitness, account: Address, slot: U256) -> Option<U256> {
    state
        .storage_reads
        .iter()
        .find(|read| read.account == account && read.slot == slot)
        .map(|read| read.value)
}

fn storage_slot_u256(slot: B256) -> U256 {
    slot.into()
}

fn low_u64(value: U256) -> u64 {
    (value & U256::from(u64::MAX)).to::<u64>()
}

pub(crate) fn validate_node_pool(
    node_pool: &BTreeMap<B256, Bytes>,
    err: impl Fn(B256) -> ProverError,
) -> Result<(), ProverError> {
    for (expected_hash, node) in node_pool {
        if keccak256(node.as_ref()) != *expected_hash {
            return Err(err(*expected_hash));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloy_consensus::{
        TxReceipt,
        proofs::{calculate_receipt_root, calculate_transaction_root},
    };
    use alloy_evm::block::BlockExecutionResult;
    use alloy_primitives::{B256, address, keccak256};
    use alloy_rlp::Encodable;
    use alloy_sol_types::SolCall;
    use alloy_trie::{HashBuilder, KECCAK_EMPTY, Nibbles, TrieAccount, proof::ProofRetainer};
    use reth_trie_common::{KeccakKeyHasher, KeyHasher};
    use revm_database::{
        AccountStatus, BundleState, TransitionAccount, primitives::StorageKeyMap,
        states::StorageSlot,
    };
    use revm_database_interface::{
        Database, DatabaseCommit,
        primitives::AddressMap,
        state::{Account, AccountInfo},
    };
    use tempo_chainspec::hardfork::TempoHardfork;
    use tempo_primitives::{TempoReceipt, TempoTxType};
    use tempo_zone_contracts::TempoStateReader;

    fn fixture_cfg_env() -> ZoneCfgEnvWitness {
        ZoneCfgEnvWitness {
            chain_id: 421_700_001,
            spec: TempoHardfork::T1,
            enable_amsterdam_eip8037: false,
        }
    }

    fn fixture_execution_context() -> ZoneBlockExecutionContextWitness {
        ZoneBlockExecutionContextWitness {
            parent_beacon_block_root: None,
            extra_data: Bytes::new(),
        }
    }

    fn fixture_block_env() -> ZoneBlockEnvWitness {
        ZoneBlockEnvWitness {
            gas_limit: 30_000_000,
            basefee: 0,
            difficulty: U256::ZERO,
            prevrandao: Some(B256::ZERO),
            slot_num: 0,
            timestamp_millis_part: 0,
        }
    }

    fn fixture_witness() -> BatchWitness {
        let tempo_block_number = 100;
        let tempo_block_hash = B256::repeat_byte(0x03);
        let initial_zone_state = tempo_bound_zone_state(tempo_block_number, tempo_block_hash);
        let header = ZoneHeader {
            parent_hash: B256::repeat_byte(0x01),
            beneficiary: address!("0x0000000000000000000000000000000000000001"),
            state_root: initial_zone_state.state_root,
            transactions_root: EMPTY_TRIE_ROOT,
            receipts_root: EMPTY_TRIE_ROOT,
            number: 7,
            timestamp: 11,
            protocol_version: 1,
        };
        let prev_block_hash = header.hash();
        BatchWitness {
            public_inputs: PublicInputs {
                prev_block_hash,
                tempo_block_number,
                anchor_block_number: tempo_block_number,
                anchor_block_hash: tempo_block_hash,
                expected_withdrawal_batch_index: 5,
                sequencer: header.beneficiary,
            },
            prev_block_header: header,
            zone_ancestry_headers: Vec::new(),
            zone_blocks: vec![ZoneBlock {
                number: 8,
                parent_hash: prev_block_hash,
                timestamp: 12,
                beneficiary: address!("0x0000000000000000000000000000000000000001"),
                protocol_version: 1,
                cfg_env: fixture_cfg_env(),
                execution_context: fixture_execution_context(),
                block_env: fixture_block_env(),
                tempo_header_rlp: None,
                deposits: Vec::new(),
                decryptions: Vec::new(),
                enabled_tokens: Vec::new(),
                finalize_withdrawal_batch_count: Some(U256::ZERO),
                finalize_withdrawal_encrypted_senders: Vec::new(),
                transactions: Vec::new(),
            }],
            initial_zone_state,
            tempo_state_proofs: BatchStateProof {
                node_pool: BTreeMap::new(),
                reads: Vec::new(),
            },
            tempo_ancestry_headers: Vec::new(),
        }
    }

    #[test]
    fn production_prover_fails_closed_until_full_stateless_execution() {
        assert_eq!(
            prove_zone_batch(fixture_witness()).unwrap_err(),
            ProverError::FullStatelessExecutionUnsupported
        );
    }

    #[test]
    fn production_prover_rejects_bad_previous_header_before_executor_boundary() {
        let mut witness = fixture_witness();
        witness.public_inputs.prev_block_hash = B256::repeat_byte(0xff);

        assert_eq!(
            prove_zone_batch(witness).unwrap_err(),
            ProverError::PrevHeaderHashMismatch
        );
    }

    #[test]
    fn production_prover_decodes_user_transactions_before_executor_boundary() {
        let mut witness = fixture_witness();
        witness.zone_blocks[0]
            .transactions
            .push(Bytes::from_static(b"not a transaction"));

        assert_eq!(
            prove_zone_batch(witness).unwrap_err(),
            ProverError::UserTransactionDecodeFailed {
                block_index: 0,
                transaction_index: 0,
            }
        );
    }

    #[test]
    fn prepares_verified_stateless_execution_inputs() {
        let witness = fixture_witness();
        let mut prepared = prepare_stateless_execution(&witness).unwrap();

        assert_eq!(
            prepared
                .execution_db
                .block_hash(witness.prev_block_header.number),
            Ok(witness.prev_block_header.hash())
        );
        assert_eq!(
            prepared.initial_zone_state.state_root,
            witness.initial_zone_state.state_root
        );
        assert_eq!(
            prepared
                .state_root_calculator()
                .unwrap()
                .calculate(ExecutionPostState::default().into_hashed())
                .unwrap()
                .get(),
            witness.initial_zone_state.state_root
        );
        assert_eq!(prepared.execution_plan.blocks.len(), 1);
        assert_eq!(prepared.execution_plan.blocks[0].transactions.len(), 1);
        assert_eq!(
            prepared.commitments.initial_deposit_queue.processed_number,
            12
        );
        assert_eq!(
            prepared
                .commitments
                .previous_last_batch
                .withdrawal_batch_index,
            witness
                .public_inputs
                .expected_withdrawal_batch_index
                .checked_sub(1)
                .expect("fixture index is nonzero")
        );
        assert_eq!(
            prepared.commitments.initial_tempo_block_number,
            witness.public_inputs.tempo_block_number
        );
        assert_eq!(
            prepared.commitments.final_tempo_block_hash,
            witness.public_inputs.anchor_block_hash
        );
    }

    #[test]
    fn prepared_execution_state_uses_strict_witness_database() {
        let witness = fixture_witness();
        let prepared = prepare_stateless_execution(&witness).unwrap();
        let mut state = prepared.execution_state();

        assert!(state.basic(TEMPO_STATE_ADDRESS).unwrap().is_some());
        assert_eq!(
            state
                .storage(ZONE_INBOX_ADDRESS, ZONE_INBOX_PROCESSED_NUMBER_SLOT)
                .unwrap(),
            U256::from(12)
        );
        assert_eq!(
            state
                .storage(TEMPO_STATE_ADDRESS, U256::from(0xfe))
                .unwrap_err()
                .into_external_error(),
            WitnessDbError::MissingStorage {
                account: TEMPO_STATE_ADDRESS,
                slot: U256::from(0xfe),
            }
        );
    }

    #[test]
    fn execution_state_commits_feed_hashed_post_state() {
        let witness = fixture_witness();
        let prepared = prepare_stateless_execution(&witness).unwrap();
        let mut state = prepared.clone().into_execution_state();
        let info = state
            .basic(ZONE_INBOX_ADDRESS)
            .unwrap()
            .expect("fixture witnesses the zone inbox account");

        let mut account = Account::from(info);
        account.info.balance = U256::from(99);
        account.mark_touch();
        let mut changes = AddressMap::default();
        changes.insert(ZONE_INBOX_ADDRESS, account);
        state.commit(changes);

        let post_state = execution_post_state_from_state(state);
        let hashed_address = KeccakKeyHasher::hash_key(ZONE_INBOX_ADDRESS);
        let hashed_account = post_state
            .hashed()
            .accounts
            .get(&hashed_address)
            .expect("committed account should appear in post-state")
            .expect("committed account should not be deleted");

        assert_eq!(hashed_account.balance, U256::from(99));
    }

    struct SuccessfulBlockExecutor {
        expected_withdrawal_batch_index: u64,
    }

    impl StatelessZoneBlockExecutor for SuccessfulBlockExecutor {
        type Receipt = TempoReceipt;

        fn execute_block(
            &mut self,
            state: &mut ZoneExecutionState,
            input: &ZoneBlockExecutionInput<'_>,
        ) -> Result<BlockExecutionResult<Self::Receipt>, ProverError> {
            let original = state
                .storage(ZONE_OUTBOX_ADDRESS, ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT)
                .unwrap();
            let info = state
                .basic(ZONE_OUTBOX_ADDRESS)
                .unwrap()
                .expect("fixture witnesses the zone outbox account");
            let mut storage = StorageKeyMap::default();
            storage.insert(
                ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT,
                StorageSlot::new_changed(
                    original,
                    U256::from(self.expected_withdrawal_batch_index),
                ),
            );
            state
                .transition_state
                .as_mut()
                .expect("execution state must retain transitions")
                .transitions
                .insert(
                    ZONE_OUTBOX_ADDRESS,
                    TransitionAccount {
                        info: Some(info.clone()),
                        status: AccountStatus::Changed,
                        previous_info: Some(info),
                        previous_status: AccountStatus::Loaded,
                        storage,
                        storage_was_destroyed: false,
                    },
                );

            Ok(successful_block_result(input.transactions.len()))
        }
    }

    #[test]
    fn prove_with_executor_derives_batch_output_from_execution() {
        let witness = fixture_witness();
        let mut executor = SuccessfulBlockExecutor {
            expected_withdrawal_batch_index: witness.public_inputs.expected_withdrawal_batch_index,
        };

        let output = prove_zone_batch_with_executor(witness.clone(), &mut executor).unwrap();

        assert_eq!(
            output.block_transition.prevBlockHash,
            witness.public_inputs.prev_block_hash
        );
        assert_ne!(
            output.block_transition.nextBlockHash,
            witness.public_inputs.prev_block_hash
        );
        assert_eq!(output.deposit_queue_transition.prevDepositNumber, 12);
        assert_eq!(output.deposit_queue_transition.nextDepositNumber, 12);
        assert_eq!(
            output.last_batch_commitment.withdrawal_batch_index,
            witness.public_inputs.expected_withdrawal_batch_index
        );
    }

    #[test]
    fn execution_commitments_derive_deposit_queue_from_post_state() {
        let witness = fixture_witness();
        let prepared = prepare_stateless_execution(&witness).unwrap();
        let next_hash = B256::repeat_byte(0x66);
        let next_number = 21;
        let mut storage = StorageKeyMap::default();
        storage.insert(
            ZONE_INBOX_PROCESSED_HASH_SLOT,
            (
                U256::from_be_bytes(prepared.commitments.initial_deposit_queue.processed_hash.0),
                U256::from_be_bytes(next_hash.0),
            ),
        );
        storage.insert(
            ZONE_INBOX_PROCESSED_NUMBER_SLOT,
            (
                U256::from(prepared.commitments.initial_deposit_queue.processed_number),
                U256::from(next_number),
            ),
        );

        let post_state = post_state_for_storage(ZONE_INBOX_ADDRESS, storage);
        let commitments = execution_commitments_from_post_state(&prepared, &post_state);

        assert_eq!(commitments.final_deposit_queue.processed_hash, next_hash);
        assert_eq!(
            commitments.final_deposit_queue.processed_number,
            next_number
        );
    }

    #[test]
    fn execution_output_rejects_wrong_block_parent() {
        let witness = fixture_witness();
        let mut prepared = prepare_stateless_execution(&witness).unwrap();
        prepared.zone_blocks[0].parent_hash = B256::repeat_byte(0xee);
        let execution = successful_execution_output(&prepared);

        assert_eq!(
            batch_output_from_execution(&prepared, &execution).unwrap_err(),
            ProverError::ExecutionBlockParentHashMismatch {
                index: 0,
                expected: witness.public_inputs.prev_block_hash,
                actual: B256::repeat_byte(0xee),
            }
        );
    }

    #[test]
    fn execution_roots_are_used_to_derive_next_block_hash() {
        let witness = fixture_witness();
        let prepared = prepare_stateless_execution(&witness).unwrap();
        let mut first = successful_execution_output(&prepared);
        let mut second = successful_execution_output(&prepared);
        let transactions = recovered_transactions_for_block(&prepared, 0);
        let mut different_receipts = successful_block_result(transactions.len());
        different_receipts
            .receipts
            .first_mut()
            .expect("fixture block has a system transaction receipt")
            .success = false;
        second.blocks[0] = ExecutedZoneBlock::from_alloy_block_execution(
            0,
            CalculatedStateRoot::trusted_for_test(prepared.prev_block_header.state_root),
            &transactions,
            &different_receipts,
        )
        .unwrap();

        let first_output = batch_output_from_execution(&prepared, &first).unwrap();
        let second_output = batch_output_from_execution(&prepared, &second).unwrap();

        assert_ne!(
            first_output.block_transition.nextBlockHash,
            second_output.block_transition.nextBlockHash
        );
        let block_result = successful_block_result(transactions.len());
        first.blocks[0] = ExecutedZoneBlock::from_alloy_block_execution(
            0,
            CalculatedStateRoot::trusted_for_test(B256::repeat_byte(0x98)),
            &transactions,
            &block_result,
        )
        .unwrap();
        let third_output = batch_output_from_execution(&prepared, &first).unwrap();
        assert_ne!(
            third_output.block_transition.nextBlockHash,
            first_output.block_transition.nextBlockHash
        );
    }

    #[test]
    fn alloy_execution_output_derives_roots_from_execution_artifacts() {
        let witness = fixture_witness();
        let prepared = prepare_stateless_execution(&witness).unwrap();
        let transactions = recovered_transactions_for_block(&prepared, 0);
        let block_result = successful_block_result(transactions.len());
        let bundle_state = BundleState::default();
        let artifact = ZoneBlockExecutionArtifacts {
            state_root: CalculatedStateRoot::trusted_for_test(B256::repeat_byte(0x42)),
            transactions: &transactions,
            result: &block_result,
        };

        let output =
            StatelessExecutionOutput::from_alloy_execution(&[artifact], &bundle_state).unwrap();

        let receipts_with_bloom = block_result
            .receipts
            .iter()
            .map(TxReceipt::with_bloom_ref)
            .collect::<Vec<_>>();
        assert_eq!(output.blocks[0].state_root(), B256::repeat_byte(0x42));
        assert_eq!(
            output.blocks[0].transactions_root(),
            calculate_transaction_root(&transactions)
        );
        assert_eq!(
            output.blocks[0].receipts_root(),
            calculate_receipt_root(&receipts_with_bloom)
        );
        assert!(output.post_state.is_empty());
    }

    #[test]
    fn alloy_execution_output_rejects_receipt_count_mismatch() {
        let witness = fixture_witness();
        let prepared = prepare_stateless_execution(&witness).unwrap();
        let transactions = recovered_transactions_for_block(&prepared, 0);
        let block_result = successful_block_result(0);
        let artifact = ZoneBlockExecutionArtifacts {
            state_root: CalculatedStateRoot::trusted_for_test(B256::repeat_byte(0x42)),
            transactions: &transactions,
            result: &block_result,
        };

        assert_eq!(
            StatelessExecutionOutput::from_alloy_execution(&[artifact], &BundleState::default())
                .unwrap_err(),
            ProverError::ExecutionReceiptCountMismatch {
                block_index: 0,
                expected: transactions.len(),
                actual: 0,
            }
        );
    }

    #[test]
    fn execution_output_rejects_block_count_mismatch() {
        let witness = fixture_witness();
        let prepared = prepare_stateless_execution(&witness).unwrap();
        let mut execution = successful_execution_output(&prepared);
        execution.blocks.clear();

        assert_eq!(
            batch_output_from_execution(&prepared, &execution).unwrap_err(),
            ProverError::ExecutionBlockCountMismatch {
                expected: 1,
                actual: 0,
            }
        );
    }

    #[test]
    fn execution_output_rejects_wrong_final_tempo_binding() {
        let witness = fixture_witness();
        let prepared = prepare_stateless_execution(&witness).unwrap();
        let mut execution = successful_execution_output(&prepared);
        let mut storage = StorageKeyMap::default();
        storage.insert(
            storage_slot_u256(TEMPO_BLOCK_HASH_SLOT),
            (
                U256::from_be_bytes(prepared.commitments.initial_tempo_block_hash.0),
                U256::from_be_bytes(B256::repeat_byte(0xee).0),
            ),
        );
        execution.post_state = post_state_for_storage(TEMPO_STATE_ADDRESS, storage);

        assert_eq!(
            batch_output_from_execution(&prepared, &execution).unwrap_err(),
            ProverError::ExecutionFinalTempoMismatch {
                expected: TempoExecutionCommitment {
                    block_number: prepared.commitments.final_tempo_block_number,
                    block_hash: prepared.commitments.final_tempo_block_hash,
                    state_root: prepared.commitments.final_tempo_state_root,
                },
                actual: execution_commitments_from_post_state(&prepared, &execution.post_state)
                    .final_tempo,
            }
        );
    }

    #[test]
    fn execution_output_rejects_wrong_withdrawal_batch_index() {
        let witness = fixture_witness();
        let prepared = prepare_stateless_execution(&witness).unwrap();
        let mut execution = successful_execution_output(&prepared);
        let mut storage = StorageKeyMap::default();
        storage.insert(
            ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT,
            (
                U256::from(
                    prepared
                        .commitments
                        .previous_last_batch
                        .withdrawal_batch_index,
                ),
                U256::from(99),
            ),
        );
        execution.post_state = post_state_for_storage(ZONE_OUTBOX_ADDRESS, storage);

        assert_eq!(
            batch_output_from_execution(&prepared, &execution).unwrap_err(),
            ProverError::ExecutionWithdrawalBatchIndexMismatch {
                expected: witness.public_inputs.expected_withdrawal_batch_index,
                actual: 99,
            }
        );
    }

    #[test]
    fn verifies_zone_ancestry_for_blockhash_witness() {
        let mut witness = fixture_witness();
        let parent = parent_of(&witness.prev_block_header);
        bind_parent_header(&mut witness, &parent);
        witness
            .zone_ancestry_headers
            .push(encode_zone_header(&parent));

        let hashes = ancestry::verify_zone_ancestry_headers(
            &witness.prev_block_header,
            &witness.zone_ancestry_headers,
        )
        .unwrap();

        assert_eq!(
            hashes.get(&witness.prev_block_header.number).copied(),
            Some(witness.prev_block_header.hash())
        );
        assert_eq!(hashes.get(&parent.number).copied(), Some(parent.hash()));
        let mut db = WitnessDatabase::new(&witness.initial_zone_state, hashes).unwrap();
        assert_eq!(db.block_hash(parent.number).unwrap(), parent.hash());
        prove_empty_zone_batch(witness).unwrap();
    }

    #[test]
    fn rejects_zone_ancestry_parent_hash_mismatch() {
        let mut witness = fixture_witness();
        let parent = parent_of(&witness.prev_block_header);
        witness
            .zone_ancestry_headers
            .push(encode_zone_header(&parent));

        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::ZoneAncestryParentHashMismatch {
                index: 0,
                expected: B256::repeat_byte(0x01),
                actual: parent.hash(),
            }
        );
    }

    #[test]
    fn rejects_zone_ancestry_header_with_trailing_bytes() {
        let mut witness = fixture_witness();
        let parent = parent_of(&witness.prev_block_header);
        bind_parent_header(&mut witness, &parent);
        let mut encoded = encode_zone_header(&parent).to_vec();
        encoded.push(0);
        witness.zone_ancestry_headers.push(Bytes::from(encoded));

        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::ZoneAncestryHeaderInvalid { index: 0 }
        );
    }

    #[test]
    fn rejects_zone_ancestry_beyond_blockhash_window() {
        let mut witness = fixture_witness();
        witness.zone_ancestry_headers = vec![Bytes::new(); ancestry::BLOCKHASH_ANCESTOR_LIMIT];

        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::ZoneAncestryHeaderLimitExceeded {
                count: ancestry::BLOCKHASH_ANCESTOR_LIMIT
                    .checked_add(1)
                    .expect("test count fits"),
                limit: ancestry::BLOCKHASH_ANCESTOR_LIMIT,
            }
        );
    }

    #[test]
    fn witness_database_serves_only_verified_state_reads() {
        let witness = fixture_witness();
        let mut block_hashes = BTreeMap::new();
        block_hashes.insert(6, B256::repeat_byte(0x06));
        let mut db = WitnessDatabase::new(&witness.initial_zone_state, block_hashes).unwrap();

        assert!(db.basic(TEMPO_STATE_ADDRESS).unwrap().is_some());
        assert_eq!(
            db.storage(ZONE_INBOX_ADDRESS, ZONE_INBOX_PROCESSED_NUMBER_SLOT)
                .unwrap(),
            U256::from(12)
        );
        assert_eq!(db.block_hash(6).unwrap(), B256::repeat_byte(0x06));
        assert!(db.code_by_hash(KECCAK_EMPTY).is_ok());
    }

    #[test]
    fn witness_database_rejects_unwitnessed_reads() {
        let witness = fixture_witness();
        let mut db = WitnessDatabase::new(&witness.initial_zone_state, BTreeMap::new()).unwrap();
        let missing = address!("0x0000000000000000000000000000000000009999");

        assert_eq!(
            db.basic(missing).unwrap_err(),
            WitnessDbError::MissingAccount(missing)
        );
        assert_eq!(
            db.storage(TEMPO_STATE_ADDRESS, U256::from(0xfe))
                .unwrap_err(),
            WitnessDbError::MissingStorage {
                account: TEMPO_STATE_ADDRESS,
                slot: U256::from(0xfe),
            }
        );
        assert_eq!(
            db.block_hash(9).unwrap_err(),
            WitnessDbError::MissingBlockHash(9)
        );
    }

    #[test]
    fn witness_database_rejects_missing_code_preimage() {
        let account = address!("0x0000000000000000000000000000000000001000");
        let trie_account = TrieAccount {
            nonce: 1,
            balance: U256::ZERO,
            storage_root: EMPTY_TRIE_ROOT,
            code_hash: B256::repeat_byte(0xaa),
        };
        let state = assemble_zone_state(
            vec![(account, trie_account)],
            vec![account_read(account, trie_account)],
            Vec::new(),
            BTreeMap::new(),
        );

        assert_eq!(
            WitnessDatabase::new(&state, BTreeMap::new()).unwrap_err(),
            ProverError::MissingAccountCode(account)
        );
    }

    fn set_initial_zone_state(witness: &mut BatchWitness, state: ZoneStateWitness) {
        witness.prev_block_header.state_root = state.state_root;
        let prev_block_hash = witness.prev_block_header.hash();
        witness.public_inputs.prev_block_hash = prev_block_hash;
        witness.zone_blocks[0].parent_hash = prev_block_hash;
        witness.initial_zone_state = state;
    }

    fn encode_zone_header(header: &ZoneHeader) -> Bytes {
        let mut encoded = Vec::new();
        header.encode(&mut encoded);
        Bytes::from(encoded)
    }

    fn parent_of(header: &ZoneHeader) -> ZoneHeader {
        ZoneHeader {
            parent_hash: B256::repeat_byte(0xaa),
            beneficiary: header.beneficiary,
            state_root: header.state_root,
            transactions_root: EMPTY_TRIE_ROOT,
            receipts_root: EMPTY_TRIE_ROOT,
            number: header
                .number
                .checked_sub(1)
                .expect("test header number has parent"),
            timestamp: header
                .timestamp
                .checked_sub(1)
                .expect("test header timestamp has parent"),
            protocol_version: header.protocol_version,
        }
    }

    fn bind_parent_header(witness: &mut BatchWitness, parent: &ZoneHeader) {
        witness.prev_block_header.parent_hash = parent.hash();
        let prev_block_hash = witness.prev_block_header.hash();
        witness.public_inputs.prev_block_hash = prev_block_hash;
        witness.zone_blocks[0].parent_hash = prev_block_hash;
    }

    fn post_state_for_storage(
        account: Address,
        storage: StorageKeyMap<(U256, U256)>,
    ) -> ExecutionPostState {
        ExecutionPostState::from_bundle_state(&bundle_state_for_storage(account, storage))
    }

    fn bundle_state_for_storage(
        account: Address,
        storage: StorageKeyMap<(U256, U256)>,
    ) -> BundleState {
        let bundle = BundleState::builder(0..=0)
            .state_storage(account, storage)
            .build();
        bundle
    }

    fn successful_block_result(receipt_count: usize) -> BlockExecutionResult<TempoReceipt> {
        BlockExecutionResult {
            receipts: vec![
                TempoReceipt {
                    tx_type: TempoTxType::Legacy,
                    success: true,
                    cumulative_gas_used: 0,
                    logs: Vec::new(),
                };
                receipt_count
            ],
            ..Default::default()
        }
    }

    fn recovered_transactions_for_block(
        prepared: &PreparedStatelessExecution,
        block_index: usize,
    ) -> Vec<RecoveredTempoTx> {
        prepared.execution_plan.blocks[block_index]
            .transactions
            .iter()
            .map(|transaction| transaction.tx.clone())
            .collect()
    }

    fn successful_execution_output(
        prepared: &PreparedStatelessExecution,
    ) -> StatelessExecutionOutput {
        let transactions = (0..prepared.zone_blocks.len())
            .map(|block_index| recovered_transactions_for_block(prepared, block_index))
            .collect::<Vec<_>>();
        let block_results = transactions
            .iter()
            .map(|transactions| successful_block_result(transactions.len()))
            .collect::<Vec<_>>();

        let mut storage = StorageKeyMap::default();
        storage.insert(
            ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT,
            (
                U256::from(
                    prepared
                        .commitments
                        .previous_last_batch
                        .withdrawal_batch_index,
                ),
                U256::from(prepared.public_inputs.expected_withdrawal_batch_index),
            ),
        );
        let bundle_state = bundle_state_for_storage(ZONE_OUTBOX_ADDRESS, storage);
        let artifacts = prepared
            .zone_blocks
            .iter()
            .enumerate()
            .map(|(block_index, _)| ZoneBlockExecutionArtifacts {
                state_root: CalculatedStateRoot::trusted_for_test(
                    prepared.prev_block_header.state_root,
                ),
                transactions: transactions[block_index].as_slice(),
                result: &block_results[block_index],
            })
            .collect::<Vec<_>>();

        StatelessExecutionOutput::from_alloy_execution(&artifacts, &bundle_state).unwrap()
    }

    fn insert_proof_nodes(
        node_pool: &mut BTreeMap<B256, Bytes>,
        nodes: impl IntoIterator<Item = (Nibbles, Bytes)>,
    ) -> Vec<B256> {
        let mut proof_node_hashes = Vec::new();
        for (_, node) in nodes {
            if node.as_ref() != [alloy_rlp::EMPTY_STRING_CODE] {
                let hash = keccak256(node.as_ref());
                node_pool.insert(hash, node);
                proof_node_hashes.push(hash);
            }
        }
        proof_node_hashes
    }

    fn storage_trie_with_proof(
        stored_slot: U256,
        stored_value: U256,
        proof_slot: U256,
    ) -> (B256, BTreeMap<B256, Bytes>, Vec<B256>) {
        let (root, node_pool, mut proofs) =
            storage_trie_with_entries_and_proofs(&[(stored_slot, stored_value)], &[proof_slot]);
        let proof = proofs
            .remove(&proof_slot)
            .expect("storage proof was retained for requested slot");
        (root, node_pool, proof)
    }

    fn storage_trie_with_entries_and_proofs(
        entries: &[(U256, U256)],
        proof_slots: &[U256],
    ) -> (B256, BTreeMap<B256, Bytes>, BTreeMap<U256, Vec<B256>>) {
        let proof_keys = proof_slots
            .iter()
            .copied()
            .map(|slot| (slot, Nibbles::unpack(keccak256(slot.to_be_bytes::<32>()))))
            .collect::<Vec<_>>();
        let retainer = ProofRetainer::from_iter(proof_keys.iter().map(|(_, key)| *key));
        let mut hash_builder = HashBuilder::default().with_proof_retainer(retainer);

        let mut hashed_entries = entries
            .iter()
            .filter(|(_, value)| !value.is_zero())
            .map(|(slot, value)| {
                let key_hash = keccak256(slot.to_be_bytes::<32>());
                (
                    *key_hash,
                    Nibbles::unpack(key_hash),
                    alloy_rlp::encode_fixed_size(value),
                )
            })
            .collect::<Vec<_>>();
        hashed_entries.sort_unstable_by_key(|(key_hash, _, _)| *key_hash);
        for (_, key, encoded_value) in hashed_entries {
            hash_builder.add_leaf(key, encoded_value.as_ref());
        }
        let root = hash_builder.root();

        let mut node_pool = BTreeMap::new();
        let proof_nodes = hash_builder.take_proof_nodes();
        let mut proofs = BTreeMap::new();
        for (slot, proof_key) in proof_keys {
            let proof_node_hashes = insert_proof_nodes(
                &mut node_pool,
                proof_nodes.matching_nodes_sorted(&proof_key),
            );
            proofs.insert(slot, proof_node_hashes);
        }
        (root, node_pool, proofs)
    }

    fn account_trie_with_proofs(
        entries: &[(Address, TrieAccount)],
        proof_accounts: &[Address],
    ) -> (B256, BTreeMap<B256, Bytes>, BTreeMap<Address, Vec<B256>>) {
        let proof_keys = proof_accounts
            .iter()
            .copied()
            .map(|account| (account, Nibbles::unpack(keccak256(account))))
            .collect::<Vec<_>>();
        let retainer = ProofRetainer::from_iter(proof_keys.iter().map(|(_, key)| *key));
        let mut hash_builder = HashBuilder::default().with_proof_retainer(retainer);

        let mut hashed_entries = entries
            .iter()
            .map(|(account, trie_account)| {
                let key_hash = keccak256(account);
                let mut encoded_account = Vec::new();
                trie_account.encode(&mut encoded_account);
                (*key_hash, Nibbles::unpack(key_hash), encoded_account)
            })
            .collect::<Vec<_>>();
        hashed_entries.sort_unstable_by_key(|(key_hash, _, _)| *key_hash);
        for (_, key, encoded_account) in hashed_entries {
            hash_builder.add_leaf(key, &encoded_account);
        }
        let root = hash_builder.root();

        let mut node_pool = BTreeMap::new();
        let proof_nodes = hash_builder.take_proof_nodes();
        let mut proofs = BTreeMap::new();
        for (account, proof_key) in proof_keys {
            let proof_node_hashes = insert_proof_nodes(
                &mut node_pool,
                proof_nodes.matching_nodes_sorted(&proof_key),
            );
            proofs.insert(account, proof_node_hashes);
        }
        (root, node_pool, proofs)
    }

    fn assemble_zone_state(
        account_entries: Vec<(Address, TrieAccount)>,
        mut account_reads: Vec<ZoneAccountRead>,
        storage_reads: Vec<ZoneStorageRead>,
        mut node_pool: BTreeMap<B256, Bytes>,
    ) -> ZoneStateWitness {
        let proof_accounts = account_reads
            .iter()
            .map(|read| read.account)
            .collect::<Vec<_>>();
        let (state_root, account_nodes, mut account_proofs) =
            account_trie_with_proofs(&account_entries, &proof_accounts);
        node_pool.extend(account_nodes);

        for read in &mut account_reads {
            read.proof_node_hashes = account_proofs
                .remove(&read.account)
                .expect("account proof was retained for every account read");
        }

        ZoneStateWitness {
            state_root,
            node_pool,
            account_reads,
            storage_reads,
        }
    }

    fn account_read(account: Address, trie_account: TrieAccount) -> ZoneAccountRead {
        ZoneAccountRead {
            account,
            nonce: trie_account.nonce,
            balance: trie_account.balance,
            storage_root: trie_account.storage_root,
            code_hash: trie_account.code_hash,
            code: None,
            proof_node_hashes: Vec::new(),
        }
    }

    fn absent_account_read(account: Address) -> ZoneAccountRead {
        ZoneAccountRead {
            account,
            nonce: 0,
            balance: U256::ZERO,
            storage_root: EMPTY_TRIE_ROOT,
            code_hash: KECCAK_EMPTY,
            code: None,
            proof_node_hashes: Vec::new(),
        }
    }

    struct ZoneStateParts {
        account_entries: Vec<(Address, TrieAccount)>,
        account_reads: Vec<ZoneAccountRead>,
        storage_reads: Vec<ZoneStorageRead>,
        node_pool: BTreeMap<B256, Bytes>,
    }

    impl ZoneStateParts {
        fn assemble(self) -> ZoneStateWitness {
            assemble_zone_state(
                self.account_entries,
                self.account_reads,
                self.storage_reads,
                self.node_pool,
            )
        }

        fn extend(&mut self, other: Self) {
            self.account_entries.extend(other.account_entries);
            self.account_reads.extend(other.account_reads);
            self.storage_reads.extend(other.storage_reads);
            self.node_pool.extend(other.node_pool);
        }
    }

    fn tempo_bound_zone_state(block_number: u64, block_hash: B256) -> ZoneStateWitness {
        tempo_bound_zone_state_with_root(block_number, block_hash, EMPTY_TRIE_ROOT)
    }

    fn tempo_bound_zone_state_with_root(
        block_number: u64,
        block_hash: B256,
        tempo_state_root: B256,
    ) -> ZoneStateWitness {
        base_zone_components(block_number, block_hash, tempo_state_root).assemble()
    }

    fn base_zone_components(
        block_number: u64,
        block_hash: B256,
        tempo_state_root: B256,
    ) -> ZoneStateParts {
        let mut parts = tempo_components_with_root(block_number, block_hash, tempo_state_root);
        parts.extend(system_account_components(
            ZONE_INBOX_ADDRESS,
            &[
                (
                    ZONE_INBOX_PROCESSED_HASH_SLOT,
                    storage_word(B256::repeat_byte(0x44)),
                ),
                (ZONE_INBOX_PROCESSED_NUMBER_SLOT, U256::from(12)),
            ],
            &[
                ZONE_INBOX_PROCESSED_HASH_SLOT,
                ZONE_INBOX_PROCESSED_NUMBER_SLOT,
            ],
        ));
        parts.extend(system_account_components(
            ZONE_OUTBOX_ADDRESS,
            &[
                (
                    ZONE_OUTBOX_LAST_BATCH_HASH_SLOT,
                    storage_word(B256::repeat_byte(0x55)),
                ),
                (ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT, U256::from(4)),
            ],
            &[
                ZONE_OUTBOX_LAST_BATCH_HASH_SLOT,
                ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT,
            ],
        ));
        parts
    }

    fn tempo_components_with_root(
        block_number: u64,
        block_hash: B256,
        tempo_state_root: B256,
    ) -> ZoneStateParts {
        let block_hash_slot = storage_slot_u256(TEMPO_BLOCK_HASH_SLOT);
        let state_root_slot = storage_slot_u256(TEMPO_STATE_ROOT_SLOT);
        let packed_slot = storage_slot_u256(TEMPO_PACKED_SLOT);
        let block_hash_value = storage_word(block_hash);
        let state_root_value = storage_word(tempo_state_root);
        let packed_value = U256::from(block_number);
        let (storage_root, node_pool, mut storage_proofs) = storage_trie_with_entries_and_proofs(
            &[
                (block_hash_slot, block_hash_value),
                (state_root_slot, state_root_value),
                (packed_slot, packed_value),
            ],
            &[block_hash_slot, state_root_slot, packed_slot],
        );
        let trie_account = TrieAccount {
            nonce: 0,
            balance: U256::ZERO,
            storage_root,
            code_hash: KECCAK_EMPTY,
        };
        let storage_reads = vec![
            ZoneStorageRead {
                account: TEMPO_STATE_ADDRESS,
                slot: block_hash_slot,
                value: block_hash_value,
                proof_node_hashes: storage_proofs
                    .remove(&block_hash_slot)
                    .expect("Tempo block hash proof was retained"),
            },
            ZoneStorageRead {
                account: TEMPO_STATE_ADDRESS,
                slot: state_root_slot,
                value: state_root_value,
                proof_node_hashes: storage_proofs
                    .remove(&state_root_slot)
                    .expect("Tempo state root proof was retained"),
            },
            ZoneStorageRead {
                account: TEMPO_STATE_ADDRESS,
                slot: packed_slot,
                value: packed_value,
                proof_node_hashes: storage_proofs
                    .remove(&packed_slot)
                    .expect("Tempo packed slot proof was retained"),
            },
        ];

        ZoneStateParts {
            account_entries: vec![(TEMPO_STATE_ADDRESS, trie_account)],
            account_reads: vec![account_read(TEMPO_STATE_ADDRESS, trie_account)],
            storage_reads,
            node_pool,
        }
    }

    fn system_account_components(
        account: Address,
        entries: &[(U256, U256)],
        proof_slots: &[U256],
    ) -> ZoneStateParts {
        let (storage_root, node_pool, mut storage_proofs) =
            storage_trie_with_entries_and_proofs(entries, proof_slots);
        let trie_account = TrieAccount {
            nonce: 0,
            balance: U256::ZERO,
            storage_root,
            code_hash: KECCAK_EMPTY,
        };
        let storage_reads = proof_slots
            .iter()
            .copied()
            .map(|slot| ZoneStorageRead {
                account,
                slot,
                value: entries
                    .iter()
                    .find(|(entry_slot, _)| *entry_slot == slot)
                    .map(|(_, value)| *value)
                    .expect("proof slot must have a test storage entry"),
                proof_node_hashes: storage_proofs
                    .remove(&slot)
                    .expect("system storage proof was retained"),
            })
            .collect();

        ZoneStateParts {
            account_entries: vec![(account, trie_account)],
            account_reads: vec![account_read(account, trie_account)],
            storage_reads,
            node_pool,
        }
    }

    fn storage_word(value: B256) -> U256 {
        value.into()
    }

    fn l1_state_proof(
        zone_block_index: u64,
        tempo_block_number: u64,
        account: Address,
        slot: U256,
        value: U256,
    ) -> (B256, BatchStateProof) {
        let (account_storage_root, mut node_pool, storage_proof_node_hashes) =
            storage_trie_with_proof(slot, value, slot);
        let trie_account = TrieAccount {
            nonce: 0,
            balance: U256::ZERO,
            storage_root: account_storage_root,
            code_hash: KECCAK_EMPTY,
        };
        let (state_root, account_nodes, mut account_proofs) =
            account_trie_with_proofs(&[(account, trie_account)], &[account]);
        node_pool.extend(account_nodes);
        let account_proof_node_hashes = account_proofs
            .remove(&account)
            .expect("L1 account proof was retained");

        (
            state_root,
            BatchStateProof {
                node_pool,
                reads: vec![L1StateRead {
                    zone_block_index,
                    tempo_block_number,
                    account,
                    account_nonce: trie_account.nonce,
                    account_balance: trie_account.balance,
                    account_storage_root,
                    account_code_hash: trie_account.code_hash,
                    account_proof_node_hashes,
                    slot,
                    value,
                    storage_proof_node_hashes,
                }],
            },
        )
    }

    #[test]
    fn empty_block_fixture_returns_computed_block_transition() {
        let witness = fixture_witness();
        let output = prove_empty_zone_batch(witness.clone()).unwrap();
        let expected_header = ZoneHeader {
            parent_hash: witness.public_inputs.prev_block_hash,
            beneficiary: witness.public_inputs.sequencer,
            state_root: witness.prev_block_header.state_root,
            transactions_root: EMPTY_TRIE_ROOT,
            receipts_root: EMPTY_TRIE_ROOT,
            number: 8,
            timestamp: 12,
            protocol_version: 1,
        };

        assert_eq!(
            output.block_transition.prevBlockHash,
            witness.public_inputs.prev_block_hash
        );
        assert_eq!(
            output.block_transition.nextBlockHash,
            expected_header.hash()
        );
        assert_eq!(
            output.deposit_queue_transition.prevProcessedHash,
            B256::repeat_byte(0x44)
        );
        assert_eq!(
            output.deposit_queue_transition.nextProcessedHash,
            B256::repeat_byte(0x44)
        );
        assert_eq!(output.deposit_queue_transition.prevDepositNumber, 12);
        assert_eq!(output.deposit_queue_transition.nextDepositNumber, 12);
        assert_eq!(output.withdrawal_queue_hash, B256::ZERO);
        assert_eq!(
            output.last_batch_commitment.withdrawal_queue_hash,
            B256::ZERO
        );
        assert_eq!(
            output.last_batch_commitment.withdrawal_batch_index,
            witness.public_inputs.expected_withdrawal_batch_index
        );
    }

    #[test]
    fn rejects_missing_zone_inbox_commitment_read() {
        let mut witness = fixture_witness();
        witness.initial_zone_state.storage_reads.retain(|read| {
            !(read.account == ZONE_INBOX_ADDRESS && read.slot == ZONE_INBOX_PROCESSED_HASH_SLOT)
        });

        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::MissingSystemStorageRead {
                account: ZONE_INBOX_ADDRESS,
                slot: ZONE_INBOX_PROCESSED_HASH_SLOT,
            }
        );
    }

    #[test]
    fn rejects_missing_zone_outbox_last_batch_read() {
        let mut witness = fixture_witness();
        witness.initial_zone_state.storage_reads.retain(|read| {
            !(read.account == ZONE_OUTBOX_ADDRESS && read.slot == ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT)
        });

        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::MissingSystemStorageRead {
                account: ZONE_OUTBOX_ADDRESS,
                slot: ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT,
            }
        );
    }

    #[test]
    fn rejects_zero_expected_withdrawal_batch_index() {
        let mut witness = fixture_witness();
        witness.public_inputs.expected_withdrawal_batch_index = 0;

        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::ExpectedWithdrawalBatchIndexZero
        );
    }

    #[test]
    fn rejects_expected_withdrawal_batch_index_not_extending_proved_last_batch() {
        let mut witness = fixture_witness();
        witness.public_inputs.expected_withdrawal_batch_index = 6;

        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::WithdrawalBatchIndexMismatch {
                expected_previous: 5,
                actual_previous: 4,
            }
        );
    }

    #[test]
    fn rejects_zero_block_identity_batches() {
        let mut witness = fixture_witness();
        witness.zone_blocks.clear();
        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::EmptyBatch
        );
    }

    #[test]
    fn rejects_mismatched_previous_header_hash() {
        let mut witness = fixture_witness();
        witness.public_inputs.prev_block_hash = B256::repeat_byte(0xff);
        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::PrevHeaderHashMismatch
        );
    }

    #[test]
    fn rejects_bad_block_parent_hash() {
        let mut witness = fixture_witness();
        witness.zone_blocks[0].parent_hash = B256::repeat_byte(0xfe);
        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::BlockParentHashMismatch { index: 0 }
        );
    }

    #[test]
    fn rejects_bad_block_number() {
        let mut witness = fixture_witness();
        witness.zone_blocks[0].number = 10;
        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::BlockNumberMismatch {
                index: 0,
                expected: 8,
                actual: 10
            }
        );
    }

    #[test]
    fn rejects_timestamp_regression() {
        let mut witness = fixture_witness();
        witness.zone_blocks[0].timestamp = 10;
        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::BlockTimestampRegression { index: 0 }
        );
    }

    #[test]
    fn rejects_non_sequencer_beneficiary() {
        let mut witness = fixture_witness();
        witness.zone_blocks[0].beneficiary = address!("0x00000000000000000000000000000000000000ff");
        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::BlockBeneficiaryMismatch { index: 0 }
        );
    }

    #[test]
    fn rejects_missing_finalization_on_final_block() {
        let mut witness = fixture_witness();
        witness.zone_blocks[0].finalize_withdrawal_batch_count = None;
        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::MissingFinalWithdrawalFinalization
        );
    }

    #[test]
    fn rejects_intermediate_finalization() {
        let mut witness = fixture_witness();
        let second = ZoneBlock {
            number: 9,
            parent_hash: B256::ZERO,
            timestamp: 13,
            beneficiary: witness.public_inputs.sequencer,
            protocol_version: 1,
            cfg_env: fixture_cfg_env(),
            execution_context: fixture_execution_context(),
            block_env: fixture_block_env(),
            tempo_header_rlp: None,
            deposits: Vec::new(),
            decryptions: Vec::new(),
            enabled_tokens: Vec::new(),
            finalize_withdrawal_batch_count: Some(U256::ZERO),
            finalize_withdrawal_encrypted_senders: Vec::new(),
            transactions: Vec::new(),
        };
        witness.zone_blocks.push(second);
        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::IntermediateWithdrawalFinalization { index: 0 }
        );
    }

    #[test]
    fn verifies_initial_account_and_storage_reads() {
        let account = address!("0x0000000000000000000000000000000000001000");
        let slot = U256::from(7);
        let value = U256::from(9);
        let mut parts = base_zone_components(100, B256::repeat_byte(0x03), EMPTY_TRIE_ROOT);
        let (storage_root, storage_nodes, storage_proof) =
            storage_trie_with_proof(slot, value, slot);
        parts.node_pool.extend(storage_nodes);
        let trie_account = TrieAccount {
            nonce: 3,
            balance: U256::from(4),
            storage_root,
            code_hash: KECCAK_EMPTY,
        };
        parts.account_entries.push((account, trie_account));
        parts
            .account_reads
            .push(account_read(account, trie_account));
        parts.storage_reads.push(ZoneStorageRead {
            account,
            slot,
            value,
            proof_node_hashes: storage_proof,
        });

        let mut witness = fixture_witness();
        set_initial_zone_state(&mut witness, parts.assemble());

        prove_empty_zone_batch(witness).unwrap();
    }

    #[test]
    fn verifies_tempo_state_read_proofs_against_bound_state_root() {
        let account = address!("0x0000000000000000000000000000000000001000");
        let slot = U256::from(7);
        let value = U256::from(9);
        let tempo_block_number = 100;
        let tempo_block_hash = B256::repeat_byte(0x03);
        let (tempo_state_root, proof) = l1_state_proof(0, tempo_block_number, account, slot, value);

        let mut witness = fixture_witness();
        set_initial_zone_state(
            &mut witness,
            tempo_bound_zone_state_with_root(
                tempo_block_number,
                tempo_block_hash,
                tempo_state_root,
            ),
        );
        witness.tempo_state_proofs = proof;

        prove_empty_zone_batch(witness).unwrap();
    }

    #[test]
    fn witness_tempo_state_reader_uses_prepared_verified_reads() {
        let account = address!("0x0000000000000000000000000000000000001000");
        let slot = U256::from(7);
        let value = U256::from(9);
        let tempo_block_number = 100;
        let tempo_block_hash = B256::repeat_byte(0x03);
        let (tempo_state_root, proof) = l1_state_proof(0, tempo_block_number, account, slot, value);

        let mut witness = fixture_witness();
        set_initial_zone_state(
            &mut witness,
            tempo_bound_zone_state_with_root(
                tempo_block_number,
                tempo_block_hash,
                tempo_state_root,
            ),
        );
        witness.tempo_state_proofs = proof;

        let prepared = prepare_stateless_execution(&witness).unwrap();
        let reader = prepared.tempo_state_reader(0).unwrap();
        let call = TempoStateReader::readStorageAtCall {
            account,
            slot: B256::from(slot),
            blockNumber: tempo_block_number,
        }
        .abi_encode();

        assert_eq!(
            reader.call(TEMPO_STATE_ADDRESS, true, &call).unwrap(),
            TempoStateReaderCallResult::Returned {
                gas_used: tempo_state_reader_gas(1).unwrap(),
                output: TempoStateReader::readStorageAtCall::abi_encode_returns(&B256::from(value))
                    .into(),
            }
        );
    }

    #[test]
    fn prepared_execution_rejects_out_of_range_tempo_state_reader() {
        let witness = fixture_witness();
        let prepared = prepare_stateless_execution(&witness).unwrap();

        assert_eq!(
            prepared.tempo_state_reader(1).unwrap_err(),
            ProverError::TempoReaderBlockIndexOutOfRange {
                zone_block_index: 1,
                block_count: 1,
            }
        );
    }

    #[test]
    fn sparse_state_root_calculator_is_seeded_from_initial_zone_witness() {
        let witness = fixture_witness();
        let mut calculator =
            SparseStateRootCalculator::from_zone_state_witness(&witness.initial_zone_state)
                .unwrap();

        let root = calculator
            .calculate(ExecutionPostState::default().into_hashed())
            .unwrap();

        assert_eq!(root.get(), witness.initial_zone_state.state_root);
    }

    #[test]
    fn seeded_sparse_state_root_calculator_can_update_witnessed_storage() {
        let witness = fixture_witness();
        let mut calculator =
            SparseStateRootCalculator::from_zone_state_witness(&witness.initial_zone_state)
                .unwrap();
        let account_read = witness
            .initial_zone_state
            .account_reads
            .iter()
            .find(|read| read.account == ZONE_INBOX_ADDRESS)
            .expect("fixture witnesses zone inbox account");
        let storage_read = witness
            .initial_zone_state
            .storage_reads
            .iter()
            .find(|read| {
                read.account == ZONE_INBOX_ADDRESS && read.slot == ZONE_INBOX_PROCESSED_NUMBER_SLOT
            })
            .expect("fixture witnesses processed deposit number");
        let account_info = AccountInfo {
            balance: account_read.balance,
            nonce: account_read.nonce,
            code_hash: account_read.code_hash,
            code: None,
            account_id: None,
        };
        let mut storage = StorageKeyMap::default();
        storage.insert(
            ZONE_INBOX_PROCESSED_NUMBER_SLOT,
            (storage_read.value, U256::from(99)),
        );
        let bundle = BundleState::builder(0..=0)
            .state_original_account_info(ZONE_INBOX_ADDRESS, account_info.clone())
            .state_present_account_info(ZONE_INBOX_ADDRESS, account_info)
            .state_storage(ZONE_INBOX_ADDRESS, storage)
            .build();

        let root = calculator
            .calculate(ExecutionPostState::from_bundle_state(&bundle).into_hashed())
            .unwrap();

        assert_ne!(root.get(), witness.initial_zone_state.state_root);
    }

    #[test]
    fn sparse_state_root_calculator_rejects_missing_initial_witness_node() {
        let mut witness = fixture_witness();
        let read = witness.initial_zone_state.account_reads[0].clone();
        let missing = read.proof_node_hashes[0];
        witness.initial_zone_state.node_pool.remove(&missing);

        assert_eq!(
            SparseStateRootCalculator::from_zone_state_witness(&witness.initial_zone_state)
                .unwrap_err(),
            ProverError::AccountProofMissing {
                account: read.account,
                node_hash: missing,
            }
        );
    }

    #[test]
    fn prepared_execution_exposes_block_execution_input() {
        let witness = fixture_witness();
        let prepared = prepare_stateless_execution(&witness).unwrap();
        let input = prepared.block_execution_input(0).unwrap();

        assert_eq!(input.block_index, 0);
        assert_eq!(input.block.number, 8);
        assert_eq!(input.evm_env.cfg_env.chain_id, 421_700_001);
        assert_eq!(input.evm_env.cfg_env.spec, TempoHardfork::T1);
        assert_eq!(input.evm_env.block_env.inner.gas_limit, 30_000_000);
        assert_eq!(input.evm_env.block_env.inner.basefee, 0);
        assert_eq!(input.evm_env.block_env.timestamp_millis_part, 0);
        assert_eq!(
            input.execution_context.inner.parent_hash,
            witness.public_inputs.prev_block_hash
        );
        assert_eq!(input.execution_context.inner.tx_count_hint, Some(1));
        assert_eq!(input.execution_context.inner.slot_number, Some(0));
        assert_eq!(input.execution_context.general_gas_limit, 30_000_000);
        assert_eq!(input.execution_context.shared_gas_limit, 3_000_000);
        assert_eq!(input.transactions.len(), 1);
        assert_eq!(
            input.transactions[0].kind,
            PlannedZoneTransactionKind::FinalizeWithdrawalBatch
        );
        assert_eq!(
            input.tempo_state_reader.zone_block_index(),
            0,
            "reader must be bound to the block being executed"
        );
    }

    #[test]
    fn prepared_execution_rejects_mismatched_execution_plan_blocks() {
        let witness = fixture_witness();
        let mut prepared = prepare_stateless_execution(&witness).unwrap();
        prepared.execution_plan.blocks.clear();

        assert_eq!(
            prepared.block_execution_input(0).unwrap_err(),
            ProverError::ExecutionPlanBlockCountMismatch {
                expected: 1,
                actual: 0,
            }
        );
    }

    #[test]
    fn rejects_out_of_range_timestamp_millis_part() {
        let mut witness = fixture_witness();
        witness.zone_blocks[0].block_env.timestamp_millis_part = 1000;

        assert_eq!(
            prepare_stateless_execution(&witness).unwrap_err(),
            ProverError::BlockTimestampMillisPartOutOfRange {
                index: 0,
                actual: 1000,
            }
        );
    }

    #[test]
    fn execute_prepared_blocks_drives_backend_in_order() {
        #[derive(Default)]
        struct RecordingBlockExecutor {
            seen: Vec<(usize, usize)>,
        }

        impl StatelessZoneBlockExecutor for RecordingBlockExecutor {
            type Receipt = TempoReceipt;

            fn execute_block(
                &mut self,
                state: &mut ZoneExecutionState,
                input: &ZoneBlockExecutionInput<'_>,
            ) -> Result<BlockExecutionResult<Self::Receipt>, ProverError> {
                self.seen
                    .push((input.block_index, input.transactions.len()));
                assert_eq!(
                    state
                        .storage(ZONE_INBOX_ADDRESS, ZONE_INBOX_PROCESSED_NUMBER_SLOT)
                        .unwrap(),
                    U256::from(12)
                );

                let info = state
                    .basic(ZONE_INBOX_ADDRESS)
                    .unwrap()
                    .expect("fixture witnesses the zone inbox account");
                let mut account = Account::from(info);
                account.info.balance = U256::from(77);
                account.mark_touch();
                let mut changes = AddressMap::default();
                changes.insert(ZONE_INBOX_ADDRESS, account);
                state.commit(changes);

                Ok(successful_block_result(input.transactions.len()))
            }
        }

        let witness = fixture_witness();
        let prepared = prepare_stateless_execution(&witness).unwrap();
        let mut executor = RecordingBlockExecutor { seen: Vec::new() };

        let output = execute_prepared_blocks(&prepared, &mut executor).unwrap();

        assert_eq!(executor.seen, vec![(0, 1)]);
        assert_eq!(output.blocks.len(), 1);
        let prepared_transactions = prepared
            .block_execution_input(0)
            .unwrap()
            .recovered_transactions();
        assert_eq!(
            output.blocks[0].transactions_root(),
            calculate_transaction_root(&prepared_transactions),
            "transaction root must come from the prepared witness plan"
        );
        assert_ne!(output.blocks[0].state_root(), EMPTY_TRIE_ROOT);
        let hashed_address = KeccakKeyHasher::hash_key(ZONE_INBOX_ADDRESS);
        let hashed_account = output
            .post_state
            .hashed()
            .accounts
            .get(&hashed_address)
            .expect("executor state commit should appear in post-state")
            .expect("executor state commit should not delete account");
        assert_eq!(hashed_account.balance, U256::from(77));
    }

    #[test]
    fn tempo_witness_provider_verifies_reads_against_each_bound_root() {
        let account_0 = address!("0x0000000000000000000000000000000000001000");
        let account_1 = address!("0x0000000000000000000000000000000000002000");
        let slot_0 = U256::from(7);
        let slot_1 = U256::from(8);
        let value_0 = U256::from(9);
        let value_1 = U256::from(10);
        let (root_0, mut proof) = l1_state_proof(0, 100, account_0, slot_0, value_0);
        let (root_1, proof_1) = l1_state_proof(1, 101, account_1, slot_1, value_1);
        proof.node_pool.extend(proof_1.node_pool);
        proof.reads.extend(proof_1.reads);

        let provider = TempoWitnessProvider::new(
            &proof,
            2,
            &[
                TempoRootBinding {
                    block_number: 100,
                    state_root: root_0,
                },
                TempoRootBinding {
                    block_number: 101,
                    state_root: root_1,
                },
            ],
        )
        .unwrap();

        assert_eq!(
            provider
                .read_storage_word(0, 100, account_0, slot_0)
                .unwrap(),
            value_0
        );
        assert_eq!(
            provider
                .read_storage_at(1, 101, account_1, B256::from(slot_1))
                .unwrap(),
            B256::from(value_1)
        );
    }

    #[test]
    fn tempo_witness_provider_rejects_unbound_tempo_root() {
        let account = address!("0x0000000000000000000000000000000000001000");
        let slot = U256::from(7);
        let value = U256::from(9);
        let (_root, proof) = l1_state_proof(0, 101, account, slot, value);

        assert_eq!(
            TempoWitnessProvider::new(
                &proof,
                1,
                &[TempoRootBinding {
                    block_number: 100,
                    state_root: B256::repeat_byte(0x03),
                }],
            )
            .unwrap_err(),
            ProverError::TempoStateReadUnboundTempoRoot {
                read_index: 0,
                tempo_block_number: 101,
            }
        );
    }

    #[test]
    fn tempo_witness_provider_rejects_missing_read() {
        let provider = TempoWitnessProvider::new(
            &BatchStateProof {
                node_pool: BTreeMap::new(),
                reads: Vec::new(),
            },
            1,
            &[TempoRootBinding {
                block_number: 100,
                state_root: EMPTY_TRIE_ROOT,
            }],
        )
        .unwrap();
        let account = address!("0x0000000000000000000000000000000000001000");
        let slot = U256::from(7);

        assert_eq!(
            provider
                .read_storage_word(0, 100, account, slot)
                .unwrap_err(),
            ProverError::MissingTempoStateRead {
                zone_block_index: 0,
                tempo_block_number: 100,
                account,
                slot,
            }
        );
    }

    #[test]
    fn rejects_tempo_state_read_for_out_of_range_zone_block() {
        let account = address!("0x0000000000000000000000000000000000001000");
        let slot = U256::from(7);
        let value = U256::from(9);
        let tempo_block_number = 100;
        let tempo_block_hash = B256::repeat_byte(0x03);
        let (tempo_state_root, proof) = l1_state_proof(1, tempo_block_number, account, slot, value);

        let mut witness = fixture_witness();
        set_initial_zone_state(
            &mut witness,
            tempo_bound_zone_state_with_root(
                tempo_block_number,
                tempo_block_hash,
                tempo_state_root,
            ),
        );
        witness.tempo_state_proofs = proof;

        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::TempoStateReadBlockIndexOutOfRange {
                read_index: 0,
                zone_block_index: 1,
            }
        );
    }

    #[test]
    fn rejects_tempo_state_read_for_wrong_tempo_block() {
        let account = address!("0x0000000000000000000000000000000000001000");
        let slot = U256::from(7);
        let value = U256::from(9);
        let tempo_block_number = 100;
        let tempo_block_hash = B256::repeat_byte(0x03);
        let (tempo_state_root, mut proof) =
            l1_state_proof(0, tempo_block_number, account, slot, value);
        proof.reads[0].tempo_block_number = 101;

        let mut witness = fixture_witness();
        set_initial_zone_state(
            &mut witness,
            tempo_bound_zone_state_with_root(
                tempo_block_number,
                tempo_block_hash,
                tempo_state_root,
            ),
        );
        witness.tempo_state_proofs = proof;

        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::TempoStateReadTempoBlockMismatch {
                read_index: 0,
                expected: 100,
                actual: 101,
            }
        );
    }

    #[test]
    fn rejects_duplicate_tempo_state_reads() {
        let account = address!("0x0000000000000000000000000000000000001000");
        let slot = U256::from(7);
        let value = U256::from(9);
        let tempo_block_number = 100;
        let tempo_block_hash = B256::repeat_byte(0x03);
        let (tempo_state_root, mut proof) =
            l1_state_proof(0, tempo_block_number, account, slot, value);
        proof.reads.push(proof.reads[0].clone());

        let mut witness = fixture_witness();
        set_initial_zone_state(
            &mut witness,
            tempo_bound_zone_state_with_root(
                tempo_block_number,
                tempo_block_hash,
                tempo_state_root,
            ),
        );
        witness.tempo_state_proofs = proof;

        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::DuplicateTempoStateRead { read_index: 1 }
        );
    }

    #[test]
    fn rejects_tempo_state_read_missing_account_proof_node() {
        let account = address!("0x0000000000000000000000000000000000001000");
        let slot = U256::from(7);
        let value = U256::from(9);
        let tempo_block_number = 100;
        let tempo_block_hash = B256::repeat_byte(0x03);
        let (tempo_state_root, mut proof) =
            l1_state_proof(0, tempo_block_number, account, slot, value);
        let missing = proof.reads[0].account_proof_node_hashes[0];
        proof.node_pool.remove(&missing);

        let mut witness = fixture_witness();
        set_initial_zone_state(
            &mut witness,
            tempo_bound_zone_state_with_root(
                tempo_block_number,
                tempo_block_hash,
                tempo_state_root,
            ),
        );
        witness.tempo_state_proofs = proof;

        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::TempoStateAccountProofMissing {
                read_index: 0,
                account,
                node_hash: missing,
            }
        );
    }

    #[test]
    fn rejects_tempo_state_read_missing_storage_proof_node() {
        let account = address!("0x0000000000000000000000000000000000001000");
        let slot = U256::from(7);
        let value = U256::from(9);
        let tempo_block_number = 100;
        let tempo_block_hash = B256::repeat_byte(0x03);
        let (tempo_state_root, mut proof) =
            l1_state_proof(0, tempo_block_number, account, slot, value);
        let missing = proof.reads[0].storage_proof_node_hashes[0];
        proof.node_pool.remove(&missing);

        let mut witness = fixture_witness();
        set_initial_zone_state(
            &mut witness,
            tempo_bound_zone_state_with_root(
                tempo_block_number,
                tempo_block_hash,
                tempo_state_root,
            ),
        );
        witness.tempo_state_proofs = proof;

        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::TempoStateStorageProofMissing {
                read_index: 0,
                account,
                slot,
                node_hash: missing,
            }
        );
    }

    #[test]
    fn rejects_tempo_state_storage_read_mismatch() {
        let account = address!("0x0000000000000000000000000000000000001000");
        let slot = U256::from(7);
        let value = U256::from(9);
        let tempo_block_number = 100;
        let tempo_block_hash = B256::repeat_byte(0x03);
        let (tempo_state_root, mut proof) =
            l1_state_proof(0, tempo_block_number, account, slot, value);
        proof.reads[0].value = U256::from(10);

        let mut witness = fixture_witness();
        set_initial_zone_state(
            &mut witness,
            tempo_bound_zone_state_with_root(
                tempo_block_number,
                tempo_block_hash,
                tempo_state_root,
            ),
        );
        witness.tempo_state_proofs = proof;

        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::TempoStateStorageReadMismatch {
                read_index: 0,
                account,
                slot,
            }
        );
    }

    #[test]
    fn verifies_absent_account_read_with_exclusion_proof() {
        let stored_account = address!("0x0000000000000000000000000000000000001000");
        let absent_account = address!("0x0000000000000000000000000000000000002000");
        let stored = TrieAccount {
            nonce: 1,
            balance: U256::from(2),
            storage_root: EMPTY_TRIE_ROOT,
            code_hash: KECCAK_EMPTY,
        };
        let mut parts = base_zone_components(100, B256::repeat_byte(0x03), EMPTY_TRIE_ROOT);
        parts.account_entries.push((stored_account, stored));
        parts
            .account_reads
            .push(absent_account_read(absent_account));

        let mut witness = fixture_witness();
        set_initial_zone_state(&mut witness, parts.assemble());

        prove_empty_zone_batch(witness).unwrap();
    }

    #[test]
    fn verifies_zero_storage_read_with_exclusion_proof() {
        let account = address!("0x0000000000000000000000000000000000001000");
        let stored_slot = U256::from(7);
        let absent_slot = U256::from(8);
        let mut parts = base_zone_components(100, B256::repeat_byte(0x03), EMPTY_TRIE_ROOT);
        let (storage_root, storage_nodes, storage_proof) =
            storage_trie_with_proof(stored_slot, U256::from(9), absent_slot);
        parts.node_pool.extend(storage_nodes);
        let trie_account = TrieAccount {
            nonce: 0,
            balance: U256::ZERO,
            storage_root,
            code_hash: KECCAK_EMPTY,
        };
        parts.account_entries.push((account, trie_account));
        parts
            .account_reads
            .push(account_read(account, trie_account));
        parts.storage_reads.push(ZoneStorageRead {
            account,
            slot: absent_slot,
            value: U256::ZERO,
            proof_node_hashes: storage_proof,
        });

        let mut witness = fixture_witness();
        set_initial_zone_state(&mut witness, parts.assemble());

        prove_empty_zone_batch(witness).unwrap();
    }

    #[test]
    fn rejects_account_read_mismatch() {
        let account = address!("0x0000000000000000000000000000000000001000");
        let mut parts = base_zone_components(100, B256::repeat_byte(0x03), EMPTY_TRIE_ROOT);
        let trie_account = TrieAccount {
            nonce: 3,
            balance: U256::from(4),
            storage_root: EMPTY_TRIE_ROOT,
            code_hash: KECCAK_EMPTY,
        };
        let mut bad_read = account_read(account, trie_account);
        bad_read.balance = U256::from(5);
        parts.account_entries.push((account, trie_account));
        parts.account_reads.push(bad_read);

        let mut witness = fixture_witness();
        set_initial_zone_state(&mut witness, parts.assemble());

        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::AccountReadMismatch(account)
        );
    }

    #[test]
    fn rejects_missing_account_read_for_storage_read() {
        let account = address!("0x0000000000000000000000000000000000001000");
        let mut witness = fixture_witness();
        witness
            .initial_zone_state
            .storage_reads
            .push(ZoneStorageRead {
                account,
                slot: U256::from(7),
                value: U256::ZERO,
                proof_node_hashes: Vec::new(),
            });

        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::MissingAccountRead(account)
        );
    }

    #[test]
    fn rejects_missing_storage_proof_node() {
        let account = address!("0x0000000000000000000000000000000000001000");
        let slot = U256::from(7);
        let value = U256::from(9);
        let mut parts = base_zone_components(100, B256::repeat_byte(0x03), EMPTY_TRIE_ROOT);
        let (storage_root, _storage_nodes, storage_proof) =
            storage_trie_with_proof(slot, value, slot);
        let trie_account = TrieAccount {
            nonce: 0,
            balance: U256::ZERO,
            storage_root,
            code_hash: KECCAK_EMPTY,
        };
        parts.account_entries.push((account, trie_account));
        parts
            .account_reads
            .push(account_read(account, trie_account));
        parts.storage_reads.push(ZoneStorageRead {
            account,
            slot,
            value,
            proof_node_hashes: storage_proof,
        });

        let mut witness = fixture_witness();
        set_initial_zone_state(&mut witness, parts.assemble());

        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::StorageProofMissing {
                account,
                slot,
                node_hash: storage_root,
            }
        );
    }

    #[test]
    fn rejects_storage_read_mismatch() {
        let account = address!("0x0000000000000000000000000000000000001000");
        let slot = U256::from(7);
        let mut parts = base_zone_components(100, B256::repeat_byte(0x03), EMPTY_TRIE_ROOT);
        let (storage_root, storage_nodes, storage_proof) =
            storage_trie_with_proof(slot, U256::from(9), slot);
        parts.node_pool.extend(storage_nodes);
        let trie_account = TrieAccount {
            nonce: 0,
            balance: U256::ZERO,
            storage_root,
            code_hash: KECCAK_EMPTY,
        };
        parts.account_entries.push((account, trie_account));
        parts
            .account_reads
            .push(account_read(account, trie_account));
        parts.storage_reads.push(ZoneStorageRead {
            account,
            slot,
            value: U256::from(10),
            proof_node_hashes: storage_proof,
        });

        let mut witness = fixture_witness();
        set_initial_zone_state(&mut witness, parts.assemble());

        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::StorageReadMismatch { account, slot }
        );
    }

    #[test]
    fn rejects_bad_code_preimage_before_account_proof_verification() {
        let mut witness = fixture_witness();
        witness
            .initial_zone_state
            .account_reads
            .push(ZoneAccountRead {
                account: address!("0x0000000000000000000000000000000000001000"),
                nonce: 0,
                balance: U256::ZERO,
                storage_root: EMPTY_TRIE_ROOT,
                code_hash: B256::repeat_byte(0xaa),
                code: Some(Bytes::from_static(b"not matching")),
                proof_node_hashes: Vec::new(),
            });
        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::AccountCodeHashMismatch(address!(
                "0x0000000000000000000000000000000000001000"
            ))
        );
    }

    #[test]
    fn rejects_bad_zone_state_node_hash() {
        let mut witness = fixture_witness();
        witness.initial_zone_state.node_pool.insert(
            B256::repeat_byte(0xbb),
            Bytes::from_static(b"bad proof node"),
        );
        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::ZoneStateNodeHashMismatch(B256::repeat_byte(0xbb))
        );
    }

    #[test]
    fn rejects_user_transactions_until_execution_adapter_exists() {
        let mut witness = fixture_witness();
        witness.zone_blocks[0]
            .transactions
            .push(Bytes::from_static(b"signed tx"));
        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::UserTransactionsUnsupported { index: 0 }
        );
    }

    #[test]
    fn rejects_enabled_tokens_until_execution_adapter_exists() {
        let mut witness = fixture_witness();
        witness.zone_blocks[0].enabled_tokens.push(EnabledToken {
            token: address!("0x0000000000000000000000000000000000001000"),
            name: "USD Test".into(),
            symbol: "USDT".into(),
            currency: "USD".into(),
        });
        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::DepositProcessingUnsupported { index: 0 }
        );
    }

    #[test]
    fn rejects_withdrawal_sender_payloads_until_execution_adapter_exists() {
        let mut witness = fixture_witness();
        witness.zone_blocks[0]
            .finalize_withdrawal_encrypted_senders
            .push(Bytes::from_static(b"sender"));
        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::NonZeroWithdrawalFinalizationUnsupported
        );
    }

    #[test]
    fn rejects_missing_tempo_ancestry_header() {
        let mut witness = fixture_witness();
        witness.public_inputs.anchor_block_number = witness
            .public_inputs
            .tempo_block_number
            .checked_add(1)
            .expect("test block number fits u64");
        assert_eq!(
            prove_empty_zone_batch(witness).unwrap_err(),
            ProverError::TempoAncestryLengthMismatch {
                expected: 1,
                actual: 0,
            }
        );
    }
}
