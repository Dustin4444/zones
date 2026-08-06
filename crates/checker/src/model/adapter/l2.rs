//! Authenticated Zone observation projection.

mod actual;
mod error;
mod projection;

#[cfg(test)]
mod semantic_fixtures;

#[cfg(test)]
mod tests;

pub(crate) use actual::{
    ObservedBatchFinalized, ObservedDepositFailed, ObservedDepositOutcome,
    ObservedDepositProcessed, ObservedRefundClaimed, ObservedTempoAdvanced,
    ObservedTempoBlockFinalized, ObservedTokenEnabled, ObservedWithdrawalBounceBackPending,
    ObservedWithdrawalBounceBackProcessed, ObservedWithdrawalRequested, ObservedZoneEventPosition,
    ObservedZoneOperation, ObservedZoneOutputs, ZoneProjection,
};
pub(crate) use error::{DepositInputKind, ZoneEventKind, ZoneProjectionError};
pub(crate) use projection::project_zone;

use error::event_kind;
