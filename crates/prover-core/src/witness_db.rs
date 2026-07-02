use alloc::collections::BTreeMap;
use core::fmt;

use alloy_primitives::{Address, B256, Bytes, keccak256};
use alloy_trie::{EMPTY_ROOT_HASH, KECCAK_EMPTY};
use revm_database_interface::{
    DBErrorMarker, Database,
    primitives::{StorageKey, StorageValue},
    state::{AccountInfo, Bytecode},
};

use crate::{ProverError, ZoneStateWitness, trie, validate_node_pool};

/// Strict revm database backed only by verified [`ZoneStateWitness`] reads.
///
/// Missing state is an execution error unless the witness supplied a valid
/// non-membership proof for that exact account or storage slot. This mirrors
/// the stateless validation shape used by `paradigmxyz/stateless`: the EVM gets
/// an ordinary revm database, but every value it can read is pre-bound to the
/// witnessed state root.
#[derive(Debug, Clone)]
pub struct WitnessDatabase {
    accounts: BTreeMap<Address, Option<AccountInfo>>,
    storage: BTreeMap<(Address, StorageKey), StorageValue>,
    bytecode: BTreeMap<B256, Bytecode>,
    block_hashes_by_number: BTreeMap<u64, B256>,
}

impl WitnessDatabase {
    pub fn new(
        state: &ZoneStateWitness,
        block_hashes_by_number: BTreeMap<u64, B256>,
    ) -> Result<Self, ProverError> {
        validate_node_pool(&state.node_pool, ProverError::ZoneStateNodeHashMismatch)?;

        let mut accounts = BTreeMap::new();
        let mut storage_roots = BTreeMap::new();
        let mut bytecode = BTreeMap::new();
        bytecode.insert(KECCAK_EMPTY, Bytecode::new_raw(Bytes::new()));

        for read in &state.account_reads {
            match &read.code {
                crate::ZoneAccountCode::Bytecode(code) => {
                    if keccak256(code.as_ref()) != read.code_hash {
                        return Err(ProverError::AccountCodeHashMismatch(read.account));
                    }
                    bytecode.insert(read.code_hash, Bytecode::new_raw(code.clone()));
                }
                crate::ZoneAccountCode::Empty if read.code_hash != KECCAK_EMPTY => {
                    return Err(ProverError::MissingAccountCode(read.account));
                }
                crate::ZoneAccountCode::Empty => {}
            }

            let proven_account =
                trie::verify_account_read(state.state_root, &state.node_pool, read)?;
            storage_roots.insert(read.account, proven_account.storage_root);
            accounts.insert(read.account, account_info(read));
        }

        let mut storage = BTreeMap::new();
        for read in &state.storage_reads {
            let storage_root = storage_roots
                .get(&read.account)
                .ok_or(ProverError::MissingAccountRead(read.account))?;
            trie::verify_storage_read(*storage_root, &state.node_pool, read)?;
            storage.insert((read.account, read.slot), read.value);
        }

        Ok(Self {
            accounts,
            storage,
            bytecode,
            block_hashes_by_number,
        })
    }
}

impl Database for WitnessDatabase {
    type Error = WitnessDbError;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.accounts
            .get(&address)
            .cloned()
            .ok_or(WitnessDbError::MissingAccount(address))
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
        self.storage
            .get(&(address, index))
            .copied()
            .ok_or(WitnessDbError::MissingStorage {
                account: address,
                slot: index,
            })
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
        }
    }
}

impl core::error::Error for WitnessDbError {}
impl DBErrorMarker for WitnessDbError {}

fn account_info(read: &crate::ZoneAccountRead) -> Option<AccountInfo> {
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
        code: match &read.code {
            crate::ZoneAccountCode::Empty => None,
            crate::ZoneAccountCode::Bytecode(code) => Some(Bytecode::new_raw(code.clone())),
        },
        account_id: None,
    })
}
