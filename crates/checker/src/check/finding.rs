//! Typed acquisition failures and deterministic candidate findings.

use alloy_primitives::{Address, B256, U256};

use crate::{
    model::{
        adapter::{
            ImportedProjectionError, ObservedBatchFinalized, ObservedDepositOutcome,
            ObservedImportedOutput, ObservedTempoAdvanced, ObservedTempoBlockFinalized,
            ObservedTokenEnabled, ObservedZoneOperation, ZoneProjectionError,
        },
        output::{
            ExpectedBatchFinalized, ExpectedDepositOutcome, ExpectedImportedTempoOperation,
            ExpectedTokenEnable, ExpectedZoneOperation,
        },
        transition::ModelError,
    },
    observe::{
        AcquisitionError, DataSource, EnvelopeLocation, EnvelopeRule, ObservationError,
        PortalCallError, ProtocolChain,
    },
};

/// Acquisition failures are retryable; deterministic findings freeze the
/// candidate at its verified parent.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CheckError {
    #[error(transparent)]
    Acquisition(#[from] AcquisitionError),
    #[error(transparent)]
    Finding(Box<Finding>),
}

impl From<Finding> for CheckError {
    fn from(finding: Finding) -> Self {
        Self::Finding(Box::new(finding))
    }
}

impl From<ObservationError> for CheckError {
    fn from(error: ObservationError) -> Self {
        let finding = match error {
            ObservationError::Acquisition(error) => return Self::Acquisition(error),
            ObservationError::InvalidEnvelope { location, rule } => {
                ObservationFinding::InvalidEnvelope { location, rule }
            }
            ObservationError::MalformedAuthenticatedData { kind, detail } => {
                ObservationFinding::MalformedAuthenticatedData { kind, detail }
            }
            ObservationError::ProtocolEvent {
                chain,
                transaction_index,
                receipt_log_index,
                block_log_index,
                transaction_hash,
                error,
            } => ObservationFinding::ProtocolEvent {
                chain,
                transaction_index,
                receipt_log_index,
                block_log_index,
                transaction_hash,
                error,
            },
            ObservationError::PortalCall(error) => ObservationFinding::PortalCall(error),
        };
        Finding::Observation(Box::new(finding)).into()
    }
}

/// Deterministic failures found while authenticating a canonical candidate.
/// Acquisition variants cannot inhabit this type.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ObservationFinding {
    #[error("invalid protocol envelope {location}: {rule}")]
    InvalidEnvelope {
        location: EnvelopeLocation,
        rule: EnvelopeRule,
    },
    #[error("malformed authenticated {kind}: {detail}")]
    MalformedAuthenticatedData { kind: DataSource, detail: String },
    #[error(
        "{chain} protocol-event failure at transaction {transaction_index} ({transaction_hash}), receipt log {receipt_log_index}, block log {block_log_index}: {error}"
    )]
    ProtocolEvent {
        chain: ProtocolChain,
        transaction_index: usize,
        receipt_log_index: usize,
        block_log_index: usize,
        transaction_hash: B256,
        #[source]
        error: Box<crate::model::events::ProtocolEventError>,
    },
    #[error(transparent)]
    PortalCall(PortalCallError),
}

