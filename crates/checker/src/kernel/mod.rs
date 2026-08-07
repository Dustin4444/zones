//! Semantic transitions for the observe-only Zone checker.
//!
//! This module does not depend on production Zone transition code.

mod apply;
mod commitments;
mod effects;
mod facts;
mod finding;
mod invariants;
mod state;

pub(crate) use apply::{Candidate, apply_genesis_handoff, apply_imported, apply_zone};
pub(crate) use effects::{Effect, ExpectedState};
pub(crate) use facts::{
    BatchSubmission, BounceBackDeposit, Deposit, DepositOutcome, DepositPayload, Finalization,
    ImportedFacts, ImportedOperation, OrdinaryDeposit, RefundClaim, TokenEnable, UserWithdrawal,
    WithdrawalOutcome, WithdrawalProcessing, ZoneFacts, ZoneOperation,
};
pub(crate) use finding::{Datum, Finding, FindingCategory, FindingLocation};
pub(crate) use invariants::validate;
pub(crate) use state::{
    BatchId, Cursor, DepositId, PortalIdentity, PortalState, State, StateDelta, StateKey,
    StateValue, TokenPhase, Withdrawal, WithdrawalId,
};

#[cfg(test)]
mod tests;
