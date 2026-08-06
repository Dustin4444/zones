//! Failures at the persistent checker orchestration boundary.

use alloy_eips::BlockNumHash;
use reth_storage_api::errors::provider::ProviderError;

use crate::{check::finding::CheckError, observe::AcquisitionError, store::error::StoreError};

pub(crate) type RuntimeResult<T> = Result<T, RuntimeError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum RuntimeError {
    #[error(transparent)]
    Check(#[from] CheckError),
    #[error(transparent)]
    Store(#[from] StoreError),
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
}

impl RuntimeError {
    pub(super) const fn is_acquisition(&self) -> bool {
        matches!(self, Self::Check(CheckError::Acquisition(_)))
    }
}

impl From<AcquisitionError> for RuntimeError {
    fn from(error: AcquisitionError) -> Self {
        Self::Check(CheckError::Acquisition(error))
    }
}
