use alloc::collections::BTreeMap;
#[cfg(feature = "std")]
use alloc::rc::Rc;
use core::fmt;

#[cfg(feature = "std")]
use alloy_primitives::map::B256Map;
use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
#[cfg(feature = "std")]
use alloy_rlp::Decodable;
use alloy_trie::{EMPTY_ROOT_HASH, KECCAK_EMPTY};
#[cfg(feature = "std")]
use reth_trie_common::DecodedMultiProofV2;
#[cfg(feature = "std")]
use reth_trie_sparse::{RevealableSparseTrie, SparseStateTrie};
use revm_database_interface::{
    DBErrorMarker, Database,
    primitives::{StorageKey, StorageValue},
    state::{AccountInfo, Bytecode},
};

use crate::{ProverError, ZoneExecutionWitness, ZoneStateWitness, trie};

/// Strict revm database backed only by verified [`ZoneExecutionWitness`] reads.
///
/// Missing state is an execution error unless the witness supplied a valid
/// non-membership proof for that exact account or storage slot. This mirrors
/// the stateless validation shape used by `paradigmxyz/stateless`: the EVM gets
/// an ordinary revm database, but every value it can read is pre-bound to the
/// witnessed state root.
#[derive(Debug, Clone)]
pub struct WitnessDatabase {
    #[cfg(feature = "std")]
    trie: Rc<SparseStateTrie>,
    #[cfg(not(feature = "std"))]
    accounts: BTreeMap<Address, Option<AccountInfo>>,
    #[cfg(not(feature = "std"))]
    storage_roots: BTreeMap<Address, B256>,
    #[cfg(not(feature = "std"))]
    storage: BTreeMap<(Address, StorageKey), StorageValue>,
    bytecode: BTreeMap<B256, Bytecode>,
    block_hashes_by_number: BTreeMap<u64, B256>,
}

impl WitnessDatabase {
    pub fn from_zone_state_witness(
        state: &ZoneStateWitness,
        block_hashes_by_number: BTreeMap<u64, B256>,
    ) -> Result<Self, ProverError> {
        let witness = ZoneExecutionWitness::from_zone_state_witness(state)?;
        Self::new(&witness, block_hashes_by_number)
    }

    pub fn new(
        witness: &ZoneExecutionWitness,
        block_hashes_by_number: BTreeMap<u64, B256>,
    ) -> Result<Self, ProverError> {
        let state_root = witness.pre_state_root();
        let node_pool = witness.state_nodes_by_hash();

        let mut storage_roots = BTreeMap::new();
        let mut bytecode = BTreeMap::new();
        #[cfg(not(feature = "std"))]
        let mut accounts = BTreeMap::new();
        #[cfg(not(feature = "std"))]
        let mut storage = BTreeMap::new();
        bytecode.insert(KECCAK_EMPTY, Bytecode::new_raw(Bytes::new()));

        for code in &witness.execution_witness().codes {
            bytecode.insert(keccak256(code.as_ref()), Bytecode::new_raw(code.clone()));
        }

        for read in witness.account_reads() {
            if read.code_hash != KECCAK_EMPTY && !bytecode.contains_key(&read.code_hash) {
                return Err(ProverError::MissingAccountCode(read.account));
            }
            let proven_account = trie::verify_account_read(state_root, &node_pool, read)?;
            storage_roots.insert(read.account, proven_account.storage_root);
            #[cfg(not(feature = "std"))]
            accounts.insert(read.account, account_info_from_read(read));
        }

        for read in witness.storage_reads() {
            let storage_root = storage_roots
                .get(&read.account)
                .ok_or(ProverError::MissingAccountRead(read.account))?;
            trie::verify_storage_read(*storage_root, &node_pool, read)?;
            #[cfg(not(feature = "std"))]
            storage.insert((read.account, read.slot), read.value);
        }

        #[cfg(feature = "std")]
        {
            let trie = sparse_trie_from_witness_nodes(state_root, &node_pool)?;

            Ok(Self {
                trie: Rc::new(trie),
                bytecode,
                block_hashes_by_number,
            })
        }

        #[cfg(not(feature = "std"))]
        {
            Ok(Self {
                accounts,
                storage_roots,
                storage,
                bytecode,
                block_hashes_by_number,
            })
        }
    }
}

impl Database for WitnessDatabase {
    type Error = WitnessDbError;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        #[cfg(feature = "std")]
        {
            let hashed_address = keccak256(address);
            if let Some(bytes) = self.trie.get_account_value(&hashed_address) {
                let account = alloy_trie::TrieAccount::decode(&mut bytes.as_slice())?;
                return Ok(Some(AccountInfo {
                    balance: account.balance,
                    nonce: account.nonce,
                    code_hash: account.code_hash,
                    code: None,
                    account_id: None,
                }));
            }

            if !self.trie.is_account_revealed(hashed_address) {
                return Err(WitnessDbError::MissingAccount(address));
            }

            Ok(None)
        }

