//! Staged acquire, check, durable commit, and mirror adoption.

use std::time::Instant;

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
        pipeline::{ObservedZoneCandidate, PreparedBlock},
    },
    metrics::{BlockMetricSample, BlockProcessingPhase},
    observe::{AcquisitionError, AcquisitionSource, ExactStateLookup},
    store::{
        db::CanonicalBlock,
        error::{ParentTips, StoreError},
        history::{AppliedBlockMetrics, BlockWriteResult},
    },
};

use super::{
    RuntimeResult,
    chain::ValidatedChain,
    state::{PersistentChecker, ReadyToAcknowledge},
};

/// Proof that the candidate's guarded MDBX transaction committed.
pub(crate) struct DurableBlock {
    prepared: PreparedBlock,
    write_metrics: Option<AppliedBlockMetrics>,
}

/// One concrete, lazily connected Tempo provider for the retained notification.
pub(crate) struct L1Client {
    rpc_url: String,
    chain: ChainBinding,
    provider: Option<DynProvider<TempoNetwork>>,
}

#[derive(Clone, Copy)]
enum ChainBinding {
    Unbound,
    Expected(u64),
    Validated(u64),
}

impl L1Client {
    pub(crate) fn new(rpc_url: String) -> Self {
        Self {
            rpc_url,
            chain: ChainBinding::Unbound,
            provider: None,
        }
    }

    /// Create a lazy client bound to the chain identity already stored in the
    /// checker database. The first network use validates that identity before
    /// exposing the provider to model acquisition.
    pub(super) fn for_chain(rpc_url: String, expected_chain_id: u64) -> Self {
        Self {
            rpc_url,
            chain: ChainBinding::Expected(expected_chain_id),
            provider: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_provider(provider: DynProvider<TempoNetwork>) -> Self {
        Self {
            rpc_url: String::new(),
            chain: ChainBinding::Unbound,
            provider: Some(provider),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_provider_for_chain(
        provider: DynProvider<TempoNetwork>,
        expected_chain_id: u64,
    ) -> Self {
        Self {
            rpc_url: String::new(),
            chain: ChainBinding::Expected(expected_chain_id),
            provider: Some(provider),
        }
    }

    /// Record the chain-ID read that authenticated a fresh database. The
    /// connected provider can then be reused without a duplicate RPC call.
    pub(super) fn bind_validated_chain_id(&mut self, chain_id: u64) {
        self.chain = ChainBinding::Validated(chain_id);
    }

    pub(super) async fn provider(&mut self) -> RuntimeResult<DynProvider<TempoNetwork>> {
        if !matches!(self.chain, ChainBinding::Expected(_))
            && let Some(provider) = &self.provider
        {
            return Ok(provider.clone());
        }

        let provider = match &self.provider {
            Some(provider) => provider.clone(),
            None => ProviderBuilder::new_with_network::<TempoNetwork>()
                .connect(&self.rpc_url)
                .await
                .map_err(|error| AcquisitionError::unavailable(AcquisitionSource::L1Rpc, error))?
                .erased(),
        };
        if let ChainBinding::Expected(expected) | ChainBinding::Validated(expected) = self.chain {
            let actual = provider
                .get_chain_id()
                .await
                .map_err(|error| AcquisitionError::unavailable(AcquisitionSource::L1Rpc, error))?;
            if actual != expected {
                return Err(AcquisitionError::inconsistent(
                    AcquisitionSource::L1Rpc,
                    format_args!("chain ID {expected}"),
                    format_args!("chain ID {actual}"),
                )
                .into());
            }
            self.chain = ChainBinding::Validated(expected);
        }
        self.provider = Some(provider.clone());
        Ok(provider)
    }
}

impl PersistentChecker {
    fn preflight_block(
        &mut self,
        block: &RecoveredBlock<Block>,
    ) -> RuntimeResult<Option<ReadyToAcknowledge>> {
        let child = BlockNumHash::new(block.header().number(), block.hash());
        match self
            .store
            .preflight_block(child, block.header().parent_hash())?
        {
            CanonicalBlock::AlreadyCanonical { verified_zone_tip } => {
                Ok(Some(ReadyToAcknowledge::verified(verified_zone_tip)))
            }
            CanonicalBlock::Next {
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
        self.replace_mirror_from_snapshot(snapshot);
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
        let write_metrics = match self.store.apply_block_measured(commit)? {
            BlockWriteResult::Applied(metrics) => Some(metrics),
            BlockWriteResult::AlreadyApplied => None,
        };
        Ok(DurableBlock {
            prepared,
            write_metrics,
        })
    }

    pub(crate) fn adopt_block(&mut self, durable: DurableBlock) -> ReadyToAcknowledge {
        self.mirror.apply_prepared(durable.prepared);
        ReadyToAcknowledge::verified(self.mirror.zone_tip())
    }

    pub(super) async fn process_committed_chain_once<S>(
        &mut self,
        chain: &ValidatedChain<'_>,
        zone_state: &S,
        l1_client: &mut L1Client,
        processing_phase: BlockProcessingPhase,
    ) -> RuntimeResult<ReadyToAcknowledge>
    where
        S: ExactStateLookup + ?Sized,
    {
        // `ValidatedChain` proves the iterator is nonempty. Starting from its
        // exact parent keeps that invariant out of the runtime error surface.
        let mut ready = ReadyToAcknowledge::verified(chain.base());
        for (block, receipts) in chain.inner().blocks_and_receipts() {
            let block_ready = self
                .process_canonical_block_once(
                    block,
                    receipts,
                    zone_state,
                    l1_client,
                    processing_phase,
                )
                .await?;
            if block_ready.is_alerting() {
                return Ok(ReadyToAcknowledge::alerted(chain.tip()));
            }
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

    /// Apply one exact canonical block through the same path used by live
    /// notifications and archive Zone replay.
    pub(super) async fn process_canonical_block_once<S>(
        &mut self,
        block: &RecoveredBlock<Block>,
        receipts: &[TempoReceipt],
        zone_state: &S,
        l1_client: &mut L1Client,
        processing_phase: BlockProcessingPhase,
    ) -> RuntimeResult<ReadyToAcknowledge>
    where
        S: ExactStateLookup + ?Sized,
    {
        let block_started = Instant::now();
        if let Some(ready) = self.preflight_block(block)? {
            return Ok(ready);
        }
        let observed = match self.observe_zone_candidate(block, receipts) {
            Ok(observed) => observed,
            Err(CheckError::Finding {
                finding,
                imported_tempo,
            }) => {
                self.activate_alert(block, imported_tempo, finding.as_ref())?;
                return Ok(ReadyToAcknowledge::alerted(BlockNumHash::new(
                    block.header().number(),
                    block.hash(),
                )));
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
                return Ok(ReadyToAcknowledge::alerted(BlockNumHash::new(
                    block.header().number(),
                    block.hash(),
                )));
            }
            Err(error) => return Err(error.into()),
        };
        let durable = self.commit_block(candidate)?;
        let write_metrics = durable.write_metrics;
        let ready = self.adopt_block(durable);
        if let Some(write_metrics) = write_metrics {
            self.metrics.record_block(BlockMetricSample {
                phase: processing_phase,
                block_duration: block_started.elapsed(),
                transaction_duration: write_metrics.transaction_duration,
                changeset_bytes: write_metrics.changeset_bytes,
                model_rows: write_metrics.model_rows,
                open_lifecycle_records: self.mirror.open_lifecycle_record_count(),
                database_path: self.store.path(),
            });
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
        Ok(DurableBlock {
            prepared,
            write_metrics: None,
        })
    }
}
