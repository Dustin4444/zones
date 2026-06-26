use alloc::{collections::BTreeMap, vec::Vec};

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_rlp::Encodable;
use alloy_trie::{
    EMPTY_ROOT_HASH, KECCAK_EMPTY, Nibbles, TrieAccount,
    proof::{ProofVerificationError, verify_proof},
};

use crate::{ProverError, ZoneAccountRead, ZoneStorageRead};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProvenAccount {
    pub storage_root: B256,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AccountProof<'a> {
    pub account: Address,
    pub nonce: u64,
    pub balance: U256,
    pub storage_root: B256,
    pub code_hash: B256,
    pub proof_node_hashes: &'a [B256],
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct StorageProof<'a> {
    pub slot: U256,
    pub value: U256,
    pub proof_node_hashes: &'a [B256],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrieProofError {
    MissingNode(B256),
    Invalid,
    ValueMismatch,
}

pub(crate) fn verify_account_read(
    state_root: B256,
    node_pool: &BTreeMap<B256, Bytes>,
    read: &ZoneAccountRead,
) -> Result<ProvenAccount, ProverError> {
    let proof = AccountProof {
        account: read.account,
        nonce: read.nonce,
        balance: read.balance,
        storage_root: read.storage_root,
        code_hash: read.code_hash,
        proof_node_hashes: &read.proof_node_hashes,
    };
    verify_account_proof(state_root, node_pool, proof).map_err(|err| match err {
        TrieProofError::MissingNode(node_hash) => ProverError::AccountProofMissing {
            account: read.account,
            node_hash,
        },
        TrieProofError::Invalid => ProverError::AccountProofInvalid(read.account),
        TrieProofError::ValueMismatch => ProverError::AccountReadMismatch(read.account),
    })
}

pub(crate) fn verify_storage_read(
    storage_root: B256,
    node_pool: &BTreeMap<B256, Bytes>,
    read: &ZoneStorageRead,
) -> Result<(), ProverError> {
    let proof = StorageProof {
        slot: read.slot,
        value: read.value,
        proof_node_hashes: &read.proof_node_hashes,
    };
    verify_storage_proof(storage_root, node_pool, proof).map_err(|err| match err {
        TrieProofError::MissingNode(node_hash) => ProverError::StorageProofMissing {
            account: read.account,
            slot: read.slot,
            node_hash,
        },
        TrieProofError::Invalid => ProverError::StorageProofInvalid {
            account: read.account,
            slot: read.slot,
        },
        TrieProofError::ValueMismatch => ProverError::StorageReadMismatch {
            account: read.account,
            slot: read.slot,
        },
    })
}

pub(crate) fn verify_account_proof(
    state_root: B256,
    node_pool: &BTreeMap<B256, Bytes>,
    read: AccountProof<'_>,
) -> Result<ProvenAccount, TrieProofError> {
    let key = Nibbles::unpack(keccak256(read.account));
    let expected_value = expected_account_value(read);
    let proof =
        proof_nodes(read.proof_node_hashes, node_pool).map_err(TrieProofError::MissingNode)?;

    verify_proof(state_root, key, expected_value, proof)
        .map_err(|err| proof_verification_error(err, key))?;
    Ok(ProvenAccount {
        storage_root: read.storage_root,
    })
}

pub(crate) fn verify_storage_proof(
    storage_root: B256,
    node_pool: &BTreeMap<B256, Bytes>,
    read: StorageProof<'_>,
) -> Result<(), TrieProofError> {
    let key = Nibbles::unpack(keccak256(read.slot.to_be_bytes::<32>()));
    let expected_value = expected_storage_value(read.value);
    let proof =
        proof_nodes(read.proof_node_hashes, node_pool).map_err(TrieProofError::MissingNode)?;

    verify_proof(storage_root, key, expected_value, proof)
        .map_err(|err| proof_verification_error(err, key))
}

fn proof_nodes<'a>(
    proof_node_hashes: &[B256],
    node_pool: &'a BTreeMap<B256, Bytes>,
) -> Result<Vec<&'a Bytes>, B256> {
    let mut proof = Vec::with_capacity(proof_node_hashes.len());
    for hash in proof_node_hashes {
        proof.push(node_pool.get(hash).ok_or(*hash)?);
    }
    Ok(proof)
}

fn expected_account_value(read: AccountProof<'_>) -> Option<Vec<u8>> {
    if read.nonce == 0
        && read.balance.is_zero()
        && read.storage_root == EMPTY_ROOT_HASH
        && read.code_hash == KECCAK_EMPTY
    {
        return None;
    }

    let account = TrieAccount {
        nonce: read.nonce,
        balance: read.balance,
        storage_root: read.storage_root,
        code_hash: read.code_hash,
    };
    let mut encoded = Vec::new();
    account.encode(&mut encoded);
    Some(encoded)
}

fn expected_storage_value(value: U256) -> Option<Vec<u8>> {
    if value.is_zero() {
        None
    } else {
        Some(alloy_rlp::encode_fixed_size(&value).to_vec())
    }
}

fn proof_verification_error(err: ProofVerificationError, key: Nibbles) -> TrieProofError {
    match err {
        ProofVerificationError::ValueMismatch { path, .. } if path == key => {
            TrieProofError::ValueMismatch
        }
        _ => TrieProofError::Invalid,
    }
}
