//! Checker failure policy and durable finding construction.

use crate::{
    kernel::{Datum, Finding, FindingCategory, FindingLocation},
    observe::{AcquisitionError, ObservationError},
};

/// Policy outcome for a failed observation or comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureClass {
    Terminal,
    Retry,
    Divergence,
}

/// A failure together with the durable action it requires.
#[derive(Debug, Clone)]
pub(crate) struct Failure {
    pub(crate) class: FailureClass,
    pub(crate) message: String,
    pub(crate) finding: Option<Box<Finding>>,
}

impl Failure {
    /// Construct a failure that stops verification at the current tip.
    pub(crate) fn terminal(message: impl Into<String>) -> Self {
        Self {
            class: FailureClass::Terminal,
            message: message.into(),
            finding: None,
        }
    }

    /// Construct a retryable acquisition failure.
    pub(crate) fn retry(message: impl Into<String>) -> Self {
        Self {
            class: FailureClass::Retry,
            message: message.into(),
            finding: None,
        }
    }

    /// Construct a failure that must be persisted as a finding.
    pub(crate) fn authenticated_divergence(message: impl Into<String>, finding: Finding) -> Self {
        Self {
            class: FailureClass::Divergence,
            message: message.into(),
            finding: Some(Box::new(finding)),
        }
    }
}

impl From<AcquisitionError> for Failure {
    fn from(error: AcquisitionError) -> Self {
        Self::retry(error.to_string())
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
