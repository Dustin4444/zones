use alloc::{
    collections::{BTreeMap, BTreeSet},
    vec::Vec,
};

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_rlp::Encodable;
pub use alloy_rpc_types_debug::ExecutionWitness;
use alloy_trie::KECCAK_EMPTY;
use zone_primitives::ZoneHeader;

use crate::{
    BatchWitness, ProverError, ZoneAccountCode, ZoneAccountRead, ZoneStateWitness, ZoneStorageRead,
    validate_node_pool,
};

/// Adapter from the current Zone witness payload to Alloy's upstream
/// [`ExecutionWitness`] shape.
///
/// `ExecutionWitness` carries flat trie-node, bytecode, key-preimage, and
/// header payloads. Until the external Zone witness format is migrated fully,
/// this adapter keeps the decoded read descriptors only as a compatibility
/// layer for strict read validation and no-std fallback maps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneExecutionWitness {
    pre_state_root: B256,
    execution_witness: ExecutionWitness,
    account_reads: Vec<ZoneAccountRead>,
    storage_reads: Vec<ZoneStorageRead>,
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
        validate_node_pool(&state.node_pool, ProverError::ZoneStateNodeHashMismatch)?;
        validate_read_node_refs(state)?;

        let mut codes = BTreeMap::new();
        for read in &state.account_reads {
            match &read.code {
                ZoneAccountCode::Bytecode(code) => {
                    if keccak256(code.as_ref()) != read.code_hash {
                        return Err(ProverError::AccountCodeHashMismatch(read.account));
                    }
                    codes.insert(read.code_hash, code.clone());
                }
                ZoneAccountCode::Empty if read.code_hash != KECCAK_EMPTY => {
                    return Err(ProverError::MissingAccountCode(read.account));
                }
                ZoneAccountCode::Empty => {}
            }
        }

        Ok(Self {
            pre_state_root: state.state_root,
            execution_witness: ExecutionWitness {
                state: state.node_pool.values().cloned().collect(),
                codes: codes.into_values().collect(),
                keys: execution_witness_keys(&state.account_reads, &state.storage_reads),
                headers,
            },
            account_reads: state.account_reads.clone(),
            storage_reads: state.storage_reads.clone(),
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

    pub fn account_reads(&self) -> &[ZoneAccountRead] {
        &self.account_reads
    }

    pub fn storage_reads(&self) -> &[ZoneStorageRead] {
        &self.storage_reads
    }
}

fn validate_read_node_refs(state: &ZoneStateWitness) -> Result<(), ProverError> {
    for read in &state.account_reads {
        for node_hash in &read.proof_node_hashes {
            if !state.node_pool.contains_key(node_hash) {
                return Err(ProverError::AccountProofMissing {
                    account: read.account,
                    node_hash: *node_hash,
                });
            }
        }
    }

    for read in &state.storage_reads {
        for node_hash in &read.proof_node_hashes {
            if !state.node_pool.contains_key(node_hash) {
                return Err(ProverError::StorageProofMissing {
                    account: read.account,
                    slot: read.slot,
                    node_hash: *node_hash,
                });
            }
        }
    }

    Ok(())
}

fn execution_witness_keys(
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
    use alloy_trie::EMPTY_ROOT_HASH;

    #[test]
    fn adapter_exports_upstream_execution_witness_payload() {
        let account = address!("0x00000000000000000000000000000000000000aa");
        let code = Bytes::from_static(&[0x60, 0x00]);
        let code_hash = keccak256(code.as_ref());
        let slot = U256::from(7);
        let header = Bytes::from_static(b"header");

        let state = ZoneStateWitness {
            state_root: EMPTY_ROOT_HASH,
            node_pool: BTreeMap::new(),
            account_reads: vec![ZoneAccountRead {
                account,
                nonce: 1,
                balance: U256::from(2),
                storage_root: EMPTY_ROOT_HASH,
                code_hash,
                code: ZoneAccountCode::Bytecode(code.clone()),
                proof_node_hashes: Vec::new(),
            }],
            storage_reads: vec![ZoneStorageRead {
                account,
                slot,
                value: U256::from(3),
                proof_node_hashes: Vec::new(),
            }],
        };

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
    fn adapter_rejects_missing_proof_node_reference() {
        let account = address!("0x00000000000000000000000000000000000000aa");
        let missing = B256::repeat_byte(0x42);
        let state = ZoneStateWitness {
            state_root: EMPTY_ROOT_HASH,
            node_pool: BTreeMap::new(),
            account_reads: vec![ZoneAccountRead {
                account,
                nonce: 0,
                balance: U256::ZERO,
                storage_root: EMPTY_ROOT_HASH,
                code_hash: KECCAK_EMPTY,
                code: ZoneAccountCode::Empty,
                proof_node_hashes: vec![missing],
            }],
            storage_reads: Vec::new(),
        };

        assert_eq!(
            ZoneExecutionWitness::from_zone_state_witness(&state).unwrap_err(),
            ProverError::AccountProofMissing {
                account,
                node_hash: missing,
            }
        );
    }
}
