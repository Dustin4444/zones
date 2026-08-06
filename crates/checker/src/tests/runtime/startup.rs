use reth_storage_api::{BlockHashReader, errors::provider::ProviderResult};

use super::*;
use crate::runtime::validate_local_canonical_tip;

#[test]
fn startup_requires_the_exact_local_canonical_hash() {
    let tip = BlockNumHash::new(7, B256::repeat_byte(0x70));
    assert!(validate_local_canonical_tip(&CanonicalHash(Some(tip.hash)), tip).is_ok());
    assert!(matches!(
        validate_local_canonical_tip(&CanonicalHash(None), tip),
        Err(RuntimeError::MissingLocalCanonical(actual)) if actual == tip
    ));
    let wrong = B256::repeat_byte(0x71);
    assert!(matches!(
        validate_local_canonical_tip(&CanonicalHash(Some(wrong)), tip),
        Err(RuntimeError::LocalCanonicalConflict { tip: actual_tip, actual })
            if actual_tip == tip && actual == wrong
    ));
}

#[test]
fn persistent_runtime_refuses_incomplete_bootstrap() {
    let directory = TempDir::new().unwrap();
    let mut initialization = live_initialization();
    initialization.bootstrap = BootstrapState::zone_replay(initialization.imported_tempo_tip);
    let store = CheckerStore::open(directory.path(), initialization).unwrap();

    assert!(matches!(
        LiveChecker::from_store(store),
        Err(RuntimeError::Store(StoreError::InvalidBootstrapProgress(
            "persistent live runtime requires completed bootstrap"
        )))
    ));
}

struct CanonicalHash(Option<B256>);

impl BlockHashReader for CanonicalHash {
    fn block_hash(&self, _number: u64) -> ProviderResult<Option<B256>> {
        Ok(self.0)
    }

    fn canonical_hashes_range(&self, _start: u64, _end: u64) -> ProviderResult<Vec<B256>> {
        unreachable!("startup validation performs one exact point read")
    }
}
