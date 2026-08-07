//! Independent semantic kernel for the observe-only Zone checker.
//!
//! This crate deliberately has no dependency on production Zone semantics.

mod apply;
mod commitments;
mod effects;
mod facts;
mod finding;
mod invariants;
mod state;

pub use apply::{Candidate, ImportedCandidate, ModelError, apply_imported, apply_zone};
pub use effects::{ExpectedEffect, ExpectedState};
pub use facts::{
    DepositOutcome, DepositPayload, ImportedFacts, ImportedOperation, OrdinaryDeposit, TokenEnable,
    ZoneFacts,
};
pub use finding::{Datum, Finding, FindingCategory, FindingLocation};
pub use invariants::{InvariantCode, InvariantViolation, validate};
pub use state::{
    BatchId, BatchState, Cursor, DepositId, DepositOwner, FallbackId, FallbackState, InboxRefundId,
    PortalIdentity, PortalRefundId, PortalState, RefundCredit, State, StateDelta, StateFamilyError,
    StateKey, StateValue, TokenAccounting, TokenPhase, TokenState, WithdrawalId, WithdrawalOwner,
    ZoneState,
};

#[cfg(test)]
mod tests;
