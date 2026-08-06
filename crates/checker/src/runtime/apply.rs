//! Staged acquire, check, durable commit, and mirror adoption.

use alloy_consensus::BlockHeader as _;
use alloy_eips::BlockNumHash;
use alloy_provider::{DynProvider, Provider as _, ProviderBuilder};
use reth_execution_types::Chain;
use reth_exex::ExExNotification;
use reth_primitives_traits::RecoveredBlock;
use tempo_alloy::TempoNetwork;
use tempo_primitives::{Block, TempoPrimitives, TempoReceipt};
use tracing::info;

#[cfg(test)]
use crate::store::db::StoreSnapshot;
use crate::{
    check::pipeline::{InMemoryChecker, PreparedBlock},
    observe::{AcquisitionError, AcquisitionSource, ExactStateLookup},
    store::{
        db::{CheckerStore, LiveBlock},
        error::{ParentTips, StoreError},
        operations::WriteOutcome,
        value::BootstrapState,
    },
    validate_notification_receipt_sets,
};

use super::{RuntimeError, RuntimeResult};

/// A durable checker height that may now advance Reth's pruning watermark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReadyToAcknowledge(BlockNumHash);

impl ReadyToAcknowledge {
    pub(crate) const fn tip(self) -> BlockNumHash {
        self.0
    }
}

/// Proof that the candidate's guarded MDBX transaction committed.
pub(crate) struct DurableBlock(PreparedBlock);

/// One concrete, lazily connected Tempo provider for the retained notification.
pub(crate) struct L1Client {
    rpc_url: String,
    provider: Option<DynProvider<TempoNetwork>>,
}

