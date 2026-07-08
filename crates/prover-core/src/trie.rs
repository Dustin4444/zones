use alloc::{collections::BTreeMap, vec::Vec};

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_rlp::{Decodable, Encodable};
use alloy_trie::{
    EMPTY_ROOT_HASH, KECCAK_EMPTY, Nibbles, TrieAccount,
    nodes::{CHILD_INDEX_RANGE, TrieNode},
    proof::{ProofVerificationError, verify_proof},
};

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
    pub node_pool: &'a BTreeMap<B256, Bytes>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct StorageProof<'a> {
    pub slot: U256,
    pub value: U256,
    pub node_pool: &'a BTreeMap<B256, Bytes>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrieProofError {
    MissingNode(B256),
    Invalid,
    ValueMismatch,
}

pub(crate) fn verify_account_proof(
    state_root: B256,
    read: AccountProof<'_>,
) -> Result<ProvenAccount, TrieProofError> {
    let key = Nibbles::unpack(keccak256(read.account));
    let expected_value = expected_account_value(read);
    let proof = proof_nodes_for_key(state_root, read.node_pool, &key)?;

    verify_proof(state_root, key, expected_value, proof)
        .map_err(|err| proof_verification_error(err, key))?;
    Ok(ProvenAccount {
        storage_root: read.storage_root,
    })
}

pub(crate) fn verify_storage_proof(
    storage_root: B256,
    read: StorageProof<'_>,
) -> Result<(), TrieProofError> {
    let key = Nibbles::unpack(keccak256(read.slot.to_be_bytes::<32>()));
    let expected_value = expected_storage_value(read.value);
    let proof = proof_nodes_for_key(storage_root, read.node_pool, &key)?;

    verify_proof(storage_root, key, expected_value, proof)
        .map_err(|err| proof_verification_error(err, key))
}

pub(crate) fn read_account(
    state_root: B256,
    node_pool: &BTreeMap<B256, Bytes>,
    account: Address,
) -> Result<Option<TrieAccount>, TrieProofError> {
    let key = Nibbles::unpack(keccak256(account));
    let Some(value) = trie_value_for_key(state_root, node_pool, &key)? else {
        return Ok(None);
    };
    TrieAccount::decode(&mut value.as_slice())
        .map(Some)
        .map_err(|_| TrieProofError::Invalid)
}

pub(crate) fn read_storage(
    storage_root: B256,
    node_pool: &BTreeMap<B256, Bytes>,
    slot: U256,
) -> Result<U256, TrieProofError> {
    let key = Nibbles::unpack(keccak256(slot.to_be_bytes::<32>()));
    let Some(value) = trie_value_for_key(storage_root, node_pool, &key)? else {
        return Ok(U256::ZERO);
    };
    U256::decode(&mut value.as_slice()).map_err(|_| TrieProofError::Invalid)
}

fn trie_value_for_key(
    root: B256,
    node_pool: &BTreeMap<B256, Bytes>,
    key: &Nibbles,
) -> Result<Option<Vec<u8>>, TrieProofError> {
    if root == EMPTY_ROOT_HASH {
        return Ok(None);
    }

    let node = node_pool
        .get(&root)
        .ok_or(TrieProofError::MissingNode(root))?;
    let node = TrieNode::decode(&mut &node[..]).map_err(|_| TrieProofError::Invalid)?;
    let mut path = Nibbles::new();
    trie_value_from_node(node, node_pool, &mut path, key)
}

