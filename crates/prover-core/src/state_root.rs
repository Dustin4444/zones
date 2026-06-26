use alloc::vec::Vec;

use alloy_primitives::B256;
use alloy_rlp::Decodable;
use alloy_trie::{EMPTY_ROOT_HASH, TrieAccount};
use reth_trie_common::{HashedPostState, Nibbles};
use reth_trie_sparse::{RevealableSparseTrie, SparseStateTrie};

use crate::ProverError;

/// State root computed by applying a reth hashed post-state to a verified sparse trie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalculatedStateRoot(B256);

impl CalculatedStateRoot {
    pub const fn get(self) -> B256 {
        self.0
    }

    #[cfg(test)]
    pub(crate) const fn trusted_for_test(root: B256) -> Self {
        Self(root)
    }
}

/// Reth sparse-trie state root calculator for stateless Zone execution.
#[derive(Debug, Default)]
pub struct SparseStateRootCalculator {
    trie: SparseStateTrie,
}

impl SparseStateRootCalculator {
    pub const fn new(trie: SparseStateTrie) -> Self {
        Self { trie }
    }

    pub fn revealed_empty() -> Self {
        let mut trie = SparseStateTrie::default();
        trie.set_accounts_trie(RevealableSparseTrie::revealed_empty());
        Self { trie }
    }

    pub fn calculate(
        &mut self,
        post_state: HashedPostState,
    ) -> Result<CalculatedStateRoot, ProverError> {
        calculate_state_root(&mut self.trie, post_state)
    }

    pub fn trie_mut(&mut self) -> &mut SparseStateTrie {
        &mut self.trie
    }
}

pub fn calculate_state_root(
    trie: &mut SparseStateTrie,
    state: HashedPostState,
) -> Result<CalculatedStateRoot, ProverError> {
    let HashedPostState { accounts, storages } = state;

    let mut storages = storages.into_iter().collect::<Vec<_>>();
    storages.sort_unstable_by_key(|(address, _)| *address);

    for (address, storage) in storages {
        let mut storage_trie = take_storage_trie_for_update(trie, address, storage.wiped)?;

        if storage.wiped {
            storage_trie
                .wipe()
                .map_err(|_| ProverError::StateRootCalculationFailed)?;
        }

        let mut slots = storage.storage.into_iter().collect::<Vec<_>>();
        slots.sort_unstable_by_key(|(slot, _)| *slot);

        for (slot, value) in slots {
            let path = Nibbles::unpack(slot);
            if value.is_zero() {
                storage_trie
                    .remove_leaf(&path)
                    .map_err(|_| ProverError::StateRootCalculationFailed)?;
            } else {
                storage_trie
                    .update_leaf(path, alloy_rlp::encode_fixed_size(&value).to_vec())
                    .map_err(|_| ProverError::StateRootCalculationFailed)?;
            }
        }

        storage_trie
            .root()
            .ok_or(ProverError::StateRootCalculationFailed)?;
        trie.insert_storage_trie(address, storage_trie);
    }

    let mut accounts = accounts.into_iter().collect::<Vec<_>>();
    accounts.sort_unstable_by_key(|(address, _)| *address);

    for (address, account) in accounts {
        trie.update_account_stateless(address, account)
            .map_err(|_| ProverError::StateRootCalculationFailed)?;
    }

    trie.root()
        .map(CalculatedStateRoot)
        .map_err(|_| ProverError::StateRootCalculationFailed)
}

fn take_storage_trie_for_update(
    trie: &mut SparseStateTrie,
    address: B256,
    wiped: bool,
) -> Result<RevealableSparseTrie, ProverError> {
    if let Some(storage_trie) = trie.take_storage_trie(&address) {
        return Ok(storage_trie);
    }

    if !wiped
        && matches!(
            revealed_account_storage_root(trie, address)?,
            Some(storage_root) if storage_root != EMPTY_ROOT_HASH
        )
    {
        return Err(ProverError::StateRootCalculationFailed);
    }

    Ok(RevealableSparseTrie::revealed_empty())
}

fn revealed_account_storage_root(
    trie: &SparseStateTrie,
    address: B256,
) -> Result<Option<B256>, ProverError> {
    let Some(value) = trie.get_account_value(&address) else {
        return Ok(None);
    };

    TrieAccount::decode(&mut &value[..])
        .map(|account| Some(account.storage_root))
        .map_err(|_| ProverError::StateRootCalculationFailed)
}

