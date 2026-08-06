//! Staged acquire, check, durable commit, and mirror adoption.

use alloy_consensus::BlockHeader as _;
use alloy_eips::BlockNumHash;
use alloy_provider::{DynProvider, Provider as _, ProviderBuilder};
use reth_primitives_traits::RecoveredBlock;
use tempo_alloy::TempoNetwork;
use tempo_primitives::{Block, TempoReceipt};
use tracing::info;

use crate::{
    check::{
        finding::CheckError,
        pipeline::{InMemoryChecker, ObservedZoneCandidate, PreparedBlock},
    },
    observe::{AcquisitionError, AcquisitionSource, ExactStateLookup},
    store::{
        db::LiveBlock,
        error::{ParentTips, StoreError},
        operations::WriteOutcome,
    },
};

use super::{
    RuntimeResult,
    chain::ValidatedChain,
    state::{LiveChecker, ReadyToAcknowledge},
};

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

    async fn provider(&mut self) -> RuntimeResult<DynProvider<TempoNetwork>> {
        if let Some(provider) = &self.provider {
            return Ok(provider.clone());
        }
        let provider = ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect(&self.rpc_url)
            .await
            .map_err(|error| AcquisitionError::unavailable(AcquisitionSource::L1Rpc, error))?
            .erased();
        self.provider = Some(provider.clone());
        Ok(provider)
    }
}

impl LiveChecker {
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
                Ok(Some(ReadyToAcknowledge::verified(verified_zone_tip)))
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

    #[cfg(test)]
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

    fn observe_zone_candidate(
        &self,
        block: &RecoveredBlock<Block>,
        receipts: &[TempoReceipt],
    ) -> Result<ObservedZoneCandidate, CheckError> {
        self.mirror.observe_zone_candidate(block, receipts)
    }

    async fn prepare_observed_candidate<P, S>(
        &self,
        l1_provider: &P,
        zone_state: &S,
        candidate: ObservedZoneCandidate,
    ) -> Result<PreparedBlock, CheckError>
    where
        P: alloy_provider::Provider<TempoNetwork>,
        S: ExactStateLookup + ?Sized,
    {
        self.mirror
            .prepare_observed_candidate(l1_provider, zone_state, candidate)
            .await
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
        ReadyToAcknowledge::verified(self.mirror.zone_tip())
    }

    pub(super) async fn process_committed_chain_once<S>(
        &mut self,
        chain: &ValidatedChain<'_>,
        zone_state: &S,
        l1_client: &mut L1Client,
    ) -> RuntimeResult<ReadyToAcknowledge>
    where
        S: ExactStateLookup + ?Sized,
    {
        // `ValidatedChain` proves the iterator is nonempty. Starting from its
        // exact parent keeps that invariant out of the runtime error surface.
        let mut ready = ReadyToAcknowledge::verified(chain.base());
        for (block, receipts) in chain.inner().blocks_and_receipts() {
            let block_ready = match self.preflight_block(block)? {
                Some(ready) => ready,
                None => {
                    let observed = match self.observe_zone_candidate(block, receipts) {
                        Ok(observed) => observed,
                        Err(CheckError::Finding {
                            finding,
                            imported_tempo,
                        }) => {
                            self.activate_alert(block, imported_tempo, finding.as_ref())?;
                            return Ok(ReadyToAcknowledge::alerted(chain.tip()));
                        }
                        Err(error) => return Err(error.into()),
                    };
                    let provider = l1_client.provider().await?;
                    let candidate = match self
                        .prepare_observed_candidate(&provider, zone_state, observed)
                        .await
                    {
                        Ok(candidate) => candidate,
                        Err(CheckError::Finding {
                            finding,
                            imported_tempo,
                        }) => {
                            self.activate_alert(block, imported_tempo, finding.as_ref())?;
                            return Ok(ReadyToAcknowledge::alerted(chain.tip()));
                        }
                        Err(error) => return Err(error.into()),
                    };
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
            ready = block_ready;
        }

        Ok(ready)
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
