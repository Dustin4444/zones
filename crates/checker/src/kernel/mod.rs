//! State model and deterministic transition rules for the observe-only Zone checker.
//!
//! This module does not depend on production Zone transition code.

mod derivation;
mod effects;
mod facts;
mod finding;
mod invariants;
mod state;
mod transition;

pub(crate) use effects::{Effect, ExpectedState};
pub(crate) use facts::{
    BatchSubmission, BounceBackDeposit, Deposit, DepositOutcome, DepositPayload, Finalization,
    ImportedFacts, ImportedOperation, OrdinaryDeposit, PortalCallbackOperation, RefundClaim,
    TokenEnable, UserWithdrawal, WithdrawalOutcome, WithdrawalProcessing, ZoneFacts, ZoneOperation,
};
pub(crate) use finding::{Datum, Finding, FindingCategory, FindingLocation};
pub(crate) use invariants::validate;
pub(crate) use state::{
    BatchId, Cursor, DepositId, PortalIdentity, PortalState, State, StateDelta, StateKey,
    TokenPhase, Withdrawal, WithdrawalId,
};
pub(crate) use transition::{
    TransitionCandidate, apply_genesis_handoff, apply_imported, apply_zone,
};

#[cfg(test)]
pub(crate) use state::StateValue;

#[cfg(test)]
pub(crate) use transition::TransitionError;

#[cfg(test)]
mod tests;
