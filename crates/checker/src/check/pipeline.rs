//! Staged one-block preparation and the checker's in-memory model mirror.

use std::time::{Duration, Instant};

use alloy_consensus::BlockHeader as _;
use alloy_eips::BlockNumHash;
use alloy_primitives::B256;
use alloy_provider::Provider;
use reth_primitives_traits::RecoveredBlock;
use tempo_alloy::TempoNetwork;
use tempo_primitives::{Block, TempoReceipt};

use crate::{
    check::{
        finding::{CheckError, Finding},
        reconcile::{
            reconcile_collateral, reconcile_imported_outputs, reconcile_post_zone_state,
            reconcile_zone_outputs,
        },
    },
    metrics::CheckerMetrics,
    model::{
        adapter::{project_imported, project_zone},
        state::ModelState,
        transition::{ModelError, ModelStateUpdate},
    },
    observe::{
        ExactStateLookup, L1BlockObservation, L2BlockObservation, ObservationError,
        acquire_portal_collateral, acquire_zone_post_state, observe_l1, observe_l2_block,
    },
};

/// One loaded checker cut used to prepare a block without mutating its parent.
pub(crate) struct InMemoryChecker {
    model: ModelState,
    portal_creation_block_hash: B256,
    zone_tip: BlockNumHash,
    tempo_tip: BlockNumHash,
    metrics: CheckerMetrics,
}

#[derive(Default)]
struct CheckTimings {
    collateral: Duration,
    exact_state: Duration,
}

impl CheckTimings {
    fn acquisition(&self) -> Duration {
        self.collateral + self.exact_state
    }
}

/// Fully checked, provider-free logical delta for one exact parent cut.
pub(crate) struct PreparedBlock {
    state_update: ModelStateUpdate,
    parent_zone_tip: BlockNumHash,
    parent_tempo_tip: BlockNumHash,
    child_zone_tip: BlockNumHash,
    child_tempo_tip: BlockNumHash,
}

impl PreparedBlock {
    pub(crate) const fn state_update(&self) -> &ModelStateUpdate {
        &self.state_update
    }

    pub(crate) const fn parent_zone_tip(&self) -> BlockNumHash {
        self.parent_zone_tip
    }

    pub(crate) const fn parent_tempo_tip(&self) -> BlockNumHash {
        self.parent_tempo_tip
    }

    pub(crate) const fn child_zone_tip(&self) -> BlockNumHash {
        self.child_zone_tip
    }

    pub(crate) const fn child_tempo_tip(&self) -> BlockNumHash {
        self.child_tempo_tip
    }
}

impl InMemoryChecker {
    pub(crate) fn new(
        model: ModelState,
        portal_creation_block_hash: B256,
        zone_tip: BlockNumHash,
        tempo_tip: BlockNumHash,
    ) -> Self {
        let metrics = CheckerMetrics::default();
        metrics
            .latest_observed_zone_height
            .set(zone_tip.number as f64);
        metrics
            .latest_checked_zone_height
            .set(zone_tip.number as f64);
        metrics.model_lag_blocks.set(0.0);
        Self {
            model,
            portal_creation_block_hash,
            zone_tip,
            tempo_tip,
            metrics,
        }
    }

    pub(crate) const fn model(&self) -> &ModelState {
        &self.model
    }

    pub(crate) const fn zone_tip(&self) -> BlockNumHash {
        self.zone_tip
    }

    pub(crate) const fn tempo_tip(&self) -> BlockNumHash {
        self.tempo_tip
    }

    /// Authenticate a canonical block and run the complete in-memory check.
    ///
    /// This convenience path is retained for non-persistent pipeline scenarios.
    /// The live runtime calls [`Self::prepare_observed_block`] and adopts only
    /// after its store commit succeeds.
    pub(crate) async fn observe_and_check_block<P, S>(
        &mut self,
        l1_provider: &P,
        zone_state: &S,
        block: &RecoveredBlock<Block>,
        receipts: &[TempoReceipt],
    ) -> Result<(), CheckError>
    where
        P: Provider<TempoNetwork>,
        S: ExactStateLookup + ?Sized,
    {
        let prepared = self
            .prepare_observed_block(l1_provider, zone_state, block, receipts)
            .await?;
        self.apply_prepared(prepared);
        Ok(())
    }

