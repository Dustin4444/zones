//! Checker inspection, acknowledgement, and fault controls for integration tests.

use std::{collections::BTreeMap, path::Path};

#[cfg(feature = "test-utils")]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[cfg(feature = "test-utils")]
use std::time::Duration;

use alloy_eips::BlockNumHash;
use alloy_primitives::Address;

#[cfg(feature = "test-utils")]
use crate::{CheckerConfig, CheckerExEx};
use crate::{
    model::state::TokenPhase,
    store::{db::CheckerStore, value::BootstrapState},
};

#[cfg(feature = "test-utils")]
use crate::observe::ZonePostStateOutputs;

/// Integration-test hooks kept out of production checker configuration.
#[cfg(feature = "test-utils")]
pub(crate) struct CheckerTestHooks {
    sender: Option<tokio::sync::watch::Sender<Option<BlockNumHash>>>,
    supply_observation_fault: SupplyObservationFault,
    start_gate: Option<tokio::sync::oneshot::Receiver<()>>,
}

#[cfg(feature = "test-utils")]
impl CheckerTestHooks {
    pub(crate) const fn disabled() -> Self {
        Self {
            sender: None,
            supply_observation_fault: SupplyObservationFault::disabled(),
            start_gate: None,
        }
    }

    fn channel(
        supply_observation_fault: SupplyObservationFault,
        start_gate: Option<tokio::sync::oneshot::Receiver<()>>,
    ) -> (Self, tokio::sync::watch::Receiver<Option<BlockNumHash>>) {
        let (sender, receiver) = tokio::sync::watch::channel(None);
        (
            Self {
                sender: Some(sender),
                supply_observation_fault,
                start_gate,
            },
            receiver,
        )
    }

    pub(crate) fn publish(&self, tip: BlockNumHash) {
        if let Some(sender) = &self.sender {
            sender.send_replace(Some(tip));
        }
    }

    pub(crate) fn supply_observation_fault(&self) -> SupplyObservationFault {
        self.supply_observation_fault.clone()
    }

    pub(crate) async fn wait_for_start(&mut self) {
        if let Some(start_gate) = self.start_gate.take() {
            let _ = start_gate.await;
        }
    }
}

/// Per-checker, one-shot perturbation of an acquired token supply.
#[cfg(feature = "test-utils")]
#[derive(Clone)]
pub(crate) struct SupplyObservationFault {
    armed: Option<Arc<AtomicBool>>,
}

#[cfg(feature = "test-utils")]
impl SupplyObservationFault {
    pub(crate) const fn disabled() -> Self {
        Self { armed: None }
    }

    fn controllable() -> Self {
        Self {
            armed: Some(Arc::new(AtomicBool::new(false))),
        }
    }

    fn trigger(&self) -> SupplyMismatchTrigger {
        SupplyMismatchTrigger {
            armed: Arc::clone(
                self.armed
                    .as_ref()
                    .expect("controllable test fault must have a trigger"),
            ),
        }
    }

    pub(crate) fn perturb(&self, state: &mut ZonePostStateOutputs) {
        if state.token_supplies().is_empty() {
            return;
        }
        if self
            .armed
            .as_ref()
            .is_some_and(|armed| armed.swap(false, Ordering::AcqRel))
        {
            state.perturb_first_token_supply_for_test();
        }
    }
}

/// Arms one deterministic mismatch in the next acquired nonempty supply set.
#[cfg(feature = "test-utils")]
pub struct SupplyMismatchTrigger {
    armed: Arc<AtomicBool>,
}

#[cfg(feature = "test-utils")]
impl SupplyMismatchTrigger {
    pub fn arm_next(&self) {
        assert!(
            !self.armed.swap(true, Ordering::AcqRel),
            "a supply mismatch is already armed"
        );
    }
}

/// Receives exact checker tips after their durable cut is acknowledged.
#[cfg(feature = "test-utils")]
pub struct CheckerProgress {
    receiver: tokio::sync::watch::Receiver<Option<BlockNumHash>>,
}

