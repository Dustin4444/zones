//! Cursor over one direct `processWithdrawals` transaction's ordered events.

use std::slice;

use super::super::{ImportedProjectionError, PositionedEvent};

/// Single owner of event-stream advancement for withdrawal member projection.
pub(super) struct WithdrawalEventCursor<'slice, 'event> {
    transaction_index: usize,
    events: slice::Iter<'slice, PositionedEvent<'event>>,
}

impl<'slice, 'event> WithdrawalEventCursor<'slice, 'event> {
    pub(super) fn new(transaction_index: usize, events: &'slice [PositionedEvent<'event>]) -> Self {
        Self {
            transaction_index,
            events: events.iter(),
        }
    }

    /// Consume the next event required by one calldata member.
    pub(super) fn next_required(
        &mut self,
        member_index: usize,
    ) -> Result<&'slice PositionedEvent<'event>, ImportedProjectionError> {
        self.events
            .next()
            .ok_or(ImportedProjectionError::MissingWithdrawalOutcome {
                transaction_index: self.transaction_index,
                member_index,
            })
    }

    /// Reject any event that was not owned by a calldata member.
    pub(super) fn finish(self) -> Result<(), ImportedProjectionError> {
        let remaining = self.events.len();
        if remaining != 0 {
            return Err(ImportedProjectionError::ExtraWithdrawalOutcomes {
                transaction_index: self.transaction_index,
                remaining,
            });
        }
        Ok(())
    }
}
