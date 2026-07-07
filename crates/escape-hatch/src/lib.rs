//! Escape hatch data structures and batch commitments.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![allow(dead_code)]

use alloy_primitives::B256;

pub mod note_tree;

pub use note_tree::{AppendRange, ExitNoteTree, ExitNoteTreeError, ExitNoteTreeProof};

/// Runtime configuration for escape-hatch processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EscapeHatchConfig {
    /// Whether the sequencer should compute and publish escape-hatch data.
    pub enabled: bool,
}

impl EscapeHatchConfig {
    /// Create a new escape-hatch config.
    pub const fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    /// Returns true when escape-hatch processing is disabled.
    pub const fn is_disabled(self) -> bool {
        !self.enabled
    }
}

/// Roots and metadata that will eventually be committed with each batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BatchExitCommitment {
    /// Root of the append-only exit-note tree after the batch.
    pub exit_note_root_after: B256,
    /// Root of the global sparse nullifier tree after the batch.
    pub exit_nullifier_root_after: B256,
    /// Commitment to the batch's published exit data blob.
    pub exit_data_root: B256,
    /// Exit-note tree epoch for this batch.
    pub epoch_id: u64,
}

/// Disabled escape-hatch engine used by default.
#[derive(Debug, Clone, Copy, Default)]
pub struct DisabledEscapeHatch;

impl DisabledEscapeHatch {
    /// Return the inert commitment used while escape hatch support is disabled.
    pub const fn batch_commitment(&self) -> BatchExitCommitment {
        BatchExitCommitment {
            exit_note_root_after: B256::ZERO,
            exit_nullifier_root_after: B256::ZERO,
            exit_data_root: B256::ZERO,
            epoch_id: 0,
        }
    }
}
