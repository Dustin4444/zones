//! Durable storage for one in-flight atomic settlement transaction.
//!
//! The settlement nonce lane is serialized, so at most one combined transaction can be
//! unresolved at a time. Persisting the exact signed EIP-2718 envelope before broadcast lets a
//! restarted sequencer rebroadcast or poll that transaction without reconstructing mutable
//! certificate state.

use std::{
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

use alloy_consensus::transaction::TxHashRef as _;
use alloy_eips::eip2718::{Decodable2718, Encodable2718};
use alloy_primitives::B256;
use eyre::{Result, WrapErr as _};
use tempo_primitives::TempoTxEnvelope;

/// Filesystem-backed singleton containing the exact signed combined transaction envelope.
#[derive(Debug, Clone)]
pub(crate) struct PendingCombinedSubmissionStore {
    path: PathBuf,
    durable_root: PathBuf,
}

impl PendingCombinedSubmissionStore {
    pub(crate) fn new(path: PathBuf, durable_root: PathBuf) -> Result<Self> {
        eyre::ensure!(
            durable_root.is_dir(),
            "pending transaction durable root {} does not exist or is not a directory",
            durable_root.display()
        );
        eyre::ensure!(
            path.starts_with(&durable_root) && path != durable_root,
            "pending transaction path {} must be beneath durable root {}",
            path.display(),
            durable_root.display()
        );
        Ok(Self { path, durable_root })
    }

    pub(crate) fn exists(&self) -> Result<bool> {
        self.path
            .try_exists()
            .wrap_err_with(|| format!("failed checking {}", self.path.display()))
    }

    pub(crate) fn load(&self) -> Result<Option<TempoTxEnvelope>> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .wrap_err_with(|| format!("failed reading {}", self.path.display()));
            }
        };
        let envelope =
            TempoTxEnvelope::decode_2718_exact(bytes.as_slice()).wrap_err_with(|| {
                format!(
                    "failed decoding pending combined transaction from {}",
                    self.path.display()
                )
            })?;
        Ok(Some(envelope))
    }

    /// Atomically persist and fsync an envelope before it can be broadcast.
    ///
    /// An existing identical envelope is idempotent. A different envelope is rejected so mutable
    /// retry inputs can never overwrite the transaction already assigned to the committed nonce.
    pub(crate) fn persist(&self, envelope: &TempoTxEnvelope) -> Result<()> {
        self.persist_with_hook(envelope, || Ok(()))
    }

    #[cfg(test)]
    pub(crate) fn persist_with_after_link_hook(
        &self,
        envelope: &TempoTxEnvelope,
        after_link: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        self.persist_with_hook(envelope, after_link)
    }

    fn persist_with_hook(
        &self,
        envelope: &TempoTxEnvelope,
        after_link: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        if let Some(existing) = self.load()? {
            eyre::ensure!(
                existing.tx_hash() == envelope.tx_hash(),
                "refusing to replace pending combined transaction {} with {}",
                existing.tx_hash(),
                envelope.tx_hash()
            );
            self.ensure_durable(*envelope.tx_hash())?;
            self.remove_temporary(envelope);
            return Ok(());
        }

        let parent = self.parent()?;
        fs::create_dir_all(parent).wrap_err_with(|| {
            format!(
                "failed creating pending combined transaction directory {}",
                parent.display()
            )
        })?;

        let temporary_path = self.temporary_path(envelope)?;
        let mut temporary = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary_path)
            .wrap_err_with(|| format!("failed opening {}", temporary_path.display()))?;
        temporary
            .write_all(&envelope.encoded_2718())
            .wrap_err_with(|| format!("failed writing {}", temporary_path.display()))?;
        temporary
            .sync_all()
            .wrap_err_with(|| format!("failed syncing {}", temporary_path.display()))?;
        drop(temporary);

        match fs::hard_link(&temporary_path, &self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                let existing = self.load()?.ok_or_else(|| {
                    eyre::eyre!(
                        "pending transaction appeared at {} but could not be loaded",
                        self.path.display()
                    )
                })?;
                eyre::ensure!(
                    existing.tx_hash() == envelope.tx_hash(),
                    "refusing to replace concurrently persisted combined transaction {} with {}",
                    existing.tx_hash(),
                    envelope.tx_hash()
                );
            }
            Err(error) => {
                return Err(error).wrap_err_with(|| {
                    format!(
                        "failed atomically installing pending combined transaction at {}",
                        self.path.display()
                    )
                });
            }
        }
        after_link()?;
        self.ensure_durable(*envelope.tx_hash())?;
        self.remove_temporary(envelope);
        Ok(())
    }

    /// Re-establish the full durability barrier before every broadcast or resume.
    pub(crate) fn ensure_durable(&self, expected_hash: B256) -> Result<()> {
        let existing = self.load()?.ok_or_else(|| {
            eyre::eyre!(
                "pending combined transaction {expected_hash} is missing from {}",
                self.path.display()
            )
        })?;
        eyre::ensure!(
            *existing.tx_hash() == expected_hash,
            "pending combined transaction {} does not match expected {expected_hash}",
            existing.tx_hash()
        );
        File::open(&self.path)
            .and_then(|file| file.sync_all())
            .wrap_err_with(|| format!("failed syncing {}", self.path.display()))?;
        Self::sync_directory_chain(self.parent()?, &self.durable_root)
    }

    /// Remove the stored envelope only if it is the expected transaction.
    pub(crate) fn clear(&self, expected_hash: B256) -> Result<()> {
        let Some(existing) = self.load()? else {
            return Ok(());
        };
        eyre::ensure!(
            *existing.tx_hash() == expected_hash,
            "refusing to clear pending combined transaction {} while resolving {expected_hash}",
            existing.tx_hash()
        );
        fs::remove_file(&self.path)
            .wrap_err_with(|| format!("failed removing {}", self.path.display()))?;
        Self::sync_directory(self.parent()?)?;
        Ok(())
    }

    fn parent(&self) -> Result<&Path> {
        self.path
            .parent()
            .ok_or_else(|| eyre::eyre!("pending transaction path has no parent"))
    }

    fn sync_directory(path: &Path) -> Result<()> {
        File::open(path)
            .and_then(|directory| directory.sync_all())
            .wrap_err_with(|| format!("failed syncing directory {}", path.display()))
    }

    /// Sync the journal directory and every newly created ancestor entry.
    fn sync_directory_chain(path: &Path, durable_ancestor: &Path) -> Result<()> {
        let mut current = path;
        loop {
            Self::sync_directory(current)?;
            if current == durable_ancestor {
                return Ok(());
            }
            current = current.parent().ok_or_else(|| {
                eyre::eyre!(
                    "directory {} is not beneath durable ancestor {}",
                    path.display(),
                    durable_ancestor.display()
                )
            })?;
        }
    }

    fn remove_temporary(&self, envelope: &TempoTxEnvelope) {
        let Ok(temporary_path) = self.temporary_path(envelope) else {
            return;
        };
        match fs::remove_file(&temporary_path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(_) => {
                // The canonical inode and its directory chain are already durable. An orphaned
                // hard-link name is harmless and will be retried on the next idempotent persist.
            }
        }
    }

    fn temporary_path(&self, envelope: &TempoTxEnvelope) -> Result<PathBuf> {
        let file_name = self
            .path
            .file_name()
            .ok_or_else(|| eyre::eyre!("pending transaction path has no file name"))?
            .to_string_lossy();
        Ok(self
            .path
            .with_file_name(format!(".{file_name}.{}.tmp", envelope.tx_hash())))
    }
}