    /// Authenticate and fully check a block without changing the loaded parent.
    ///
    /// The returned value owns every logical mutation needed by persistence and
    /// contains no provider handle or borrowed observation.
    pub(crate) async fn prepare_observed_block<P, S>(
        &self,
        l1_provider: &P,
        zone_state: &S,
        block: &RecoveredBlock<Block>,
        receipts: &[TempoReceipt],
    ) -> Result<PreparedBlock, CheckError>
    where
        P: Provider<TempoNetwork>,
        S: ExactStateLookup + ?Sized,
    {
        let block_number = block.header().number();
        self.metrics
            .latest_observed_zone_height
            .set(block_number as f64);
        let observation_started = Instant::now();
        let l2 = match observe_l2_block(block, receipts) {
            Ok(observation) => observation,
            Err(error) => {
                self.metrics
                    .observation_duration_seconds
                    .record(observation_started.elapsed().as_secs_f64());
                return self.finish_observation_error(block_number, error);
            }
        };
        let l1 = match observe_l1(
            l1_provider,
            l2.inputs().advance_tempo().imported_header(),
            self.model.portal().identity().portal(),
        )
        .await
        {
            Ok(observation) => observation,
            Err(error) => {
                self.metrics
                    .observation_duration_seconds
                    .record(observation_started.elapsed().as_secs_f64());
                return self.finish_observation_error(block_number, error);
            }
        };
        self.metrics
            .observation_duration_seconds
            .record(observation_started.elapsed().as_secs_f64());
        self.prepare_block(l1_provider, zone_state, &l1, &l2).await
    }

    /// Check and atomically materialize one already-authenticated child of the
    /// current in-memory tip.
    ///
    /// All external values are acquired before `self.model` or either tip is
    /// updated. A returned error therefore leaves the verified parent intact.
    pub(crate) async fn check_block<P, S>(
        &mut self,
        l1_provider: &P,
        zone_state: &S,
        l1: &L1BlockObservation,
        l2: &L2BlockObservation,
    ) -> Result<(), CheckError>
    where
        P: Provider<TempoNetwork>,
        S: ExactStateLookup + ?Sized,
    {
        let prepared = self.prepare_block(l1_provider, zone_state, l1, l2).await?;
        self.apply_prepared(prepared);
        Ok(())
    }

    /// Check an already-authenticated child without mutating the loaded parent.
    pub(crate) async fn prepare_block<P, S>(
        &self,
        l1_provider: &P,
        zone_state: &S,
        l1: &L1BlockObservation,
        l2: &L2BlockObservation,
    ) -> Result<PreparedBlock, CheckError>
    where
        P: Provider<TempoNetwork>,
        S: ExactStateLookup + ?Sized,
    {
        self.metrics
            .latest_observed_zone_height
            .set(l2.block_number() as f64);
        let check_started = Instant::now();
        let mut timings = CheckTimings::default();
        let result = self
            .check_candidate(l1_provider, zone_state, l1, l2, &mut timings)
            .await;
        let transition = check_started
            .elapsed()
            .saturating_sub(timings.acquisition());

        self.metrics
            .transition_duration_seconds
            .record(transition.as_secs_f64());
        if let Err(error) = &result {
            self.record_error(l2.block_number(), error);
        }
        result
    }

    /// Apply a candidate to the exact in-memory parent that prepared it.
    pub(crate) fn apply_prepared(&mut self, prepared: PreparedBlock) {
        assert_eq!(
            self.zone_tip, prepared.parent_zone_tip,
            "prepared Zone parent changed before application"
        );
        assert_eq!(
            self.tempo_tip, prepared.parent_tempo_tip,
            "prepared Tempo parent changed before application"
        );
        prepared
            .state_update
            .apply_to_current_parent(&mut self.model);
        self.zone_tip = prepared.child_zone_tip;
        self.tempo_tip = prepared.child_tempo_tip;
        self.record_passed(self.zone_tip.number);
    }

