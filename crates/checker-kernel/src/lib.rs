//! Independent semantic kernel for the observe-only Zone checker.
//!
//! This crate has no dependency on production Zone semantics.

mod apply;
mod commitments;
mod effects;
mod facts;
mod finding;
mod invariants;
mod state;

pub use apply::{
    Candidate, ImportedCandidate, ModelError, apply_genesis_handoff, apply_imported, apply_zone,
};
pub use effects::{Effect, ExpectedState};
pub use facts::{
    BatchSubmission, BounceBackDeposit, Deposit, DepositOutcome, DepositPayload, Finalization,
    ImportedFacts, ImportedOperation, OrdinaryDeposit, RefundClaim, TokenEnable, UserWithdrawal,
    WithdrawalOutcome, WithdrawalProcessing, ZoneFacts, ZoneOperation,
};
pub use finding::{Datum, Finding, FindingCategory, FindingLocation};
pub use invariants::{InvariantCode, InvariantViolation, validate};
pub use state::{
    BatchBoundary, BatchBoundaryStart, BatchId, BatchState, Cursor, DepositId, DepositOwner,
    FallbackId, FallbackState, InboxRefundId, PortalIdentity, PortalRefundId, PortalState,
    RefundCredit, Settlement, State, StateDelta, StateFamilyError, StateKey, StateValue,
    TokenAccounting, TokenPhase, TokenState, Withdrawal, WithdrawalId, WithdrawalOrigin,
    WithdrawalOwner, ZoneState,
};

pub use commitments::{NO_QUEUE_INDEX, RING_CAPACITY, WITHDRAWAL_SENTINEL};

#[cfg(test)]
mod tests;
