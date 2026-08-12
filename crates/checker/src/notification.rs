//! Validated runtime plans derived from ExEx notifications.

use crate::{failure::Failure, persistence::BlockNumHash};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotificationPlan {
    pub(crate) reverted: Vec<BlockNumHash>,
    pub(crate) ancestor: BlockNumHash,
    pub(crate) applied: Vec<BlockNumHash>,
    pub(crate) acknowledge: BlockNumHash,
}

impl NotificationPlan {
    /// Build and validate a plan from one canonical notification.
    pub(crate) fn new(
        reverted: Vec<BlockNumHash>,
        applied: Vec<BlockNumHash>,
        ancestor: BlockNumHash,
    ) -> Result<Self, Failure> {
        let acknowledge = applied.last().copied().unwrap_or(ancestor);
        Self {
            reverted,
            ancestor,
            applied,
            acknowledge,
        }
        .validate()
    }

    /// Reject non-contiguous, empty, or internally inconsistent notification coordinates.
    pub(crate) fn validate(self) -> Result<Self, Failure> {
        let contiguous = |values: &[BlockNumHash]| {
            values
                .windows(2)
                .all(|pair| pair[0].number.checked_add(1) == Some(pair[1].number))
        };
        if (!self.reverted.is_empty() && !contiguous(&self.reverted))
            || (!self.applied.is_empty() && !contiguous(&self.applied))
            || self
                .reverted
                .first()
                .is_some_and(|block| self.ancestor.number.checked_add(1) != Some(block.number))
            || self
                .applied
                .first()
                .is_some_and(|block| self.ancestor.number.checked_add(1) != Some(block.number))
            || (self.applied.is_empty() && self.acknowledge != self.ancestor)
            || self
                .applied
                .last()
                .is_some_and(|block| *block != self.acknowledge)
            || (self.reverted.is_empty() && self.applied.is_empty())
        {
            return Err(Failure::terminal("invalid notification shape"));
        }
        Ok(self)
    }
}
