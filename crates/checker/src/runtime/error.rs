//! Failures at the persistent checker orchestration boundary.

use alloy_eips::BlockNumHash;
use reth_storage_api::errors::provider::ProviderError;

use crate::{
    check::finding::CheckError, observe::AcquisitionError,
    runtime::bootstrap::error::BootstrapError, store::error::StoreError,
};

pub(crate) type RuntimeResult<T> = Result<T, RuntimeError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum RuntimeError {
    #[error(transparent)]
    Check(#[from] CheckError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Bootstrap(Box<BootstrapError>),
    #[error("{0} ExEx notification contains no blocks")]
    EmptyNotificationChain(&'static str),
    #[error("invalid {kind} ExEx notification chain: {reason}")]
    InvalidNotificationChain {
        kind: &'static str,
        reason: &'static str,
    },
    #[error(
        "durable checker tip {tip:?} is on neither the reverted fragment nor its common ancestor"
    )]
    ReorgProgressConflict { tip: BlockNumHash },
    #[error("failed to read local canonical hash for durable checker tip {tip:?}")]
    LocalCanonicalRead {
        tip: BlockNumHash,
        #[source]
        source: ProviderError,
    },
    #[error("failed to read the local canonical Zone head")]
    LocalCanonicalHeadRead {
        #[source]
        source: ProviderError,
    },
    #[error("local canonical Zone head {head:?} is below active finding {finding:?}")]
    CanonicalHeadBehindAlert {
        head: BlockNumHash,
        finding: BlockNumHash,
    },
    #[error("checker Zone genesis {tip:?} is not the local canonical genesis")]
    NonCanonicalGenesis { tip: BlockNumHash },
    #[error(
        "checker notification stream closed before Zone archive replay completed from verified tip {verified:?}"
    )]
    BootstrapStreamClosed { verified: BlockNumHash },
    #[error("checker notification stream closed while L1 bootstrap was still running")]
    NotificationStreamClosedDuringBootstrap,
    #[error("checker notification stream failed while L1 bootstrap was running")]
    BootstrapNotificationStream {
        #[source]
        source: eyre::Report,
    },
}

impl RuntimeError {
    pub(super) fn is_retryable(&self) -> bool {
        match self {
            // The release-one contract treats every incomplete or internally
            // inconsistent acquired view as operational. Retaining and
            // reacquiring the exact candidate preserves the acknowledgement
            // gap without manufacturing a protocol finding.
            Self::Check(CheckError::Acquisition(_)) => true,
            Self::Bootstrap(error)
                if matches!(error.as_ref(), BootstrapError::LocalCanonicalRead { .. }) =>
            {
                true
            }
            Self::LocalCanonicalRead { .. } | Self::LocalCanonicalHeadRead { .. } => true,
            _ => false,
        }
    }
}

impl From<BootstrapError> for RuntimeError {
    fn from(error: BootstrapError) -> Self {
        Self::Bootstrap(Box::new(error))
    }
}

impl From<AcquisitionError> for RuntimeError {
    fn from(error: AcquisitionError) -> Self {
        Self::Check(CheckError::Acquisition(error))
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeError;
    use crate::{
        observe::{AcquisitionError, AcquisitionSource},
        runtime::bootstrap::error::BootstrapError,
    };

    #[test]
    fn every_acquisition_failure_is_retryable_but_configuration_is_not() {
        assert!(
            RuntimeError::from(AcquisitionError::unavailable(
                AcquisitionSource::L1Rpc,
                "offline",
            ))
            .is_retryable()
        );
        assert!(
            RuntimeError::from(AcquisitionError::missing(
                AcquisitionSource::L1Block,
                "0x01",
            ))
            .is_retryable()
        );
        for source in [
            AcquisitionSource::L1Block,
            AcquisitionSource::L1Receipts,
            AcquisitionSource::ZoneNotificationReceipts,
            AcquisitionSource::ZoneNotificationBlock,
        ] {
            assert!(
                RuntimeError::from(AcquisitionError::inconsistent(source, 1, 0)).is_retryable()
            );
        }
        assert!(!RuntimeError::from(BootstrapError::MissingCreationBlockHash).is_retryable());
    }
}
