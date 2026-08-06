//! Validated geometry for one Reth canonical-chain notification.

use alloy_consensus::BlockHeader as _;
use alloy_eips::BlockNumHash;
use reth_execution_types::Chain;
use reth_primitives_traits::RecoveredBlock;
use tempo_primitives::{Block, TempoPrimitives};

use super::{RuntimeError, RuntimeResult};

/// A nonempty, consecutive chain fragment whose parent links were checked.
pub(super) struct ValidatedChain<'a> {
    chain: &'a Chain<TempoPrimitives>,
    base: BlockNumHash,
    tip: BlockNumHash,
}

impl<'a> ValidatedChain<'a> {
    pub(super) fn new(
        chain: &'a Chain<TempoPrimitives>,
        kind: &'static str,
    ) -> RuntimeResult<Self> {
        let mut blocks = chain.blocks().values();
        let first = blocks
            .next()
            .ok_or(RuntimeError::EmptyNotificationChain(kind))?;
        let first_number = first.header().number();
        let base_number =
            first_number
                .checked_sub(1)
                .ok_or(RuntimeError::InvalidNotificationChain {
                    kind,
                    reason: "notification fragment begins at genesis",
                })?;
        let base = BlockNumHash::new(base_number, first.header().parent_hash());
        let mut previous = BlockNumHash::new(first_number, first.hash());

        for block in blocks {
            let number = block.header().number();
            if previous.number.checked_add(1) != Some(number)
                || block.header().parent_hash() != previous.hash
            {
                return Err(RuntimeError::InvalidNotificationChain {
                    kind,
                    reason: "block numbers or parent hashes are not consecutive",
                });
            }
            previous = BlockNumHash::new(number, block.hash());
        }

        Ok(Self {
            chain,
            base,
            tip: previous,
        })
    }

    pub(super) const fn base(&self) -> BlockNumHash {
        self.base
    }

    pub(super) const fn tip(&self) -> BlockNumHash {
        self.tip
    }

    pub(super) fn contains(&self, block: BlockNumHash) -> bool {
        self.chain
            .blocks()
            .get(&block.number)
            .is_some_and(|candidate| candidate.hash() == block.hash)
    }

    pub(super) fn spans_height(&self, height: u64) -> bool {
        (self.base.number + 1..=self.tip.number).contains(&height)
    }

    pub(super) fn blocks(&self) -> impl DoubleEndedIterator<Item = &'a RecoveredBlock<Block>> {
        self.chain.blocks().values().map(AsRef::as_ref)
    }

    pub(super) const fn inner(&self) -> &'a Chain<TempoPrimitives> {
        self.chain
    }
}

/// Validate that Reth's old and replacement fragments fork at one exact cut.
pub(super) fn validate_reorg<'a>(
    old: &'a Chain<TempoPrimitives>,
    new: &'a Chain<TempoPrimitives>,
) -> RuntimeResult<(ValidatedChain<'a>, ValidatedChain<'a>)> {
    let old = ValidatedChain::new(old, "reverted")?;
    let new = ValidatedChain::new(new, "replacement")?;
    if old.base() != new.base() {
        return Err(RuntimeError::InvalidNotificationChain {
            kind: "reorg",
            reason: "old and replacement fragments have different common ancestors",
        });
    }
    Ok((old, new))
}