pub fn empty_state_root() -> CalculatedStateRoot {
    CalculatedStateRoot(EMPTY_ROOT_HASH)
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloy_primitives::{B256, U256};
    use alloy_rlp::Encodable;
    use alloy_trie::root::{state_root, storage_root};
    use reth_primitives_traits::Account;
    use reth_trie_common::{HashedStorage, KeccakKeyHasher, KeyHasher};

    #[test]
    fn empty_post_state_keeps_empty_state_root() {
        let mut calculator = SparseStateRootCalculator::revealed_empty();
        let root = calculator
            .calculate(HashedPostState::default())
            .expect("empty root calculation should succeed");

        assert_eq!(root.get(), EMPTY_ROOT_HASH);
    }

    #[test]
    fn account_update_matches_alloy_state_root() {
        let hashed_address = B256::repeat_byte(0x11);
        let account = Account {
            nonce: 7,
            balance: U256::from(9),
            bytecode_hash: None,
        };
        let post_state =
            HashedPostState::default().with_accounts([(hashed_address, Some(account))]);

        let mut calculator = SparseStateRootCalculator::revealed_empty();
        let actual = calculator
            .calculate(post_state)
            .expect("account root calculation should succeed");

        let expected = state_root([(hashed_address, account.into_trie_account(EMPTY_ROOT_HASH))]);
        assert_eq!(actual.get(), expected);
    }

    #[test]
    fn storage_update_matches_alloy_state_root() {
        let address = B256::repeat_byte(0x22);
        let slot = B256::repeat_byte(0x33);
        let value = U256::from(44);
        let account = Account {
            nonce: 1,
            balance: U256::from(2),
            bytecode_hash: None,
        };
        let storage = HashedStorage::from_iter(false, [(slot, value)]);
        let post_state = HashedPostState::default()
            .with_accounts([(address, Some(account))])
            .with_storages([(address, storage)]);

        let mut calculator = SparseStateRootCalculator::revealed_empty();
        let actual = calculator
            .calculate(post_state)
            .expect("storage root calculation should succeed");

        let storage_root = storage_root([(slot, value)]);
        let expected = state_root([(address, account.into_trie_account(storage_root))]);
        assert_eq!(actual.get(), expected);
    }

    #[test]
    fn non_wiped_storage_update_requires_revealed_storage_trie_for_non_empty_account() {
        let address = B256::repeat_byte(0x44);
        let account = Account {
            nonce: 1,
            balance: U256::from(2),
            bytecode_hash: None,
        }
        .into_trie_account(B256::repeat_byte(0xbb));

        let mut encoded_account = Vec::new();
        account.encode(&mut encoded_account);

        let mut calculator = SparseStateRootCalculator::revealed_empty();
        calculator
            .trie_mut()
            .update_account_leaf(Nibbles::unpack(address), encoded_account)
            .expect("account insertion into revealed trie should succeed");

        let post_state = HashedPostState::default().with_storages([(
            address,
            HashedStorage::from_iter(false, [(address, U256::ONE)]),
        )]);

        assert_eq!(
            calculator.calculate(post_state).unwrap_err(),
            ProverError::StateRootCalculationFailed
        );
    }

    #[test]
    fn hashes_plain_keys_before_root_calculation() {
        let account = alloy_primitives::address!("0x0000000000000000000000000000000000001234");
        let slot = U256::from(5);
        let value = U256::from(6);
        let hashed_address = KeccakKeyHasher::hash_key(account);
        let hashed_slot = KeccakKeyHasher::hash_key(B256::from(slot));
        let hashed_storage = HashedStorage::from_iter(false, [(hashed_slot, value)]);
        let account = Account {
            nonce: 0,
            balance: U256::ZERO,
            bytecode_hash: None,
        };
        let post_state = HashedPostState::default()
            .with_accounts([(hashed_address, Some(account))])
            .with_storages([(hashed_address, hashed_storage)]);

        let mut calculator = SparseStateRootCalculator::revealed_empty();
        let actual = calculator
            .calculate(post_state)
            .expect("root calculation should succeed");

        let storage_root = storage_root([(hashed_slot, value)]);
        let expected = state_root([(hashed_address, account.into_trie_account(storage_root))]);
        assert_eq!(actual.get(), expected);
    }
}
