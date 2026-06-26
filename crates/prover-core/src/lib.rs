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
use alloy_sol_types::SolValue;
use alloy_trie::EMPTY_ROOT_HASH;
use tempo_zone_contracts::{
    BlockTransition, DecryptionData, DepositQueueTransition, QueuedDeposit,
};
use zone_primitives::{
    ZoneHeader,
    constants::{
        TEMPO_BLOCK_HASH_SLOT, TEMPO_PACKED_SLOT, TEMPO_STATE_ADDRESS, TEMPO_STATE_ROOT_SLOT,
        ZONE_INBOX_ADDRESS, ZONE_INBOX_PROCESSED_HASH_SLOT, ZONE_INBOX_PROCESSED_NUMBER_SLOT,
        ZONE_OUTBOX_ADDRESS, ZONE_OUTBOX_LAST_BATCH_HASH_SLOT, ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT,
    },
};

mod post_state;
mod tempo;
mod trie;
mod witness_db;

pub use post_state::ExecutionPostState;
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
    pub tempo_header_rlp: Option<Bytes>,
    pub deposits: Vec<QueuedDeposit>,
    pub decryptions: Vec<DecryptionData>,
    pub finalize_withdrawal_batch_count: Option<U256>,
    /// Raw transaction bytes. Full EVM re-execution is not yet implemented; any
    /// non-empty transaction list is rejected by the current prover core.
    pub transactions: Vec<Bytes>,
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
    BlockBeneficiaryMismatch {
        index: usize,
    },
    IntermediateWithdrawalFinalization {
        index: usize,
    },
    MissingFinalWithdrawalFinalization,
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
pub fn prove_zone_batch(_witness: BatchWitness) -> Result<BatchOutput, ProverError> {
    Err(ProverError::FullStatelessExecutionUnsupported)
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

    validate_node_pool(
        &witness.initial_zone_state.node_pool,
        ProverError::ZoneStateNodeHashMismatch,
    )?;
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
    if !block.deposits.is_empty() || !block.decryptions.is_empty() {
        return Err(ProverError::DepositProcessingUnsupported { index });
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
struct DepositQueueState {
    processed_hash: B256,
    processed_number: u64,
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
    use alloy_primitives::{B256, address, keccak256};
    use alloy_rlp::Encodable;
    use alloy_trie::{HashBuilder, KECCAK_EMPTY, Nibbles, TrieAccount, proof::ProofRetainer};
    use revm_database_interface::Database;

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
            zone_blocks: vec![ZoneBlock {
                number: 8,
                parent_hash: prev_block_hash,
                timestamp: 12,
                beneficiary: address!("0x0000000000000000000000000000000000000001"),
                protocol_version: 1,
                tempo_header_rlp: None,
                deposits: Vec::new(),
                decryptions: Vec::new(),
                finalize_withdrawal_batch_count: Some(U256::ZERO),
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
            tempo_header_rlp: None,
            deposits: Vec::new(),
            decryptions: Vec::new(),
            finalize_withdrawal_batch_count: Some(U256::ZERO),
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