#[cfg(feature = "test-utils")]
impl CheckerProgress {
    /// Wait until the checker acknowledges `expected` or the timeout expires.
    pub async fn wait_for(
        &mut self,
        expected: BlockNumHash,
        timeout: Duration,
    ) -> eyre::Result<()> {
        tokio::time::timeout(timeout, async {
            loop {
                if *self.receiver.borrow() == Some(expected) {
                    return Ok(());
                }
                self.receiver.changed().await.map_err(|_| {
                    eyre::eyre!("checker progress channel closed before {expected:?}")
                })?;
            }
        })
        .await
        .map_err(|_| eyre::eyre!("checker did not acknowledge {expected:?} within {timeout:?}"))?
    }
}

/// Construct a checker ExEx paired with an acknowledgement receiver.
#[cfg(feature = "test-utils")]
pub fn checker_with_progress(config: CheckerConfig) -> (CheckerExEx, CheckerProgress) {
    checker_with_hooks(config, SupplyObservationFault::disabled(), None)
}

/// Construct a checker that initializes its durable genesis cut, then waits
/// until the returned sender releases its operational notification loop.
#[cfg(feature = "test-utils")]
pub fn checker_with_paused_progress(
    config: CheckerConfig,
) -> (
    CheckerExEx,
    CheckerProgress,
    tokio::sync::oneshot::Sender<()>,
) {
    let (release, start_gate) = tokio::sync::oneshot::channel();
    let (checker, progress) =
        checker_with_hooks(config, SupplyObservationFault::disabled(), Some(start_gate));
    (checker, progress, release)
}

/// Construct a test checker paired with acknowledgement and an explicit,
/// per-instance supply-observation fault trigger.
#[cfg(feature = "test-utils")]
pub fn checker_with_progress_and_supply_mismatch(
    config: CheckerConfig,
) -> (CheckerExEx, CheckerProgress, SupplyMismatchTrigger) {
    let fault = SupplyObservationFault::controllable();
    let trigger = fault.trigger();
    let (checker, progress) = checker_with_hooks(config, fault, None);
    (checker, progress, trigger)
}

#[cfg(feature = "test-utils")]
fn checker_with_hooks(
    config: CheckerConfig,
    supply_observation_fault: SupplyObservationFault,
    start_gate: Option<tokio::sync::oneshot::Receiver<()>>,
) -> (CheckerExEx, CheckerProgress) {
    let (test_hooks, receiver) = CheckerTestHooks::channel(supply_observation_fault, start_gate);
    (
        CheckerExEx { config, test_hooks },
        CheckerProgress { receiver },
    )
}

/// Coarse durable bootstrap phase exposed without leaking cursor internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckerBootstrapPhase {
    L1Replay,
    ZoneReplay,
    Live,
}

/// Durable lifecycle phase for one checker token row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckerTokenPhase {
    PendingZoneEnable,
    ZoneEnabled,
}

/// Validated authoritative checker cut used by integration tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckerSnapshot {
    pub verified_zone_tip: BlockNumHash,
    pub imported_tempo_tip: BlockNumHash,
    pub bootstrap: CheckerBootstrapPhase,
    pub portal_created: bool,
    pub active_alert: bool,
    pub tokens: BTreeMap<Address, CheckerTokenPhase>,
}

/// Open an existing checker database read-only and validate its complete model cut.
pub fn inspect_database(path: impl AsRef<Path>) -> eyre::Result<CheckerSnapshot> {
    let path = path.as_ref();
    let identity = CheckerStore::inspect_identity_at(path)?;
    let snapshot = CheckerStore::inspect_existing_at(path, identity)?;
    let bootstrap = match snapshot.bootstrap {
        BootstrapState::L1Replay { .. } => CheckerBootstrapPhase::L1Replay,
        BootstrapState::ZoneReplay { .. } => CheckerBootstrapPhase::ZoneReplay,
        BootstrapState::Live => CheckerBootstrapPhase::Live,
    };
    let tokens = snapshot
        .model
        .tokens()
        .iter()
        .map(|(&token, state)| {
            let phase = match state.phase() {
                TokenPhase::PendingZoneEnable => CheckerTokenPhase::PendingZoneEnable,
                TokenPhase::ZoneEnabled => CheckerTokenPhase::ZoneEnabled,
            };
            (token, phase)
        })
        .collect();

    Ok(CheckerSnapshot {
        verified_zone_tip: snapshot.verified_zone_tip,
        imported_tempo_tip: snapshot.imported_tempo_tip,
        bootstrap,
        portal_created: snapshot.model.portal().created().is_some(),
        active_alert: snapshot.active_alert.is_some(),
        tokens,
    })
}
