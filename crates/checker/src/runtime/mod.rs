//! Durable sole-writer orchestration for live checker notifications.

mod alert;
mod apply;
pub(crate) mod bootstrap;
mod chain;
mod error;
mod exex;
mod reorg;
mod state;

#[cfg(test)]
pub(crate) use apply::L1Client;
pub(crate) use error::{RuntimeError, RuntimeResult};
pub(crate) use exex::launch;
#[cfg(test)]
pub(crate) use exex::{RuntimeStatus, process_retained_notification};
pub(crate) use state::PersistentChecker;