impl L1Client {
    pub(crate) fn new(rpc_url: String) -> Self {
        Self {
            rpc_url,
            provider: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_provider(provider: DynProvider<TempoNetwork>) -> Self {
        Self {
            rpc_url: String::new(),
            provider: Some(provider),
        }
    }

    async fn provider(&mut self) -> RuntimeResult<&DynProvider<TempoNetwork>> {
        if self.provider.is_none() {
            let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
                .connect(&self.rpc_url)
                .await
                .map_err(|error| AcquisitionError::unavailable(AcquisitionSource::L1Rpc, error))?
                .erased();
            self.provider = Some(provider);
        }
        Ok(self
            .provider
            .as_ref()
            .expect("provider initialized immediately above"))
    }
}

/// Sole owner of one checker store and its post-commit in-memory mirror.
pub(crate) struct LiveChecker {
    store: CheckerStore,
    mirror: InMemoryChecker,
}

impl LiveChecker {
    pub(crate) fn from_store(store: CheckerStore) -> RuntimeResult<Self> {
        let snapshot = store.load_current()?;
        if snapshot.bootstrap != BootstrapState::Live {
            return Err(StoreError::InvalidBootstrapProgress(
                "persistent live runtime requires completed bootstrap",
            )
            .into());
        }
        if let Some(alert) = snapshot.active_alert {
            return Err(StoreError::ActiveAlert(alert.finding).into());
        }

        let mirror = InMemoryChecker::new(
            snapshot.model,
            store.portal_creation_block_hash(),
            snapshot.verified_zone_tip,
            snapshot.imported_tempo_tip,
        );
        Ok(Self { store, mirror })
    }

    pub(crate) const fn mirror_tip(&self) -> BlockNumHash {
        self.mirror.zone_tip()
    }

    fn preflight_block(
        &mut self,
        block: &RecoveredBlock<Block>,
    ) -> RuntimeResult<Option<ReadyToAcknowledge>> {
        let child = BlockNumHash::new(block.header().number(), block.hash());
        match self
            .store
            .preflight_live_block(child, block.header().parent_hash())?
        {
            LiveBlock::AlreadyCanonical { verified_zone_tip } => {
                Ok(Some(ReadyToAcknowledge(verified_zone_tip)))
            }
            LiveBlock::Next {
                verified_zone_tip,
                imported_tempo_tip,
            } => {
                self.refresh_mirror_if_stale(verified_zone_tip, imported_tempo_tip)?;
                Ok(None)
            }
        }
    }

    /// Tips are the store generation guard under the sole-writer contract.
    /// A mismatch is possible only after a commit whose mirror adoption did
    /// not run; reload that durable cut without imposing an unbounded model
    /// scan on the ordinary steady-state path.
    fn refresh_mirror_if_stale(
        &mut self,
        verified_zone_tip: BlockNumHash,
        imported_tempo_tip: BlockNumHash,
    ) -> RuntimeResult<()> {
        if self.mirror.zone_tip() == verified_zone_tip
            && self.mirror.tempo_tip() == imported_tempo_tip
        {
            return Ok(());
        }

        let snapshot = self.store.load_current()?;
        if snapshot.verified_zone_tip != verified_zone_tip
            || snapshot.imported_tempo_tip != imported_tempo_tip
        {
            return Err(StoreError::ParentChanged {
                expected: Box::new(ParentTips::new(verified_zone_tip, imported_tempo_tip)),
                actual: Box::new(ParentTips::new(
                    snapshot.verified_zone_tip,
                    snapshot.imported_tempo_tip,
                )),
            }
            .into());
        }
        self.mirror = InMemoryChecker::new(
            snapshot.model,
            self.store.portal_creation_block_hash(),
            snapshot.verified_zone_tip,
            snapshot.imported_tempo_tip,
        );
        Ok(())
    }

    pub(crate) async fn prepare_block<P, S>(
        &self,
        l1_provider: &P,
        zone_state: &S,
        block: &RecoveredBlock<Block>,
        receipts: &[TempoReceipt],
    ) -> RuntimeResult<PreparedBlock>
    where
        P: alloy_provider::Provider<TempoNetwork>,
        S: ExactStateLookup + ?Sized,
    {
        let prepared = self
            .mirror
            .prepare_observed_block(l1_provider, zone_state, block, receipts)
            .await?;
        Ok(prepared)
    }

    pub(crate) fn commit_block(&self, prepared: PreparedBlock) -> RuntimeResult<DurableBlock> {
        let commit = self.store.block_commit(
            prepared.parent_zone_tip(),
            prepared.parent_tempo_tip(),
            prepared.child_zone_tip(),
            prepared.child_tempo_tip(),
            prepared.state_update(),
        )?;
        match self.store.apply_block(commit)? {
            WriteOutcome::Applied | WriteOutcome::AlreadyApplied => Ok(DurableBlock(prepared)),
        }
    }

    pub(crate) fn adopt_block(&mut self, durable: DurableBlock) -> ReadyToAcknowledge {
        self.mirror.apply_prepared(durable.0);
        ReadyToAcknowledge(self.mirror.zone_tip())
    }

    async fn process_committed_chain_once<S>(
        &mut self,
        chain: &Chain<TempoPrimitives>,
        zone_state: &S,
        l1_client: &mut L1Client,
    ) -> RuntimeResult<ReadyToAcknowledge>
    where
        S: ExactStateLookup + ?Sized,
    {
        if chain.blocks().is_empty() {
            return Err(RuntimeError::EmptyCommittedChain);
        }
        validate_notification_receipt_sets(
            chain.blocks().len(),
            chain.block_receipts_iter().count(),
        )?;

        let mut ready = None;
        for (block, receipts) in chain.blocks_and_receipts() {
            let block_ready = match self.preflight_block(block)? {
                Some(ready) => ready,
                None => {
                    let provider = l1_client.provider().await?;
                    let candidate = self
                        .prepare_block(provider, zone_state, block, receipts)
                        .await?;
                    let durable = self.commit_block(candidate)?;
                    self.adopt_block(durable)
                }
            };
            info!(
                target: "zone::checker",
                number = block.header().number(),
                hash = %block.hash(),
                durable_tip = ?block_ready.tip(),
                "Canonical checker block is durable"
            );
            ready = Some(block_ready);
        }

        ready.ok_or(RuntimeError::EmptyCommittedChain)
    }

    pub(crate) async fn process_notification_once<S>(
        &mut self,
        notification: &ExExNotification<TempoPrimitives>,
        zone_state: &S,
        l1_client: &mut L1Client,
    ) -> RuntimeResult<ReadyToAcknowledge>
    where
        S: ExactStateLookup + ?Sized,
    {
        match notification {
            ExExNotification::ChainCommitted { new } => {
                self.process_committed_chain_once(new, zone_state, l1_client)
                    .await
            }
            ExExNotification::ChainReverted { .. } => {
                Err(RuntimeError::UnsupportedNotification("revert"))
            }
            ExExNotification::ChainReorged { .. } => {
                Err(RuntimeError::UnsupportedNotification("reorg"))
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn current_snapshot_for_test(&self) -> StoreSnapshot {
        self.store.load_current().unwrap()
    }

    #[cfg(test)]
    pub(crate) fn commit_block_aborting_after(
        &self,
        prepared: PreparedBlock,
        writes: usize,
    ) -> RuntimeResult<DurableBlock> {
        let commit = self.store.block_commit(
            prepared.parent_zone_tip(),
            prepared.parent_tempo_tip(),
            prepared.child_zone_tip(),
            prepared.child_tempo_tip(),
            prepared.state_update(),
        )?;
        self.store.apply_block_aborting_after(commit, writes)?;
        Ok(DurableBlock(prepared))
    }
}