    async fn check_candidate<P, S>(
        &self,
        l1_provider: &P,
        zone_state: &S,
        l1: &L1BlockObservation,
        l2: &L2BlockObservation,
        timings: &mut CheckTimings,
    ) -> Result<PreparedBlock, CheckError>
    where
        P: Provider<TempoNetwork>,
        S: ExactStateLookup + ?Sized,
    {
        self.check_continuity(l2)?;
        let expected_portal = self.model.portal().identity().portal();
        if l1.portal_address() != expected_portal {
            return Err(Finding::PortalObservationIdentityMismatch {
                expected: expected_portal,
                actual: l1.portal_address(),
            }
            .into());
        }
        let imported_header = l2.inputs().advance_tempo().imported_header();
        let imported_projection = project_imported(l1, imported_header).map_err(Finding::from)?;
        let imported = imported_projection
            .apply(&self.model)
            .map_err(Finding::from)?;
        self.check_creation_anchor(imported_header.hash(), imported.created_portal().is_some())?;
        reconcile_imported_outputs(imported.expected(), imported_projection.outputs())
            .map_err(Finding::from)?;

        let portal_tokens = imported.tokens().collect::<Vec<_>>();
        if !portal_tokens.is_empty() {
            let portal = imported
                .created_portal()
                .ok_or(ModelError::PortalNotCreated)
                .map_err(Finding::from)?;
            for (token, state) in portal_tokens {
                self.metrics.collateral_calls_total.increment(1);
                let acquisition_started = Instant::now();
                let balance = acquire_portal_collateral(
                    l1_provider,
                    token,
                    portal.identity().portal(),
                    imported_header.hash(),
                )
                .await;
                let elapsed = acquisition_started.elapsed();
                timings.collateral += elapsed;
                self.metrics
                    .collateral_call_duration_seconds
                    .record(elapsed.as_secs_f64());
                let balance = match balance {
                    Ok(balance) => balance,
                    Err(error) => {
                        self.metrics.collateral_call_failures_total.increment(1);
                        return Err(error.into());
                    }
                };
                reconcile_collateral(token, state, balance)?;
            }
        }

        let zone_projection = project_zone(l2).map_err(Finding::from)?;
        let completed = zone_projection.apply(imported).map_err(Finding::from)?;
        reconcile_zone_outputs(
            imported_header,
            completed.expected(),
            zone_projection.outputs(),
        )
        .map_err(Finding::from)?;
        let expected_post_state =
            completed.expected_post_zone_state(imported_header.hash(), imported_header.number());
        let supply_tokens = completed
            .tokens()
            .filter_map(|(token, state)| state.is_zone_enabled().then_some(token))
            .collect::<Vec<_>>();

        self.metrics.exact_state_reads_total.increment(1);
        self.metrics
            .supply_tokens_requested_total
            .increment(supply_tokens.len() as u64);
        let acquisition_started = Instant::now();
        let actual_post_state =
            acquire_zone_post_state(zone_state, l2.block_hash(), &supply_tokens);
        let elapsed = acquisition_started.elapsed();
        timings.exact_state += elapsed;
        self.metrics
            .exact_state_read_duration_seconds
            .record(elapsed.as_secs_f64());
        let actual_post_state = match actual_post_state {
            Ok(state) => state,
            Err(error) => {
                self.metrics.exact_state_read_failures_total.increment(1);
                return Err(error.into());
            }
        };

        reconcile_post_zone_state(expected_post_state, completed.tokens(), &actual_post_state)?;
        let state_update = completed.into_state_update();

        Ok(PreparedBlock {
            state_update,
            parent_zone_tip: self.zone_tip,
            parent_tempo_tip: self.tempo_tip,
            child_zone_tip: BlockNumHash::new(l2.block_number(), l2.block_hash()),
            child_tempo_tip: BlockNumHash::new(imported_header.number(), imported_header.hash()),
        })
    }

    fn check_continuity(&self, l2: &L2BlockObservation) -> Result<(), Finding> {
        if l2.block_number().checked_sub(1) != Some(self.zone_tip.number)
            || l2.parent_hash() != self.zone_tip.hash
        {
            return Err(Finding::ZoneContinuity {
                expected_number: self.zone_tip.number,
                expected_hash: self.zone_tip.hash,
                actual_number: l2.block_number(),
                actual_parent: l2.parent_hash(),
            });
        }

        let imported = l2.inputs().advance_tempo().imported_header();
        if imported.number().checked_sub(1) != Some(self.tempo_tip.number)
            || imported.header().parent_hash() != self.tempo_tip.hash
        {
            return Err(Finding::TempoContinuity {
                expected_number: self.tempo_tip.number,
                expected_hash: self.tempo_tip.hash,
                actual_number: imported.number(),
                actual_parent: imported.header().parent_hash(),
            });
        }
        Ok(())
    }

    fn check_creation_anchor(&self, imported_hash: B256, created: bool) -> Result<(), Finding> {
        let was_created = self.model.portal().created().is_some();
        if !was_created && created && imported_hash != self.portal_creation_block_hash {
            return Err(Finding::PortalCreationBlockMismatch {
                expected: self.portal_creation_block_hash,
                actual: imported_hash,
            });
        }
        if !was_created && !created && imported_hash == self.portal_creation_block_hash {
            return Err(Finding::PortalCreationMissing {
                block_hash: imported_hash,
            });
        }
        Ok(())
    }

    fn finish_observation_error<T>(
        &self,
        block_number: u64,
        error: ObservationError,
    ) -> Result<T, CheckError> {
        let error = error.into();
        self.record_error(block_number, &error);
        Err(error)
    }

    fn record_error(&self, observed_number: u64, error: &CheckError) {
        match error {
            CheckError::Acquisition(_) => {
                self.metrics.acquisition_failures_total.increment(1);
            }
            CheckError::Finding(_) => self.metrics.findings_total.increment(1),
        }
        self.record_progress(observed_number);
    }

    fn record_passed(&self, observed_number: u64) {
        self.metrics.passed_blocks_total.increment(1);
        self.record_progress(observed_number);
    }

    fn record_progress(&self, observed_number: u64) {
        self.metrics
            .latest_checked_zone_height
            .set(self.zone_tip.number as f64);
        self.metrics
            .model_lag_blocks
            .set(observed_number.saturating_sub(self.zone_tip.number) as f64);
    }
}
