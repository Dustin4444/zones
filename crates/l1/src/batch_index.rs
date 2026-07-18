//! Durable observations of receipt-root-verified `BatchSubmitted` events.

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use alloy_eips::NumHash;
use alloy_primitives::{B256, U256};
use parking_lot::Mutex;

use crate::abi::ZonePortal;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BatchSubmissionObservation {
    pub withdrawal_batch_index: u64,
    pub withdrawal_queue_index: u64,
    pub next_processed_deposit_queue_hash: B256,
    pub next_block_hash: B256,
    pub withdrawal_queue_hash: B256,
    pub last_processed_deposit_number: u64,
}

impl TryFrom<ZonePortal::BatchSubmitted> for BatchSubmissionObservation {
    type Error = eyre::Report;
    fn try_from(event: ZonePortal::BatchSubmitted) -> Result<Self, Self::Error> {
        Ok(Self {
            withdrawal_batch_index: event.withdrawalBatchIndex,
            withdrawal_queue_index: event
                .withdrawalQueueIndex
                .try_into()
                .map_err(|_| eyre::eyre!("withdrawal queue index overflow in BatchSubmitted"))?,
            next_processed_deposit_queue_hash: event.nextProcessedDepositQueueHash,
            next_block_hash: event.nextBlockHash,
            withdrawal_queue_hash: event.withdrawalQueueHash,
            last_processed_deposit_number: event.lastProcessedDepositNumber,
        })
    }
}

impl From<BatchSubmissionObservation> for ZonePortal::BatchSubmitted {
    fn from(event: BatchSubmissionObservation) -> Self {
        Self {
            withdrawalBatchIndex: event.withdrawal_batch_index,
            withdrawalQueueIndex: U256::from(event.withdrawal_queue_index),
            nextProcessedDepositQueueHash: event.next_processed_deposit_queue_hash,
            nextBlockHash: event.next_block_hash,
            withdrawalQueueHash: event.withdrawal_queue_hash,
            lastProcessedDepositNumber: event.last_processed_deposit_number,
        }
    }
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct PersistedIndex {
    blocks: BTreeMap<u64, PersistedBlock>,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct PersistedBlock {
    hash: B256,
    observations: Vec<BatchSubmissionObservation>,
}

#[derive(Clone, Debug)]
pub struct BatchSubmissionIndex {
    path: Option<Arc<PathBuf>>,
    inner: Arc<Mutex<PersistedIndex>>,
}

impl Default for BatchSubmissionIndex {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl BatchSubmissionIndex {
    pub fn in_memory() -> Self {
        Self {
            path: None,
            inner: Arc::default(),
        }
    }
    pub fn open(path: impl Into<PathBuf>) -> eyre::Result<Self> {
        let path = path.into();
        let inner = if path.exists() {
            serde_json::from_slice(&fs::read(&path)?)?
        } else {
            PersistedIndex::default()
        };
        Ok(Self {
            path: Some(Arc::new(path)),
            inner: Arc::new(Mutex::new(inner)),
        })
    }
    /// Replaces a block's observations and discards observations on a losing fork.
    pub fn record_block(
        &self,
        block: NumHash,
        observations: Vec<BatchSubmissionObservation>,
    ) -> eyre::Result<()> {
        let mut inner = self.inner.lock();
        if inner
            .blocks
            .get(&block.number)
            .is_some_and(|current| current.hash == block.hash)
        {
            return Ok(());
        }
        inner.blocks.split_off(&block.number);
        inner.blocks.insert(
            block.number,
            PersistedBlock {
                hash: block.hash,
                observations,
            },
        );
        self.persist(&inner)
    }
    pub fn events(&self, first_index: u64, tail: u64) -> BTreeMap<u64, ZonePortal::BatchSubmitted> {
        self.inner
            .lock()
            .blocks
            .values()
            .flat_map(|block| block.observations.iter().cloned())
            .filter(|event| (first_index..tail).contains(&event.withdrawal_queue_index))
            .map(|event| (event.withdrawal_queue_index, event.into()))
            .collect()
    }
    fn persist(&self, index: &PersistedIndex) -> eyre::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        write_atomically(path, &serde_json::to_vec(index)?)
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> eyre::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| eyre::eyre!("batch index path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension("tmp");
    let mut file = fs::File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(index: u64) -> BatchSubmissionObservation {
        BatchSubmissionObservation {
            withdrawal_batch_index: index,
            withdrawal_queue_index: index,
            next_processed_deposit_queue_hash: B256::repeat_byte(index as u8),
            next_block_hash: B256::repeat_byte(index as u8),
            withdrawal_queue_hash: B256::repeat_byte(index as u8),
            last_processed_deposit_number: index,
        }
    }

    #[test]
    fn replaces_observations_on_a_reorged_height() {
        let index = BatchSubmissionIndex::in_memory();
        index
            .record_block(
                NumHash::new(10, B256::repeat_byte(10)),
                vec![observation(1)],
            )
            .unwrap();
        index
            .record_block(
                NumHash::new(11, B256::repeat_byte(11)),
                vec![observation(2)],
            )
            .unwrap();
        index
            .record_block(
                NumHash::new(11, B256::repeat_byte(12)),
                vec![observation(3)],
            )
            .unwrap();

        let events = index.events(1, 4);
        assert!(events.contains_key(&1));
        assert!(!events.contains_key(&2));
        assert!(events.contains_key(&3));
    }
}