/// Dedicated finding families for one authenticated candidate block.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Finding {
    #[error(transparent)]
    Observation(Box<ObservationFinding>),
    #[error(
        "Zone block continuity mismatch: verified {expected_number}/{expected_hash}, candidate {actual_number} parent {actual_parent}"
    )]
    ZoneContinuity {
        expected_number: u64,
        expected_hash: B256,
        actual_number: u64,
        actual_parent: B256,
    },
    #[error(
        "Tempo block continuity mismatch: verified {expected_number}/{expected_hash}, imported {actual_number} parent {actual_parent}"
    )]
    TempoContinuity {
        expected_number: u64,
        expected_hash: B256,
        actual_number: u64,
        actual_parent: B256,
    },
    #[error("L1 observation used Portal {actual}, configured model Portal is {expected}")]
    PortalObservationIdentityMismatch { expected: Address, actual: Address },
    #[error(
        "Portal creation applied in Tempo block {actual}, configured creation block is {expected}"
    )]
    PortalCreationBlockMismatch { expected: B256, actual: B256 },
    #[error("configured Portal creation block {block_hash} did not create the Portal")]
    PortalCreationMissing { block_hash: B256 },
    #[error("authenticated Tempo projection failed: {0}")]
    ImportedProjection(#[from] ImportedProjectionError),
    #[error("authenticated Zone projection failed: {0}")]
    ZoneProjection(#[from] ZoneProjectionError),
    #[error("logical transition failed: {0}")]
    Model(#[from] ModelError),
    #[error(transparent)]
    ImportedOutput(#[from] ImportedOutputFinding),
    #[error(transparent)]
    ZoneOutput(Box<ZoneOutputFinding>),
    #[error(transparent)]
    FixedState(#[from] FixedStateFinding),
    #[error("Portal collateral deficit for token {token}: required {required}, got {actual}")]
    CollateralDeficit {
        token: Address,
        required: U256,
        actual: U256,
    },
    #[error("exact Zone supply for token {token} is absent from the acquired result")]
    MissingSupply { token: Address },
    #[error("Zone supply mismatch for token {token}: expected {expected}, got {actual}")]
    SupplyMismatch {
        token: Address,
        expected: U256,
        actual: U256,
    },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ImportedOutputFinding {
    #[error("Portal output count mismatch: expected {expected}, got {actual}")]
    Count { expected: usize, actual: usize },
    #[error("Portal output mismatch at operation {index}: expected {expected:?}, got {actual:?}")]
    Mismatch {
        index: usize,
        expected: Box<ExpectedImportedTempoOperation>,
        actual: Box<ObservedImportedOutput>,
    },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ZoneOutputFinding {
    #[error("TempoBlockFinalized mismatch: expected {expected:?}, got {actual:?}")]
    TempoBlockFinalized {
        expected: TempoBlockFinalizedExpectation,
        actual: Box<ObservedTempoBlockFinalized>,
    },
    #[error("Zone TokenEnabled count mismatch: expected {expected}, got {actual}")]
    TokenEnableCount { expected: usize, actual: usize },
    #[error("Zone TokenEnabled mismatch at index {index}: expected {expected:?}, got {actual:?}")]
    TokenEnable {
        index: usize,
        expected: Box<ExpectedTokenEnable>,
        actual: Box<ObservedTokenEnabled>,
    },
    #[error("Zone deposit-outcome count mismatch: expected {expected}, got {actual}")]
    DepositOutcomeCount { expected: usize, actual: usize },
    #[error(
        "Zone deposit outcome mismatch at index {index}: expected {expected:?}, got {actual:?}"
    )]
    DepositOutcome {
        index: usize,
        expected: Box<ExpectedDepositOutcome>,
        actual: Box<ObservedDepositOutcome>,
    },
    #[error("TempoAdvanced mismatch: expected {expected:?}, got {actual:?}")]
    TempoAdvanced {
        expected: TempoAdvancedExpectation,
        actual: Box<ObservedTempoAdvanced>,
    },
    #[error("Zone operation-output count mismatch: expected {expected}, got {actual}")]
    OperationCount { expected: usize, actual: usize },
    #[error(
        "Zone operation output mismatch at index {index}: expected {expected:?}, got {actual:?}"
    )]
    Operation {
        index: usize,
        expected: Box<ExpectedZoneOperation>,
        actual: Box<ObservedZoneOperation>,
    },
    #[error("BatchFinalized mismatch: expected {expected:?}, got {actual:?}")]
    BatchFinalized {
        expected: Option<ExpectedBatchFinalized>,
        actual: Option<Box<ObservedBatchFinalized>>,
    },
}

impl From<ZoneOutputFinding> for Finding {
    fn from(finding: ZoneOutputFinding) -> Self {
        Self::ZoneOutput(Box::new(finding))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TempoBlockFinalizedExpectation {
    pub(in crate::check) block_hash: B256,
    pub(in crate::check) block_number: u64,
    pub(in crate::check) state_root: B256,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TempoAdvancedExpectation {
    pub(in crate::check) block_hash: B256,
    pub(in crate::check) block_number: u64,
    pub(in crate::check) deposits_processed: U256,
    pub(in crate::check) processed_deposit_hash: B256,
    pub(in crate::check) processed_deposit_number: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum FixedStateFinding {
    #[error("TempoState hash mismatch: expected {expected}, got {actual}")]
    TempoBlockHash { expected: B256, actual: B256 },
    #[error("TempoState number mismatch: expected {expected}, got {actual}")]
    TempoBlockNumber { expected: u64, actual: u64 },
    #[error("Inbox processed-deposit hash mismatch: expected {expected}, got {actual}")]
    ProcessedDepositHash { expected: B256, actual: B256 },
    #[error("Inbox processed-deposit number mismatch: expected {expected}, got {actual}")]
    ProcessedDepositNumber { expected: u64, actual: u64 },
    #[error("Outbox last-batch queue hash mismatch: expected {expected}, got {actual}")]
    WithdrawalQueueHash { expected: B256, actual: B256 },
    #[error("Outbox last-batch index mismatch: expected {expected}, got {actual}")]
    WithdrawalBatchIndex { expected: u64, actual: u64 },
}
