use std::collections::BTreeMap;

use alloy_primitives::{Address, B256, Bytes, U256, address, keccak256};
use alloy_rlp::Encodable;
use alloy_trie::{HashBuilder, KECCAK_EMPTY, Nibbles, TrieAccount, proof::ProofRetainer};
use zone_primitives::{
    ZoneHeader,
    constants::{
        TEMPO_BLOCK_HASH_SLOT, TEMPO_PACKED_SLOT, TEMPO_STATE_ADDRESS, TEMPO_STATE_ROOT_SLOT,
        ZONE_INBOX_ADDRESS, ZONE_INBOX_PROCESSED_HASH_SLOT, ZONE_INBOX_PROCESSED_NUMBER_SLOT,
        ZONE_OUTBOX_ADDRESS, ZONE_OUTBOX_LAST_BATCH_HASH_SLOT, ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT,
    },
};

use crate::types::{
    BatchStateProof, BatchWitness, EMPTY_TRIE_ROOT, PublicInputs, TempoHardfork, ZoneAccountRead,
    ZoneBlock, ZoneBlockEnvWitness, ZoneBlockExecutionContextWitness, ZoneCfgEnvWitness,
    ZoneStateWitness, ZoneStorageRead, ZoneTempoImport, ZoneWithdrawalFinalization,
};

const TEST_INBOX_CONTRACT_CODE: &[u8] = &[0x00];
const ZONE_OUTBOX_PACKED_SLOT: U256 = U256::ZERO;
const ZONE_OUTBOX_PENDING_WITHDRAWALS_SLOT: U256 = U256::from_limbs([3, 0, 0, 0]);
const ZONE_OUTBOX_PENDING_WITHDRAWALS_HEAD_SLOT: U256 = U256::from_limbs([4, 0, 0, 0]);

