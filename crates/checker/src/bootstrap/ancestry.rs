//! Authenticates Tempo ancestry used to reconstruct the bootstrap state.

use alloy_consensus::BlockHeader as _;
use alloy_eips::BlockNumHash;
use alloy_provider::DynProvider;
use tempo_alloy::TempoNetwork;

use crate::observe::{ImportedTempoHeader, acquire_l1_header};

use super::BootstrapError;

/// Acquire the exact Tempo header named by the Zone genesis checkpoint.
pub(super) async fn anchor_header(
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

/// Prove and return the inclusive, hash-linked path from ancestor to descendant.
pub(super) async fn authenticated_path(
    provider: &DynProvider<TempoNetwork>,
    ancestor: &ImportedTempoHeader,
    descendant: ImportedTempoHeader,
) -> eyre::Result<Vec<ImportedTempoHeader>> {
    let ancestor_tip = BlockNumHash::new(ancestor.number(), ancestor.hash());
    let descendant_tip = BlockNumHash::new(descendant.number(), descendant.hash());
    if descendant_tip.number < ancestor_tip.number {
        return Err(BootstrapError::InvalidTempoAncestryRange {
            descendant: descendant_tip,
            ancestor: ancestor_tip,
        }
        .into());
    }
    if descendant_tip == ancestor_tip {
        return Ok(vec![ancestor.clone()]);
    }
    if descendant_tip.number == ancestor_tip.number {
        return Err(BootstrapError::TempoAncestryNotLinked {
            descendant: descendant_tip,
            expected_ancestor: ancestor_tip,
            reached: descendant_tip,
        }
        .into());
    }

    let mut current = descendant;
    let mut descending = Vec::new();
    while current.number() > ancestor_tip.number {
        let child = BlockNumHash::new(current.number(), current.hash());
        let parent_hash = current.header().parent_hash();
        descending.push(current.clone());
        if ancestor_tip.number.checked_add(1) == Some(current.number()) {
            if parent_hash != ancestor_tip.hash {
                return Err(BootstrapError::TempoAncestryNotLinked {
                    descendant: descendant_tip,
                    expected_ancestor: ancestor_tip,
                    reached: BlockNumHash::new(ancestor_tip.number, parent_hash),
                }
                .into());
            }
            descending.reverse();
            let mut path = Vec::with_capacity(descending.len() + 1);
            path.push(ancestor.clone());
            path.extend(descending);
            return Ok(path);
        }
        let parent = acquire_l1_header(provider, parent_hash).await?;
        if parent.number().checked_add(1) != Some(current.number()) {
            return Err(BootstrapError::NonConsecutiveTempoAncestry {
                child,
                expected_parent: BlockNumHash::new(current.number().saturating_sub(1), parent_hash),
                actual_parent: BlockNumHash::new(parent.number(), parent.hash()),
            }
            .into());
        }
        current = parent;
    }
    Err(BootstrapError::TempoAncestryNotLinked {
        descendant: descendant_tip,
        expected_ancestor: ancestor_tip,
        reached: BlockNumHash::new(current.number(), current.hash()),
    }
    .into())
}
