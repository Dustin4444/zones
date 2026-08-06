//! Narrow projections from authenticated observations into pure model inputs.
//!
//! These adapters are children of `model` so input constructors remain scoped
//! to the model boundary. They normalize disposable actual outputs only far
//! enough to compare them once against independently derived expectations.

mod l1;
mod l2;

pub(crate) use l1::{
    ImportedProjectionError, ObservedDepositAppend, ObservedDepositRefund, ObservedImportedOutput,
    ObservedProcessedWithdrawal, ObservedSubmittedBatch, ObservedWithdrawalProcessed,
    ObservedWithdrawalProcessing, project_imported,
};
pub(crate) use l2::{
    ObservedBatchFinalized, ObservedDepositFailed, ObservedDepositOutcome,
    ObservedDepositProcessed, ObservedRefundClaimed, ObservedTempoAdvanced,
    ObservedTempoBlockFinalized, ObservedTokenEnabled, ObservedWithdrawalBounceBackPending,
    ObservedWithdrawalBounceBackProcessed, ObservedWithdrawalRequested, ObservedZoneOperation,
    ObservedZoneOutputs, ZoneProjectionError, project_zone,
};
