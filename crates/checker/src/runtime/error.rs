//! Failures at the persistent checker orchestration boundary.

use alloy_eips::BlockNumHash;
use alloy_primitives::B256;
use reth_storage_api::errors::provider::ProviderError;

use crate::{check::finding::CheckError, observe::AcquisitionError, store::error::StoreError};

pub(crate) type RuntimeResult<T> = Result<T, RuntimeError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum RuntimeError {
    #[error(transparent)]
    Check(#[from] CheckError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("persistent checker does not accept {0} notifications")]
    UnsupportedNotification(&'static str),
    #[error("committed ExEx notification contains no blocks")]
    EmptyCommittedChain,
    #[error("failed to read local canonical hash for durable checker tip {tip:?}")]
    LocalCanonicalRead {
        tip: BlockNumHash,
        #[source]
        source: ProviderError,
    },
    #[error("local canonical chain is missing durable checker tip {0:?}")]
    MissingLocalCanonical(BlockNumHash),
    #[error("local canonical hash at durable checker height {tip:?} differs: found {actual}")]
    LocalCanonicalConflict { tip: BlockNumHash, actual: B256 },
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