        #[cfg(not(feature = "std"))]
        {
            self.accounts
                .get(&address)
                .cloned()
                .ok_or(WitnessDbError::MissingAccount(address))
        }
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        self.bytecode
            .get(&code_hash)
            .cloned()
            .ok_or(WitnessDbError::MissingCode(code_hash))
    }

    fn storage(
        &mut self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        #[cfg(feature = "std")]
        {
            let hashed_address = keccak256(address);
            let hashed_slot = keccak256(B256::from(index));
            if let Some(raw) = self
                .trie
                .get_storage_slot_value(&hashed_address, &hashed_slot)
            {
                return Ok(U256::decode(&mut raw.as_slice())?);
            }

            if let Some(bytes) = self.trie.get_account_value(&hashed_address) {
                let account = alloy_trie::TrieAccount::decode(&mut bytes.as_slice())?;
                if account.storage_root != EMPTY_ROOT_HASH
                    && !self
                        .trie
                        .check_valid_storage_witness(hashed_address, hashed_slot)
                {
                    return Err(WitnessDbError::MissingStorage {
                        account: address,
                        slot: index,
                    });
                }
            } else if !self.trie.is_account_revealed(hashed_address) {
                return Err(WitnessDbError::MissingStorage {
                    account: address,
                    slot: index,
                });
            }

            Ok(U256::ZERO)
        }

        #[cfg(not(feature = "std"))]
        {
            if let Some(value) = self.storage.get(&(address, index)).copied() {
                return Ok(value);
            }

            match self.accounts.get(&address) {
                Some(None) => Ok(U256::ZERO),
                Some(Some(_)) if self.storage_roots.get(&address) == Some(&EMPTY_ROOT_HASH) => {
                    Ok(U256::ZERO)
                }
                _ => Err(WitnessDbError::MissingStorage {
                    account: address,
                    slot: index,
                }),
            }
        }
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        self.block_hashes_by_number
            .get(&number)
            .copied()
            .ok_or(WitnessDbError::MissingBlockHash(number))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WitnessDbError {
    MissingAccount(Address),
    MissingStorage { account: Address, slot: StorageKey },
    MissingCode(B256),
    MissingBlockHash(u64),
    TrieWitness,
    Rlp(alloy_rlp::Error),
}

impl fmt::Display for WitnessDbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingAccount(account) => {
                write!(f, "witness is missing account read for {account}")
            }
            Self::MissingStorage { account, slot } => {
                write!(
                    f,
                    "witness is missing storage read for account {account} slot {slot}"
                )
            }
            Self::MissingCode(code_hash) => {
                write!(f, "witness is missing bytecode for code hash {code_hash}")
            }
            Self::MissingBlockHash(number) => {
                write!(f, "witness is missing ancestor block hash for {number}")
            }
            Self::TrieWitness => {
                write!(f, "witness trie nodes do not reveal the bound state root")
            }
            Self::Rlp(err) => {
                write!(f, "witness trie value failed to decode: {err}")
            }
        }
    }
}

impl core::error::Error for WitnessDbError {}
impl DBErrorMarker for WitnessDbError {}

impl From<alloy_rlp::Error> for WitnessDbError {
    fn from(err: alloy_rlp::Error) -> Self {
        Self::Rlp(err)
    }
}

#[cfg(not(feature = "std"))]
fn account_info_from_read(read: &crate::ZoneAccountRead) -> Option<AccountInfo> {
    if read.nonce == 0
        && read.balance.is_zero()
        && read.storage_root == EMPTY_ROOT_HASH
        && read.code_hash == KECCAK_EMPTY
    {
        return None;
    }

    Some(AccountInfo {
        balance: read.balance,
        nonce: read.nonce,
        code_hash: read.code_hash,
        code: None,
        account_id: None,
    })
}

#[cfg(feature = "std")]
fn sparse_trie_from_witness_nodes(
    state_root: B256,
    node_pool: &BTreeMap<B256, Bytes>,
) -> Result<SparseStateTrie, ProverError> {
    if state_root == EMPTY_ROOT_HASH && node_pool.is_empty() {
        return Ok(revealed_empty_sparse_trie());
    }

    let witness = node_pool
        .iter()
        .map(|(hash, node)| (*hash, node.clone()))
        .collect::<B256Map<_>>();
    let multiproof = DecodedMultiProofV2::from_witness(state_root, &witness)
        .map_err(|_| ProverError::ZoneStateNodeHashMismatch(state_root))?;

    let mut trie = SparseStateTrie::new();
    trie.reveal_decoded_multiproof_v2(multiproof)
        .map_err(|_| ProverError::ZoneStateNodeHashMismatch(state_root))?;
    let root = trie
        .root()
        .map_err(|_| ProverError::ZoneStateNodeHashMismatch(state_root))?;
    if root != state_root {
        return Err(ProverError::ZoneStateNodeHashMismatch(root));
    }

    Ok(trie)
}

#[cfg(feature = "std")]
fn revealed_empty_sparse_trie() -> SparseStateTrie {
    let mut trie = SparseStateTrie::default();
    trie.set_accounts_trie(RevealableSparseTrie::revealed_empty());
    trie
}
