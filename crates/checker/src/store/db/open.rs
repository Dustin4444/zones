//! Version-gated checker database opening and fresh-path initialization.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use reth_db::{
    Database, DatabaseEnv, DatabaseEnvKind, is_database_empty,
    mdbx::{DatabaseArguments, init_db_for},
    open_db_read_only,
    transaction::DbTx,
};

use super::{
    CheckerStore, Initialization, StoreSnapshot, all_tables_empty, finish_read, initialize_empty,
    prepare_initial_model, read_stored_identity, validate_schema_version,
};
use crate::store::{
    error::{StoreError, StoreResult},
    schema::{CheckerMeta, CheckerTables},
    value::StoreIdentity,
};

const CHECKER_DIRECTORY: &str = "checker";

impl CheckerStore {
    /// Canonical checker path inside one node data directory.
    pub(crate) fn path_in(data_dir: impl AsRef<Path>) -> PathBuf {
        data_dir.as_ref().join(CHECKER_DIRECTORY)
    }

    /// Open the canonical checker path, creating only a wholly empty schema.
    ///
    /// Existing state is inspected read-only before any read/write open, so an
    /// incompatible version cannot have tables created or rewritten by startup.
    #[cfg(test)]
    pub(crate) fn open(
        data_dir: impl AsRef<Path>,
        initialization: Initialization,
    ) -> StoreResult<Self> {
        let path = Self::path_in(data_dir);
        match Self::inspect_existing_at(&path, initialization.identity) {
            Ok(_) => open_read_write_at(path, initialization.identity).map(|(store, _)| store),
            Err(StoreError::EmptyExistingDatabase { .. }) => {
                Self::create_fresh_at(path, initialization)
            }
            Err(error) => Err(error),
        }
    }

    /// Create one checker environment at an explicit fresh path.
    #[cfg(test)]
    pub(crate) fn create_fresh_at(
        path: impl AsRef<Path>,
        initialization: Initialization,
    ) -> StoreResult<Self> {
        Self::create_fresh_with_snapshot_at(path, initialization).map(|(store, _)| store)
    }

    /// Create and validate one checker environment, returning the exact
    /// snapshot read after its initialization transaction commits.
    pub(crate) fn create_fresh_with_snapshot_at(
        path: impl AsRef<Path>,
        initialization: Initialization,
    ) -> StoreResult<(Self, StoreSnapshot)> {
        let path = path.as_ref().to_path_buf();
        if !fresh_path_is_empty(&path)? {
            return Err(StoreError::NonEmptyFreshDatabase { path });
        }
        // Validate the complete logical genesis before `init_db_for` can create
        // or modify anything at the operator-selected rebuild path.
        let rows = prepare_initial_model(&initialization)?;
        let db = init_db_for::<_, CheckerTables>(&path, DatabaseArguments::default()).map_err(
            |source| StoreError::Open {
                path: path.clone(),
                source,
            },
        )?;
        let store = Self {
            db: Arc::new(db),
            identity: initialization.identity,
            path,
        };

        let tx = store.db.tx_mut()?;
        let is_empty = match all_tables_empty(&tx) {
            Ok(is_empty) => is_empty,
            Err(error) => {
                tx.abort();
                return Err(error);
            }
        };
        if !is_empty {
            tx.abort();
            return Err(StoreError::NonEmptyFreshDatabase { path: store.path });
        }
        let result = initialize_empty(&tx, &initialization, rows);
        match result {
            Ok(()) => tx.commit()?,
            Err(error) => {
                tx.abort();
                return Err(error);
            }
        }
        let snapshot = store.load_validated_snapshot()?;
        Ok((store, snapshot))
    }

    /// Open the canonical initialized checker database without creating state.
    #[cfg(test)]
    pub(crate) fn open_existing(
        data_dir: impl AsRef<Path>,
        identity: StoreIdentity,
    ) -> StoreResult<Self> {
        Self::open_existing_at(Self::path_in(data_dir), identity)
    }

    /// Read the durable database identity without requiring archive-derived state.
    ///
    /// The stable version row is decoded before any versioned metadata value.
    pub(crate) fn inspect_identity_at(path: impl AsRef<Path>) -> StoreResult<StoreIdentity> {
        let path = path.as_ref();
        let db = open_compatible_read_only_at(path)?;
        let tx = db.tx()?;
        let result = read_stored_identity(&tx);
        finish_read(tx, result)
    }

    /// Inspect one initialized checker database read-only and return its exact cut.
    pub(crate) fn inspect_existing_at(
        path: impl AsRef<Path>,
        identity: StoreIdentity,
    ) -> StoreResult<StoreSnapshot> {
        let store = open_read_only_at(path.as_ref(), identity)?;
        store.load_validated_snapshot()
    }

    /// Open an initialized checker database at an explicit path.
    #[cfg(test)]
    pub(crate) fn open_existing_at(
        path: impl AsRef<Path>,
        identity: StoreIdentity,
    ) -> StoreResult<Self> {
        Self::open_existing_with_snapshot_at(path, identity).map(|(store, _)| store)
    }

    /// Open and validate an initialized checker database, returning the exact
    /// snapshot read from the final read/write environment.
    pub(crate) fn open_existing_with_snapshot_at(
        path: impl AsRef<Path>,
        identity: StoreIdentity,
    ) -> StoreResult<(Self, StoreSnapshot)> {
        let path = path.as_ref().to_path_buf();
        Self::inspect_existing_at(&path, identity)?;
        open_read_write_at(path, identity)
    }
}

fn open_read_only_at(path: &Path, identity: StoreIdentity) -> StoreResult<CheckerStore> {
    let db = open_compatible_read_only_at(path)?;
    Ok(CheckerStore {
        db: Arc::new(db),
        identity,
        path: path.to_path_buf(),
    })
}

fn open_compatible_read_only_at(path: &Path) -> StoreResult<DatabaseEnv> {
    if is_database_empty(path) {
        return Err(StoreError::EmptyExistingDatabase {
            path: path.to_path_buf(),
        });
    }
    let db = open_db_read_only(path, DatabaseArguments::default()).map_err(|source| {
        StoreError::Open {
            path: path.to_path_buf(),
            source,
        }
    })?;
    let tx = db.tx()?;
    let result = (|| {
        if tx.entries::<CheckerMeta>()? == 0 && all_tables_empty(&tx)? {
            return Err(StoreError::EmptyExistingDatabase {
                path: path.to_path_buf(),
            });
        }
        validate_schema_version(&tx, path)
    })();
    finish_read(tx, result)?;
    Ok(db)
}

fn open_read_write_at(
    path: PathBuf,
    identity: StoreIdentity,
) -> StoreResult<(CheckerStore, StoreSnapshot)> {
    let db = DatabaseEnv::open(&path, DatabaseEnvKind::RW, DatabaseArguments::default()).map_err(
        |source| StoreError::Open {
            path: path.clone(),
            source: source.into(),
        },
    )?;
    let store = CheckerStore {
        db: Arc::new(db),
        identity,
        path,
    };
    // Validate after reopening read/write so a replaced environment can never
    // bypass the read-only probe.
    let snapshot = store.load_validated_snapshot()?;
    Ok((store, snapshot))
}

fn fresh_path_is_empty(path: &Path) -> StoreResult<bool> {
    if is_database_empty(path) {
        return Ok(true);
    }
    let probe = open_db_read_only(path, DatabaseArguments::default()).map_err(|source| {
        StoreError::Open {
            path: path.to_path_buf(),
            source,
        }
    })?;
    let tx = probe.tx()?;
    let empty = all_tables_empty(&tx);
    finish_read(tx, empty)
}