fn trie_value_from_node(
    node: TrieNode,
    node_pool: &BTreeMap<B256, Bytes>,
    path: &mut Nibbles,
    key: &Nibbles,
) -> Result<Option<Vec<u8>>, TrieProofError> {
    match node {
        TrieNode::Branch(branch) => {
            let Some(next) = key.get(path.len()) else {
                return Ok(None);
            };
            let mut stack_ptr = branch.as_ref().first_child_index();
            for index in CHILD_INDEX_RANGE {
                if branch.state_mask.is_bit_set(index) {
                    if index == next {
                        path.push(next);
                        let child = branch.stack[stack_ptr].clone();
                        if child.is_hash() {
                            let child_hash = B256::from_slice(&child[1..]);
                            let child = node_pool
                                .get(&child_hash)
                                .ok_or(TrieProofError::MissingNode(child_hash))?;
                            let child = TrieNode::decode(&mut &child[..])
                                .map_err(|_| TrieProofError::Invalid)?;
                            return trie_value_from_node(child, node_pool, path, key);
                        }
                        let child = TrieNode::decode(&mut &child[..])
                            .map_err(|_| TrieProofError::Invalid)?;
                        return trie_value_from_node(child, node_pool, path, key);
                    }
                    stack_ptr += 1;
                }
            }
            Ok(None)
        }
        TrieNode::Extension(extension) => {
            path.extend(&extension.key);
            if !key.starts_with(path) {
                return Ok(None);
            }
            if extension.child.is_hash() {
                let child_hash = B256::from_slice(&extension.child[1..]);
                let child = node_pool
                    .get(&child_hash)
                    .ok_or(TrieProofError::MissingNode(child_hash))?;
                let child =
                    TrieNode::decode(&mut &child[..]).map_err(|_| TrieProofError::Invalid)?;
                return trie_value_from_node(child, node_pool, path, key);
            }
            let child =
                TrieNode::decode(&mut &extension.child[..]).map_err(|_| TrieProofError::Invalid)?;
            trie_value_from_node(child, node_pool, path, key)
        }
        TrieNode::Leaf(leaf) => {
            path.extend(&leaf.key);
            if path == key {
                Ok(Some(leaf.value))
            } else {
                Ok(None)
            }
        }
        TrieNode::EmptyRoot => Ok(None),
    }
}

fn proof_nodes_for_key<'a>(
    root: B256,
    node_pool: &'a BTreeMap<B256, Bytes>,
    key: &Nibbles,
) -> Result<Vec<&'a Bytes>, TrieProofError> {
    if root == EMPTY_ROOT_HASH {
        return Ok(Vec::new());
    }

    let mut proof = Vec::new();
    let mut next_hash = root;
    let mut path = Nibbles::new();

    loop {
        let node = node_pool
            .get(&next_hash)
            .ok_or(TrieProofError::MissingNode(next_hash))?;
        proof.push(node);
        let node = TrieNode::decode(&mut &node[..]).map_err(|_| TrieProofError::Invalid)?;
        let Some(hash) = next_child_hash_for_key(node, &mut path, key)? else {
            return Ok(proof);
        };
        next_hash = hash;
    }
}

fn next_child_hash_for_key(
    node: TrieNode,
    path: &mut Nibbles,
    key: &Nibbles,
) -> Result<Option<B256>, TrieProofError> {
    match node {
        TrieNode::Branch(branch) => {
            let Some(next) = key.get(path.len()) else {
                return Ok(None);
            };
            let mut stack_ptr = branch.as_ref().first_child_index();
            for index in CHILD_INDEX_RANGE {
                if branch.state_mask.is_bit_set(index) {
                    if index == next {
                        path.push(next);
                        let child = branch.stack[stack_ptr].clone();
                        if child.is_hash() {
                            return Ok(Some(B256::from_slice(&child[1..])));
                        }
                        let child = TrieNode::decode(&mut &child[..])
                            .map_err(|_| TrieProofError::Invalid)?;
                        return next_child_hash_for_key(child, path, key);
                    }
                    stack_ptr += 1;
                }
            }
            Ok(None)
        }
        TrieNode::Extension(extension) => {
            path.extend(&extension.key);
            if !key.starts_with(path) {
                return Ok(None);
            }
            if extension.child.is_hash() {
                return Ok(Some(B256::from_slice(&extension.child[1..])));
            }
            let child =
                TrieNode::decode(&mut &extension.child[..]).map_err(|_| TrieProofError::Invalid)?;
            next_child_hash_for_key(child, path, key)
        }
        TrieNode::Leaf(leaf) => {
            path.extend(&leaf.key);
            Ok(None)
        }
        TrieNode::EmptyRoot => Ok(None),
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
