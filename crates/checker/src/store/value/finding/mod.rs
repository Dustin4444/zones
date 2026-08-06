//! Compact, persistence-safe checker findings.
//!
//! The table key owns the candidate Zone height and hash. Runtime errors are
//! deliberately not serialized: they contain process-width indices and
//! disposable dynamic evidence that do not belong in the checker database.

mod codec;
pub(crate) mod leaf;
mod projection;
pub(crate) mod types;

#[cfg(test)]
pub use leaf::{
    StoredDataSource, StoredEnvelopeRule, StoredImportedProjectionError, StoredModelError,
    StoredPortalCallError, StoredProtocolChain, StoredZoneProjectionError,
};
#[cfg(test)]
pub use types::{ChainLocation, FindingKind, FindingSummary};
pub use types::{FindingRecord, FindingStatus};

#[cfg(test)]
mod tests;
