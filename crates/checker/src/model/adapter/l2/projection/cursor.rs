//! Typed cursor over canonical Zone protocol outcomes.

use std::{iter::Peekable, slice};

use alloy_primitives::B256;

use crate::observe::OrderedL2Outcome;

use super::super::{ObservedZoneEventPosition, ZoneProjectionError, event_kind};

/// Single owner of event-stream advancement for the Zone grammar.
pub(super) struct ZoneEventCursor<'a> {
    events: Peekable<slice::Iter<'a, OrderedL2Outcome>>,
    advance_hash: B256,
}

impl<'a> ZoneEventCursor<'a> {
    pub(super) fn new(events: &'a [OrderedL2Outcome], advance_hash: B256) -> Self {
        Self {
            events: events.iter().peekable(),
            advance_hash,
        }
    }

    /// Consume the next transaction-zero event, retaining later transactions
    /// for the post-advance grammar.
    pub(super) fn next_advance(
        &mut self,
        missing: ZoneProjectionError,
    ) -> Result<&'a OrderedL2Outcome, ZoneProjectionError> {
        let Some(outcome) = self.events.peek().copied() else {
            return Err(missing);
        };
        if outcome.position().transaction_index() != 0 {
            return Err(missing);
        }
        self.events.next();

        let position = observed_position(outcome);
        if position.transaction_hash != self.advance_hash {
            return Err(ZoneProjectionError::AdvanceTransactionHashMismatch {
                expected: self.advance_hash,
                position,
            });
        }
        Ok(outcome)
    }

    /// Reject protocol output left in the opening system transaction after the
    /// terminal `TempoAdvanced` event.
    pub(super) fn finish_advance(&mut self) -> Result<(), ZoneProjectionError> {
        let Some(outcome) = self.events.peek().copied() else {
            return Ok(());
        };
        if outcome.position().transaction_index() != 0 {
            return Ok(());
        }
        self.events.next();
        Err(ZoneProjectionError::ExtraAdvanceEvent {
            actual: event_kind(outcome.event()),
            position: observed_position(outcome),
        })
    }

    /// Consume one ordinary post-advance event without crossing into the final
    /// system transaction.
    pub(super) fn next_before_finalization(
        &mut self,
        finalization_hash: Option<B256>,
    ) -> Option<&'a OrderedL2Outcome> {
        let outcome = self.events.peek().copied()?;
        if finalization_hash.is_some_and(|hash| outcome.position().transaction_hash() == hash) {
            return None;
        }
        self.events.next();
        Some(outcome)
    }

    pub(super) fn next(&mut self) -> Option<&'a OrderedL2Outcome> {
        self.events.next()
    }

    pub(super) fn is_empty(&mut self) -> bool {
        self.events.peek().is_none()
    }
}

pub(super) fn observed_position(outcome: &OrderedL2Outcome) -> ObservedZoneEventPosition {
    let position = outcome.position();
    ObservedZoneEventPosition {
        transaction_index: position.transaction_index(),
        receipt_log_index: position.receipt_log_index(),
        block_log_index: position.block_log_index(),
        transaction_hash: position.transaction_hash(),
    }
}
