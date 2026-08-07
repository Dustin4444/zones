//! Read-only checker database inspection for cross-crate integration tests.

use std::{collections::BTreeMap, path::Path};

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

/// Integration-test signal emitted only after the durable cut is acknowledged.
#[cfg(feature = "test-utils")]
pub(crate) struct TestProgressObserver {
    sender: Option<tokio::sync::watch::Sender<Option<BlockNumHash>>>,
}

#[cfg(feature = "test-utils")]
impl TestProgressObserver {
    pub(crate) const fn disabled() -> Self {
        Self { sender: None }
    }

    fn channel() -> (Self, tokio::sync::watch::Receiver<Option<BlockNumHash>>) {
        let (sender, receiver) = tokio::sync::watch::channel(None);
        (
            Self {
                sender: Some(sender),
            },
            receiver,
        )
    }

    pub(crate) fn publish(&self, tip: BlockNumHash) {
        if let Some(sender) = &self.sender {
            sender.send_replace(Some(tip));
        }
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
    let (test_progress, receiver) = TestProgressObserver::channel();
    (
        CheckerExEx {
            config,
            test_progress,
        },
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
