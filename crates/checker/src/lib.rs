//! Durable observe-only checker for one Tempo Zone.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

mod check;
pub mod diagnostic;
mod metrics;
mod model;
mod observe;
pub(crate) mod persistence;
mod runtime;
mod store;

#[cfg(any(test, feature = "test-utils"))]
#[doc(hidden)]
pub mod test_utils;

use std::{fmt, future::Future, path::PathBuf, str::FromStr};

use alloy_primitives::{Address, B256};
use reth_exex::ExExContext;
use reth_node_api::{FullNodeComponents, NodeTypes};
use reth_storage_api::{BlockReader, StateProviderFactory};
use tempo_primitives::{Block, TempoPrimitives};

use observe::{AcquisitionError, AcquisitionSource};
#[cfg(feature = "test-utils")]
use test_utils::CheckerTestHooks;

/// Runtime mode for the checker ExEx.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CheckerMode {
    /// Checker is not installed.
    #[default]
    Off,
    /// Checker authenticates observations and persists its shadow model and
    /// findings without affecting block execution.
    Observe,
}

impl fmt::Display for CheckerMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Off => "off",
            Self::Observe => "observe",
        })
    }
}

impl FromStr for CheckerMode {
    type Err = eyre::Report;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "observe" => Ok(Self::Observe),
            other => Err(eyre::eyre!(
                "unsupported checker mode `{other}`, expected `off` or `observe`"
            )),
        }
    }
}

impl CheckerMode {
    /// Parse a mode without coupling this crate to clap.
    pub fn parse(value: &str) -> Result<Self, eyre::Report> {
        value.parse()
    }
}

/// Complete configuration for one checker database and Portal identity.
#[derive(Debug, Clone)]
pub struct CheckerConfig {
    /// Archive-capable Tempo RPC used for exact L1 bootstrap and live checks.
    pub l1_rpc_url: String,
    /// ZonePortal whose authenticated lifecycle this checker models.
    pub portal_address: Address,
    /// Exact Tempo block containing the ZoneFactory creation event.
    pub portal_creation_block_hash: B256,
    /// ZoneFactory Zone ID bound to the local Zone chain ID.
    pub zone_id: u32,
    /// Optional exact checker database path for fresh-path rebuilds.
    pub database_path: Option<PathBuf>,
}

/// Durable checker ExEx configuration.
pub struct CheckerExEx {
    config: CheckerConfig,
    #[cfg(feature = "test-utils")]
    test_hooks: CheckerTestHooks,
}

impl CheckerExEx {
    pub const fn new(config: CheckerConfig) -> Self {
        Self {
            config,
            #[cfg(feature = "test-utils")]
            test_hooks: CheckerTestHooks::disabled(),
        }
    }

    /// Run deterministic local preflight in Reth's outer initializer, then
    /// return the non-resolving durable checker worker.
    pub fn launch<Node>(
        self,
        ctx: ExExContext<Node>,
    ) -> eyre::Result<impl Future<Output = eyre::Result<()>> + Send>
    where
        Node: FullNodeComponents,
        Node::Provider: BlockReader<Block = Block> + StateProviderFactory,
        Node::Types: NodeTypes<Primitives = TempoPrimitives>,
    {
        runtime::launch(
            self.config,
            #[cfg(feature = "test-utils")]
            self.test_hooks,
            ctx,
        )
    }
}

pub(crate) fn validate_notification_receipt_sets(
    block_count: usize,
    receipt_set_count: usize,
) -> Result<(), AcquisitionError> {
    if block_count != receipt_set_count {
        return Err(AcquisitionError::inconsistent(
            AcquisitionSource::ZoneNotificationReceipts,
            format_args!("{block_count} block receipt sets"),
            format_args!("{receipt_set_count} block receipt sets"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
