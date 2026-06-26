use alloc::{collections::BTreeMap, vec::Vec};

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_rlp::{Decodable, Encodable};
use alloy_trie::{
    EMPTY_ROOT_HASH, KECCAK_EMPTY, Nibbles, TrieAccount,
    nodes::{CHILD_INDEX_RANGE, TrieNode},
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

pub(crate) fn proof_subtrie_for_key(
    key: Nibbles,
    node_pool: &BTreeMap<B256, Bytes>,
    proof_node_hashes: &[B256],
) -> Result<alloy_trie::proof::ProofNodes, TrieProofError> {
    let proof = proof_nodes(proof_node_hashes, node_pool).map_err(TrieProofError::MissingNode)?;
    let mut subtrie = alloy_trie::proof::ProofNodes::default();
    let mut path = Nibbles::new();

    for node in proof {
        subtrie.insert(path, node.clone());
        let node = TrieNode::decode(&mut &node[..]).map_err(|_| TrieProofError::Invalid)?;
        advance_proof_path(node, &mut path, &key)?;
    }

    Ok(subtrie)
}

fn advance_proof_path(
    node: TrieNode,
    path: &mut Nibbles,
    key: &Nibbles,
) -> Result<(), TrieProofError> {
    match node {
        TrieNode::Branch(mut branch) => {
            let Some(next) = key.get(path.len()) else {
                return Ok(());
            };
            let mut stack_ptr = branch.as_ref().first_child_index();
            for index in CHILD_INDEX_RANGE {
                if branch.state_mask.is_bit_set(index) {
                    if index == next {
                        path.push(next);
                        let child = branch.stack.remove(stack_ptr);
                        if child.is_hash() {
                            return Ok(());
                        }
                        let child = TrieNode::decode(&mut &child[..])
                            .map_err(|_| TrieProofError::Invalid)?;
                        return advance_proof_path(child, path, key);
                    }
                    stack_ptr += 1;
                }
            }
            Ok(())
        }
        TrieNode::Extension(extension) => {
            path.extend(&extension.key);
            if extension.child.is_hash() {
                return Ok(());
            }
            let child =
                TrieNode::decode(&mut &extension.child[..]).map_err(|_| TrieProofError::Invalid)?;
            advance_proof_path(child, path, key)
        }
        TrieNode::Leaf(leaf) => {
            path.extend(&leaf.key);
            Ok(())
        }
        TrieNode::EmptyRoot => Ok(()),
    }
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
