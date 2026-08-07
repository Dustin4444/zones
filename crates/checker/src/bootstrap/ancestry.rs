use alloy_consensus::BlockHeader as _;
use alloy_eips::BlockNumHash;
use alloy_provider::DynProvider;
use tempo_alloy::TempoNetwork;

use crate::observe::{ImportedTempoHeader, acquire_l1_header};

use super::error::BootstrapError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FreshHistory {
    PortalPresentAtGenesisAnchor,
    PortalCreatedAfterGenesisAnchor,
}

pub(crate) fn classify_fresh_history(creation: BlockNumHash, anchor: BlockNumHash) -> FreshHistory {
    if creation.number <= anchor.number {
        FreshHistory::PortalPresentAtGenesisAnchor
    } else {
        FreshHistory::PortalCreatedAfterGenesisAnchor
    }
}

pub(crate) async fn acquire_anchor_header(
    provider: &DynProvider<TempoNetwork>,
    anchor: BlockNumHash,
) -> eyre::Result<ImportedTempoHeader> {
    let header = acquire_l1_header(provider, anchor.hash).await?;
    if header.number() != anchor.number {
        return Err(BootstrapError::GenesisAnchorNumberMismatch {
            hash: anchor.hash,
            checkpoint_number: anchor.number,
            header_number: header.number(),
        }
        .into());
    }
    Ok(header)
}

/// Return the hash-linked ancestor-to-descendant header sequence, inclusive.
///
/// Reuse the acquired headers during replay instead of fetching them again.
pub(crate) async fn prove_ancestry(
    provider: &DynProvider<TempoNetwork>,
    descendant: ImportedTempoHeader,
    ancestor: &ImportedTempoHeader,
) -> eyre::Result<Vec<ImportedTempoHeader>> {
    let ancestor_tip = header_tip(ancestor);
    let descendants = prove_descendants_after(provider, descendant, ancestor_tip).await?;
    let mut path = Vec::with_capacity(descendants.len() + 1);
    path.push(ancestor.clone());
    path.extend(descendants);
    Ok(path)
}

/// Return hash-linked descendants strictly after an ancestor.
///
/// The first child's parent link checks the boundary without fetching it again.
pub(super) async fn prove_descendants_after(
    provider: &DynProvider<TempoNetwork>,
    descendant: ImportedTempoHeader,
    ancestor: BlockNumHash,
) -> eyre::Result<Vec<ImportedTempoHeader>> {
    let descendant_tip = header_tip(&descendant);
    if descendant_tip.number < ancestor.number {
        return Err(BootstrapError::InvalidTempoAncestryRange {
            descendant: descendant_tip,
            ancestor,
        }
        .into());
    }
    if descendant_tip == ancestor {
        return Ok(Vec::new());
    }
    if descendant_tip.number == ancestor.number {
        return Err(BootstrapError::TempoAncestryNotLinked {
            descendant: descendant_tip,
            expected_ancestor: ancestor,
            reached: descendant_tip,
        }
        .into());
    }

    let mut current = descendant;
    let mut descending = Vec::new();
    while current.number() > ancestor.number {
        let child_tip = header_tip(&current);
        let parent_hash = current.header().parent_hash();
        descending.push(current.clone());
        if ancestor.number.checked_add(1) == Some(current.number()) {
            if parent_hash != ancestor.hash {
                return Err(BootstrapError::TempoAncestryNotLinked {
                    descendant: descendant_tip,
                    expected_ancestor: ancestor,
                    reached: BlockNumHash::new(ancestor.number, parent_hash),
                }
                .into());
            }
            descending.reverse();
            return Ok(descending);
        }

        let parent = acquire_l1_header(provider, parent_hash).await?;
        if parent.number().checked_add(1) != Some(current.number()) {
            return Err(BootstrapError::NonConsecutiveTempoAncestry {
                child: child_tip,
                expected_parent: BlockNumHash::new(current.number().saturating_sub(1), parent_hash),
                actual_parent: header_tip(&parent),
            }
            .into());
        }
        current = parent;
    }

    Err(BootstrapError::TempoAncestryNotLinked {
        descendant: descendant_tip,
        expected_ancestor: ancestor,
        reached: header_tip(&current),
    }
    .into())
}

pub(crate) fn header_tip(header: &ImportedTempoHeader) -> BlockNumHash {
    BlockNumHash::new(header.number(), header.hash())
}
