use alloc::{
    collections::{BTreeMap, BTreeSet},
    vec::Vec,
};

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_rlp::Encodable;
pub use alloy_rpc_types_debug::ExecutionWitness;
use alloy_trie::TrieAccount;
use zone_primitives::ZoneHeader;

use crate::{BatchWitness, ProverError, ZoneAccountRead, ZoneStateWitness, ZoneStorageRead, trie};

/// Adapter from the current Zone witness payload to Alloy's upstream
/// [`ExecutionWitness`] shape.
///
/// `ExecutionWitness` carries flat trie-node, bytecode, key-preimage, and
/// header payloads. Zone account and storage reads are resolved on demand by
/// walking the trie nodes committed under `pre_state_root`; decoded read
/// descriptors are only local inputs used to derive key preimages while
/// assembling an execution witness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneExecutionWitness {
    pre_state_root: B256,
    execution_witness: ExecutionWitness,
}

impl ZoneExecutionWitness {
    pub fn from_batch_witness(witness: &BatchWitness) -> Result<Self, ProverError> {
        let mut headers = Vec::with_capacity(witness.zone_ancestry_headers.len().saturating_add(1));
        headers.push(encode_zone_header(&witness.prev_block_header));
        headers.extend(witness.zone_ancestry_headers.iter().cloned());
        Self::from_zone_state_witness_with_headers(&witness.initial_zone_state, headers)
    }

    pub fn from_zone_state_witness(state: &ZoneStateWitness) -> Result<Self, ProverError> {
        Self::from_zone_state_witness_with_headers(state, Vec::new())
    }

    pub fn from_zone_state_witness_with_headers(
        state: &ZoneStateWitness,
        headers: Vec<Bytes>,
    ) -> Result<Self, ProverError> {
        let mut execution_witness = state.execution_witness.clone();
        execution_witness.headers = headers;

        Ok(Self {
            pre_state_root: state.state_root,
            execution_witness,
        })
    }

    pub const fn pre_state_root(&self) -> B256 {
        self.pre_state_root
    }

    pub const fn execution_witness(&self) -> &ExecutionWitness {
        &self.execution_witness
    }

    pub fn state_nodes_by_hash(&self) -> BTreeMap<B256, Bytes> {
        self.execution_witness
            .state
            .iter()
            .map(|node| (keccak256(node.as_ref()), node.clone()))
            .collect()
    }

    pub fn account(&self, account: Address) -> Result<Option<TrieAccount>, ProverError> {
        let node_pool = self.state_nodes_by_hash();
        trie::read_account(self.pre_state_root, &node_pool, account)
            .map_err(|err| account_read_error(account, err))
    }

    pub fn storage(&self, account: Address, slot: U256) -> Result<U256, ProverError> {
        let Some(account_state) = self.account(account)? else {
            return Ok(U256::ZERO);
        };
        if account_state.storage_root == alloy_trie::EMPTY_ROOT_HASH {
            return Ok(U256::ZERO);
        }
        let node_pool = self.state_nodes_by_hash();
        trie::read_storage(account_state.storage_root, &node_pool, slot)
            .map_err(|err| storage_read_error(account, slot, err))
    }
}

fn account_read_error(account: Address, err: trie::TrieProofError) -> ProverError {
    match err {
        trie::TrieProofError::MissingNode(node_hash) => {
            ProverError::AccountProofMissing { account, node_hash }
        }
        trie::TrieProofError::Invalid => ProverError::AccountProofInvalid(account),
        trie::TrieProofError::ValueMismatch => ProverError::AccountReadMismatch(account),
    }
}

fn storage_read_error(account: Address, slot: U256, err: trie::TrieProofError) -> ProverError {
    match err {
        trie::TrieProofError::MissingNode(node_hash) => ProverError::StorageProofMissing {
            account,
            slot,
            node_hash,
        },
        trie::TrieProofError::Invalid => ProverError::StorageProofInvalid { account, slot },
        trie::TrieProofError::ValueMismatch => ProverError::StorageReadMismatch { account, slot },
    }
}

pub(crate) fn execution_witness_keys(
    account_reads: &[ZoneAccountRead],
    storage_reads: &[ZoneStorageRead],
) -> Vec<Bytes> {
    let mut keys = BTreeSet::new();
    for read in account_reads {
        keys.insert(address_key(read.account));
    }
    for read in storage_reads {
        keys.insert(storage_key(read.slot));
    }
    keys.into_iter().map(Bytes::from).collect()
}

fn address_key(address: Address) -> Vec<u8> {
    address.as_slice().to_vec()
}

fn storage_key(slot: U256) -> Vec<u8> {
    slot.to_be_bytes::<32>().to_vec()
}

fn encode_zone_header(header: &ZoneHeader) -> Bytes {
    let mut encoded = Vec::with_capacity(header.length());
    header.encode(&mut encoded);
    Bytes::from(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloy_primitives::address;
    use alloy_trie::{EMPTY_ROOT_HASH, KECCAK_EMPTY};

    #[test]
    fn adapter_exports_upstream_execution_witness_payload() {
        let account = address!("0x00000000000000000000000000000000000000aa");
        let code = Bytes::from_static(&[0x60, 0x00]);
        let code_hash = keccak256(code.as_ref());
        let slot = U256::from(7);
        let header = Bytes::from_static(b"header");

        let account_reads = vec![ZoneAccountRead {
            account,
            nonce: 1,
            balance: U256::from(2),
            storage_root: EMPTY_ROOT_HASH,
            code_hash,
        }];
        let storage_reads = vec![ZoneStorageRead {
            account,
            slot,
            value: U256::from(3),
        }];
        let state = ZoneStateWitness::from_node_pool(
            EMPTY_ROOT_HASH,
            BTreeMap::new(),
            account_reads,
            storage_reads,
            vec![code.clone()],
        );

        let witness = ZoneExecutionWitness::from_zone_state_witness_with_headers(
            &state,
            vec![header.clone()],
        )
        .unwrap();

        assert_eq!(witness.pre_state_root(), EMPTY_ROOT_HASH);
        assert_eq!(witness.execution_witness().state, Vec::<Bytes>::new());
        assert_eq!(witness.execution_witness().codes, vec![code]);
        assert_eq!(witness.execution_witness().headers, vec![header]);
        assert!(
            witness
                .execution_witness()
                .keys
                .contains(&Bytes::copy_from_slice(account.as_slice()))
        );
        assert!(
            witness
                .execution_witness()
                .keys
                .contains(&Bytes::copy_from_slice(&slot.to_be_bytes::<32>()))
        );
    }

    #[test]
    fn adapter_does_not_trust_descriptor_key_lists() {
        let account = address!("0x00000000000000000000000000000000000000aa");
        let slot = U256::from(7);
        let mut state = ZoneStateWitness::from_node_pool(
            EMPTY_ROOT_HASH,
            BTreeMap::new(),
            vec![ZoneAccountRead {
                account,
                nonce: 0,
                balance: U256::ZERO,
                storage_root: EMPTY_ROOT_HASH,
                code_hash: KECCAK_EMPTY,
            }],
            vec![ZoneStorageRead {
                account,
                slot,
                value: U256::ZERO,
            }],
            Vec::new(),
        );
        state
            .execution_witness
            .keys
            .retain(|key| key.as_ref() != slot.to_be_bytes::<32>().as_slice());

        ZoneExecutionWitness::from_zone_state_witness(&state).unwrap();
    }
}