pub(crate) fn minimal_batch_witness() -> BatchWitness {
    let tempo_block_number = 10;
    let tempo_block_hash = B256::repeat_byte(0x33);
    let beneficiary = address!("0x0000000000000000000000000000000000000001");
    let initial_zone_state =
        tempo_bound_zone_state(tempo_block_number, tempo_block_hash, beneficiary);
    let header = ZoneHeader {
        parent_hash: B256::repeat_byte(0x01),
        beneficiary,
        state_root: initial_zone_state.state_root,
        transactions_root: EMPTY_TRIE_ROOT,
        receipts_root: EMPTY_TRIE_ROOT,
        number: 1,
        timestamp: 1,
        protocol_version: 1,
    };
    let prev_block_hash = header.hash();

    BatchWitness {
        public_inputs: PublicInputs {
            prev_block_hash,
            tempo_block_number,
            anchor_block_number: tempo_block_number,
            anchor_block_hash: tempo_block_hash,
            expected_withdrawal_batch_index: 1,
            sequencer: beneficiary,
        },
        prev_block_header: header,
        zone_ancestry_headers: Vec::new(),
        zone_blocks: vec![ZoneBlock {
            number: 2,
            parent_hash: prev_block_hash,
            timestamp: 2,
            beneficiary,
            protocol_version: 1,
            cfg_env: ZoneCfgEnvWitness {
                chain_id: 421_700_001,
                spec: TempoHardfork::T1,
                enable_amsterdam_eip8037: false,
            },
            execution_context: ZoneBlockExecutionContextWitness {
                parent_beacon_block_root: B256::ZERO,
                extra_data: Bytes::new(),
            },
            block_env: ZoneBlockEnvWitness {
                gas_limit: 30_000_000,
                basefee: 0,
                difficulty: U256::ZERO,
                prevrandao: B256::ZERO,
                slot_num: 0,
                timestamp_millis_part: 0,
            },
            tempo_import: ZoneTempoImport::none(),
            withdrawal_finalization: ZoneWithdrawalFinalization::finalize(U256::ZERO, Vec::new()),
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

fn genesis_predeploy_code(addr: Address) -> Bytes {
    let genesis: serde_json::Value = serde_json::from_str(include_str!(
        "../../node/tests/assets/zone-test-genesis.json"
    ))
    .expect("zone test genesis should parse");
    let key = format!("{addr:#x}");
    let code = genesis["alloc"][&key]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("missing code for {key} in zone-test-genesis.json"));
    Bytes::from(
        const_hex::decode(code.strip_prefix("0x").unwrap_or(code))
            .expect("genesis bytecode should decode"),
    )
}

fn absent_account_read(account: Address) -> ZoneAccountRead {
    ZoneAccountRead {
        account,
        nonce: 0,
        balance: U256::ZERO,
        storage_root: EMPTY_TRIE_ROOT,
        code_hash: KECCAK_EMPTY,
    }
}

fn tempo_bound_zone_state(
    block_number: u64,
    block_hash: B256,
    beneficiary: Address,
) -> ZoneStateWitness {
    let block_hash_slot = storage_slot_u256(TEMPO_BLOCK_HASH_SLOT);
    let state_root_slot = storage_slot_u256(TEMPO_STATE_ROOT_SLOT);
    let packed_slot = storage_slot_u256(TEMPO_PACKED_SLOT);
    let block_hash_value = storage_word(block_hash);
    let state_root_value = storage_word(EMPTY_TRIE_ROOT);
    let packed_value = U256::from(block_number);
    let (tempo_storage_root, mut node_pool, _) = storage_trie_with_entries_and_proofs(
        &[
            (block_hash_slot, block_hash_value),
            (state_root_slot, state_root_value),
            (packed_slot, packed_value),
        ],
        &[block_hash_slot, state_root_slot, packed_slot],
    );
    let (zone_inbox_storage_root, zone_inbox_nodes, _) = storage_trie_with_entries_and_proofs(
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
    );
    node_pool.extend(zone_inbox_nodes);
    let (zone_outbox_storage_root, zone_outbox_nodes, _) = storage_trie_with_entries_and_proofs(
        &[
            (
                ZONE_OUTBOX_LAST_BATCH_HASH_SLOT,
                storage_word(B256::repeat_byte(0x55)),
            ),
            (ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT, U256::ZERO),
        ],
        &[
            ZONE_OUTBOX_PACKED_SLOT,
            ZONE_OUTBOX_LAST_BATCH_HASH_SLOT,
            ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT,
            ZONE_OUTBOX_PENDING_WITHDRAWALS_SLOT,
            ZONE_OUTBOX_PENDING_WITHDRAWALS_HEAD_SLOT,
        ],
    );
    node_pool.extend(zone_outbox_nodes);

    let tempo_trie_account = TrieAccount {
        nonce: 0,
        balance: U256::ZERO,
        storage_root: tempo_storage_root,
        code_hash: KECCAK_EMPTY,
    };
    let inbox_code_hash = keccak256(TEST_INBOX_CONTRACT_CODE);
    let outbox_code = genesis_predeploy_code(ZONE_OUTBOX_ADDRESS);
    let outbox_code_hash = keccak256(outbox_code.as_ref());
    let zone_inbox_trie_account = TrieAccount {
        nonce: 0,
        balance: U256::ZERO,
        storage_root: zone_inbox_storage_root,
        code_hash: inbox_code_hash,
    };
    let zone_outbox_trie_account = TrieAccount {
        nonce: 0,
        balance: U256::ZERO,
        storage_root: zone_outbox_storage_root,
        code_hash: outbox_code_hash,
    };
    let tempo_account_read = ZoneAccountRead {
        account: TEMPO_STATE_ADDRESS,
        nonce: tempo_trie_account.nonce,
        balance: tempo_trie_account.balance,
        storage_root: tempo_trie_account.storage_root,
        code_hash: tempo_trie_account.code_hash,
    };
    let zone_inbox_account_read = ZoneAccountRead {
        account: ZONE_INBOX_ADDRESS,
        nonce: zone_inbox_trie_account.nonce,
        balance: zone_inbox_trie_account.balance,
        storage_root: zone_inbox_trie_account.storage_root,
        code_hash: zone_inbox_trie_account.code_hash,
    };
    let zone_outbox_account_read = ZoneAccountRead {
        account: ZONE_OUTBOX_ADDRESS,
        nonce: zone_outbox_trie_account.nonce,
        balance: zone_outbox_trie_account.balance,
        storage_root: zone_outbox_trie_account.storage_root,
        code_hash: zone_outbox_trie_account.code_hash,
    };
    let mut storage_reads = vec![
        ZoneStorageRead {
            account: TEMPO_STATE_ADDRESS,
            slot: block_hash_slot,
            value: block_hash_value,
        },
        ZoneStorageRead {
            account: TEMPO_STATE_ADDRESS,
            slot: state_root_slot,
            value: state_root_value,
        },
        ZoneStorageRead {
            account: TEMPO_STATE_ADDRESS,
            slot: packed_slot,
            value: packed_value,
        },
    ];
    storage_reads.extend([
        ZoneStorageRead {
            account: ZONE_INBOX_ADDRESS,
            slot: ZONE_INBOX_PROCESSED_HASH_SLOT,
            value: storage_word(B256::repeat_byte(0x44)),
        },
        ZoneStorageRead {
            account: ZONE_INBOX_ADDRESS,
            slot: ZONE_INBOX_PROCESSED_NUMBER_SLOT,
            value: U256::from(12),
        },
        ZoneStorageRead {
            account: ZONE_OUTBOX_ADDRESS,
            slot: ZONE_OUTBOX_PACKED_SLOT,
            value: U256::ZERO,
        },
        ZoneStorageRead {
            account: ZONE_OUTBOX_ADDRESS,
            slot: ZONE_OUTBOX_LAST_BATCH_HASH_SLOT,
            value: storage_word(B256::repeat_byte(0x55)),
        },
        ZoneStorageRead {
            account: ZONE_OUTBOX_ADDRESS,
            slot: ZONE_OUTBOX_LAST_BATCH_INDEX_SLOT,
            value: U256::ZERO,
        },
        ZoneStorageRead {
            account: ZONE_OUTBOX_ADDRESS,
            slot: ZONE_OUTBOX_PENDING_WITHDRAWALS_SLOT,
            value: U256::ZERO,
        },
        ZoneStorageRead {
            account: ZONE_OUTBOX_ADDRESS,
            slot: ZONE_OUTBOX_PENDING_WITHDRAWALS_HEAD_SLOT,
            value: U256::ZERO,
        },
    ]);

    let (state_root, account_nodes, _) = account_trie_with_proofs(
        &[
            (TEMPO_STATE_ADDRESS, tempo_trie_account),
            (ZONE_INBOX_ADDRESS, zone_inbox_trie_account),
            (ZONE_OUTBOX_ADDRESS, zone_outbox_trie_account),
        ],
        &[
            TEMPO_STATE_ADDRESS,
            ZONE_INBOX_ADDRESS,
            ZONE_OUTBOX_ADDRESS,
            beneficiary,
        ],
    );
    node_pool.extend(account_nodes);

    let account_reads = vec![
        tempo_account_read,
        zone_inbox_account_read,
        zone_outbox_account_read,
        absent_account_read(beneficiary),
    ];

    ZoneStateWitness::from_node_pool(
        state_root,
        node_pool,
        account_reads,
        storage_reads,
        vec![
            Bytes::copy_from_slice(TEST_INBOX_CONTRACT_CODE),
            outbox_code,
        ],
    )
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
                key_hash,
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
            (key_hash, Nibbles::unpack(key_hash), encoded_account)
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

fn storage_slot_u256(slot: B256) -> U256 {
    slot.into()
}

fn storage_word(value: B256) -> U256 {
    value.into()
}
