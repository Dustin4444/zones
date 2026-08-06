//! Durable runtime state and startup reconciliation.

use alloy_eips::BlockNumHash;
use reth_storage_api::{BlockHashReader, BlockNumReader};

use crate::{
    check::pipeline::InMemoryChecker,
    store::{
        db::{CheckerStore, StoreSnapshot},
        error::StoreError,
        value::{ActiveAlert, BootstrapState},
    },
};

use super::{RuntimeError, RuntimeResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Acknowledgement {
    Verified(BlockNumHash),
    Alerted(BlockNumHash),
}

/// A canonical height that may now advance Reth's pruning watermark.
///
/// `Verified` follows a model commit or exact unwind. `Alerted` is available
/// only while a committed `ActiveAlert` makes descendant checking deliberately
/// unnecessary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReadyToAcknowledge(Acknowledgement);

impl ReadyToAcknowledge {
    pub(super) const fn verified(tip: BlockNumHash) -> Self {
        Self(Acknowledgement::Verified(tip))
    }

    pub(super) const fn alerted(tip: BlockNumHash) -> Self {
        Self(Acknowledgement::Alerted(tip))
    }

    pub(crate) const fn tip(self) -> BlockNumHash {
        match self.0 {
            Acknowledgement::Verified(tip) | Acknowledgement::Alerted(tip) => tip,
        }
    }

    pub(crate) const fn is_alerting(self) -> bool {
        matches!(self.0, Acknowledgement::Alerted(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LivePhase {
    Verifying,
    Alerting(ActiveAlert),
}

/// Sole owner of one checker store and its post-commit in-memory mirror.
pub(crate) struct LiveChecker {
    pub(super) store: CheckerStore,
    pub(super) mirror: InMemoryChecker,
    pub(super) phase: LivePhase,
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

        let phase = snapshot
            .active_alert
            .map_or(LivePhase::Verifying, LivePhase::Alerting);
        let mirror = mirror_from_snapshot(&store, snapshot);
        Ok(Self {
            store,
            mirror,
            phase,
        })
    }

    #[cfg(test)]
    pub(crate) const fn mirror_tip(&self) -> BlockNumHash {
        self.mirror.zone_tip()
    }

    pub(crate) const fn is_alerting(&self) -> bool {
        matches!(self.phase, LivePhase::Alerting(_))
    }

    pub(super) const fn active_alert(&self) -> Option<ActiveAlert> {
        match self.phase {
            LivePhase::Verifying => None,
            LivePhase::Alerting(alert) => Some(alert),
        }
    }

    pub(super) fn reload_mirror(&mut self) -> RuntimeResult<()> {
        let snapshot = self.store.load_current()?;
        self.mirror = mirror_from_snapshot(&self.store, snapshot);
        Ok(())
    }

    /// Reconcile durable progress with local canonical hashes before asking
    /// Reth to synthesize catch-up notifications.
    pub(crate) fn reconcile_startup<P>(&mut self, provider: &P) -> RuntimeResult<ReadyToAcknowledge>
    where
        P: BlockNumReader + ?Sized,
    {
        if let Some(alert) = self.active_alert() {
            let finding_tip =
                BlockNumHash::new(alert.finding.zone_height(), alert.finding.zone_hash());
            if let Some(head) = canonical_alert_head(provider, finding_tip)? {
                return Ok(ReadyToAcknowledge::alerted(head));
            }
            self.store.orphan_active_finding(alert.finding)?;
            self.phase = LivePhase::Verifying;
        }

        loop {
            let tip = self.store.load_current()?.verified_zone_tip;
            if local_hash(provider, tip)? == Some(tip.hash) {
                self.reload_mirror()?;
                return Ok(ReadyToAcknowledge::verified(tip));
            }
            if tip.number == 0 {
                return Err(RuntimeError::NonCanonicalGenesis { tip });
            }
            self.store.unwind_tip(tip)?;
        }
    }

    #[cfg(test)]
    pub(crate) fn current_snapshot_for_test(&self) -> StoreSnapshot {
        self.store.load_current().unwrap()
    }

    #[cfg(test)]
    pub(crate) fn finding_for_test(
        &self,
        key: crate::store::schema::FindingKey,
    ) -> crate::store::value::FindingRecord {
        self.store.finding(key).unwrap().unwrap()
    }
}

/// Read one self-consistent canonical cut without requiring a long-lived
/// provider snapshot. A moving head is retried; equal head hashes commit to
/// the same ancestry at the finding height.
fn canonical_alert_head<P>(
    provider: &P,
    finding: BlockNumHash,
) -> RuntimeResult<Option<BlockNumHash>>
where
    P: BlockNumReader + ?Sized,
{
    loop {
        let before = local_head(provider)?;
        let finding_hash = local_hash(provider, finding)?;
        let after = local_head(provider)?;
        if before != after {
            continue;
        }
        if finding_hash != Some(finding.hash) {
            return Ok(None);
        }
        if after.number < finding.number {
            return Err(RuntimeError::CanonicalHeadBehindAlert {
                head: after,
                finding,
            });
        }
        return Ok(Some(after));
    }
}

fn local_head<P>(provider: &P) -> RuntimeResult<BlockNumHash>
where
    P: BlockNumReader + ?Sized,
{
    provider
        .chain_info()
        .map(Into::into)
        .map_err(|source| RuntimeError::LocalCanonicalHeadRead { source })
}

fn mirror_from_snapshot(store: &CheckerStore, snapshot: StoreSnapshot) -> InMemoryChecker {
    InMemoryChecker::new(
        snapshot.model,
        store.portal_creation_block_hash(),
        snapshot.verified_zone_tip,
        snapshot.imported_tempo_tip,
    )
}

fn local_hash<P>(provider: &P, tip: BlockNumHash) -> RuntimeResult<Option<alloy_primitives::B256>>
where
    P: BlockHashReader + ?Sized,
{
    provider
        .block_hash(tip.number)
        .map_err(|source| RuntimeError::LocalCanonicalRead { tip, source })
}
