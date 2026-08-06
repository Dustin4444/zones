//! Durable sole-writer orchestration for live checker notifications.

mod apply;
mod error;
mod exex;

#[cfg(test)]
pub(crate) use apply::L1Client;
pub(crate) use apply::LiveChecker;
pub(crate) use error::{RuntimeError, RuntimeResult};
#[cfg(test)]
pub(crate) use exex::{RuntimeStatus, process_retained_notification, validate_local_canonical_tip};
