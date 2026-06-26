use reth_trie_common::{HashedPostState, KeccakKeyHasher};
use revm_database::BundleState;

/// Hashed post-execution state derived from revm's bundle state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionPostState {
    hashed: HashedPostState,
}

impl ExecutionPostState {
    /// Convert revm execution changes into reth's hashed post-state format.
    pub fn from_bundle_state(bundle_state: &BundleState) -> Self {
        Self {
            hashed: HashedPostState::from_bundle_state::<KeccakKeyHasher>(bundle_state.state()),
        }
    }

    pub fn hashed(&self) -> &HashedPostState {
        &self.hashed
    }

    pub fn into_hashed(self) -> HashedPostState {
        self.hashed
    }

    pub fn is_empty(&self) -> bool {
        self.hashed.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloy_primitives::{B256, U256, address};
    use reth_trie_common::{KeccakKeyHasher, KeyHasher};
    use revm_database::{
        primitives::StorageKeyMap,
        state::{AccountInfo, Bytecode},
    };

    #[test]
    fn hashes_revm_bundle_state_with_reth_keccak_keys() {
        let account = address!("0x0000000000000000000000000000000000001000");
        let slot = U256::from(7);
        let value = U256::from(9);
        let mut storage = StorageKeyMap::default();
        storage.insert(slot, (U256::ZERO, value));
        let code_hash = B256::repeat_byte(0xaa);
        let present = AccountInfo {
            nonce: 3,
            balance: U256::from(11),
            code_hash,
            code: Some(Bytecode::default()),
            ..Default::default()
        };
        let bundle = BundleState::builder(0..=0)
            .state_original_account_info(account, AccountInfo::default())
            .state_present_account_info(account, present)
            .state_storage(account, storage)
            .build();

        let post_state = ExecutionPostState::from_bundle_state(&bundle);
        let hashed = post_state.hashed();
        let hashed_address = KeccakKeyHasher::hash_key(account);
        let hashed_slot = KeccakKeyHasher::hash_key(B256::from(slot));

        let hashed_account = hashed
            .accounts
            .get(&hashed_address)
            .expect("account must be present")
            .expect("account must not be deleted");
        assert_eq!(hashed_account.nonce, 3);
        assert_eq!(hashed_account.balance, U256::from(11));
        assert_eq!(hashed_account.bytecode_hash, Some(code_hash));
        assert_eq!(
            hashed
                .storages
                .get(&hashed_address)
                .expect("storage must be present")
                .storage
                .get(&hashed_slot),
            Some(&value)
        );
    }
}
