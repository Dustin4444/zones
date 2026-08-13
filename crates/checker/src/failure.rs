//! Checker failure policy and durable finding construction.

use crate::{
    kernel::{Datum, Finding, FindingCategory, FindingLocation},
    observe::{AcquisitionError, ObservationError},
    persistence::CoverageGapReason,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Policy outcome for a failed observation or comparison.
pub(crate) enum FailureClass {
    ImmediateTerminal,
    BoundedRetry,
    TransientRetry,
    AuthenticatedDivergence,
}

#[derive(Debug, Clone)]
/// A failure together with the durable action it requires.
pub(crate) struct Failure {
    pub(crate) class: FailureClass,
    gap_reason: Option<CoverageGapReason>,
    pub(crate) message: String,
    pub(crate) finding: Option<Box<Finding>>,
}

impl Failure {
    /// Construct a failure that stops checking without acknowledgement.
    pub(crate) fn terminal(message: impl Into<String>) -> Self {
        Self {
            class: FailureClass::ImmediateTerminal,
            gap_reason: None,
            message: message.into(),
            finding: None,
        }
    }

    /// Construct a retryable provider failure.
    pub(crate) fn transient(message: impl Into<String>) -> Self {
        Self {
            class: FailureClass::TransientRetry,
            gap_reason: Some(CoverageGapReason::ProviderUnavailable),
            message: message.into(),
            finding: None,
        }
    }

    /// Construct a retryable failure for an absent Zone receipt set.
    pub(crate) fn missing_receipts(message: impl Into<String>) -> Self {
        Self {
            class: FailureClass::BoundedRetry,
            gap_reason: Some(CoverageGapReason::MissingReceipts),
            message: message.into(),
            finding: None,
        }
    }

    fn incomplete(message: impl Into<String>) -> Self {
        Self {
            class: FailureClass::BoundedRetry,
            gap_reason: Some(CoverageGapReason::MissingTempoData),
            message: message.into(),
            finding: None,
        }
    }

    /// Construct a failure that must be persisted as a finding.
    pub(crate) fn authenticated_divergence(message: impl Into<String>, finding: Finding) -> Self {
        Self {
            class: FailureClass::AuthenticatedDivergence,
            gap_reason: Some(CoverageGapReason::NotCheckedAncestorDivergence),
            message: message.into(),
            finding: Some(Box::new(finding)),
        }
    }

    /// Return the durable coverage reason for a retryable failure.
    pub(crate) fn gap_reason(&self) -> CoverageGapReason {
        self.gap_reason
            .clone()
            .expect("terminal failures do not create coverage gaps")
    }
}

impl From<AcquisitionError> for Failure {
    fn from(error: AcquisitionError) -> Self {
        let message = error.to_string();
        match error {
            AcquisitionError::Unavailable { .. } => Self::transient(message),
            AcquisitionError::Missing { .. } | AcquisitionError::Inconsistent { .. } => {
                Self::incomplete(message)
            }
        }
    }
}

impl From<ObservationError> for Failure {
    fn from(error: ObservationError) -> Self {
        let message = error.to_string();
        match error {
            ObservationError::Acquisition(error) => error.into(),
            ObservationError::MalformedAuthenticatedData {
                transaction,
                evidence,
                ..
            } => Self::authenticated_divergence(
                message,
                Finding::new(
                    FindingCategory::Observation,
                    110,
                    Some(FindingLocation::Operation(
                        transaction.transaction_index() as u32
                    )),
                    None,
                    Some(Datum::Bytes {
                        length: evidence.length(),
                        digest: evidence.digest(),
                    }),
                ),
            ),
            ObservationError::InvalidEnvelope { .. } => Self::authenticated_divergence(
                message,
                Finding::coded(FindingCategory::Observation, 120, FindingLocation::Block),
            ),
            ObservationError::ProtocolEvent {
                transaction_index, ..
            } => Self::authenticated_divergence(
                message,
                Finding::coded(
                    FindingCategory::Observation,
                    130,
                    FindingLocation::Operation(transaction_index as u32),
                ),
            ),
            ObservationError::PortalCall(_) => Self::authenticated_divergence(
                message,
                Finding::coded(FindingCategory::Observation, 140, FindingLocation::Block),
            ),
        }
    }
}

/// Build an authenticated divergence from structured evidence.
pub(crate) fn divergence(
    category: FindingCategory,
    code: u16,
    location: Option<FindingLocation>,
    expected: Option<Datum>,
    actual: Option<Datum>,
    message: impl Into<String>,
) -> Failure {
    Failure::authenticated_divergence(
        message,
        Finding::new(category, code, location, expected, actual),
    )
}
