extern crate alloc;

use std::sync::Arc;

use alloy_primitives::{Address, B256, U256};
use reth_codecs::Compact;
use reth_db::{
    DatabaseEnv, DatabaseError,
    database::Database,
    table::{Decode, Encode},
    transaction::DbTx,
};
use serde::{Deserialize, Serialize};

use super::WithdrawalCheckError;

const CODEC_VERSION: u64 = 1;
pub(super) const CHECKPOINT_KEY: u8 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[doc(hidden)]
#[expect(unreachable_pub, reason = "custom Reth table key type")]
pub struct BalanceKey {
    pub(super) user: Address,
    pub(super) token: Address,
}

impl BalanceKey {
    const ENCODED_LEN: usize = 40;

    pub(super) const fn new(user: Address, token: Address) -> Self {
        Self { user, token }
    }
}

impl Encode for BalanceKey {
    type Encoded = [u8; Self::ENCODED_LEN];

    fn encode(self) -> Self::Encoded {
        let mut encoded = [0u8; Self::ENCODED_LEN];
        encoded[..20].copy_from_slice(self.user.as_slice());
        encoded[20..].copy_from_slice(self.token.as_slice());
        encoded
    }
}

impl Decode for BalanceKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        if value.len() != Self::ENCODED_LEN {
            return Err(DatabaseError::Decode);
        }
        Ok(Self {
            user: Address::from_slice(&value[..20]),
            token: Address::from_slice(&value[20..]),
        })
    }
}

mod tables {
    use reth_db::{TableSet, TableViewer, table::TableInfo, tables, tables::TableType};
    use std::fmt;

    tables! {
        /// Available withdrawal backing keyed by the 40-byte `user || token` key.
        table WithdrawalBacking {
            type Key = super::BalanceKey;
            type Value = reth_db::models::CompactU256;
        }

        /// Sum of available withdrawal backing for every user, keyed by L1 token.
        table WithdrawalTokenBacking {
            type Key = alloy_primitives::Address;
            type Value = reth_db::models::CompactU256;
        }

        /// Singleton state for the withdrawal checker.
        table WithdrawalCheckerState {
            type Key = u8;
            type Value = super::Checkpoint;
        }

        /// Reversible balance changes retained for the latest canonical reorg window.
        table WithdrawalBlockDeltas {
            type Key = u64;
            type Value = super::BlockDelta;
        }
    }
}

pub(crate) use tables::Tables as WithdrawalCheckerTables;
pub(super) use tables::{
    WithdrawalBacking, WithdrawalBlockDeltas, WithdrawalCheckerState, WithdrawalTokenBacking,
};

/// Proof that the custom withdrawal-checker tables were registered before node construction.
#[derive(Debug)]
pub struct RegisteredWithdrawalCheckerTables {
    pub(crate) tables_are_new: bool,
}

/// Register the custom withdrawal-checker tables before passing the database to Reth.
pub fn register_withdrawal_checker_tables(
    database: DatabaseEnv,
) -> eyre::Result<RegisteredWithdrawalCheckerTables> {
    Ok(RegisteredWithdrawalCheckerTables {
        tables_are_new: register_tables(database)?,
    })
}

/// Register the ledger schema before Reth shares the MDBX environment.
///
/// Returns `true` when none of the tables existed. The first launch may bootstrap that fresh
/// schema at genesis; subsequent launches require every table and fail closed on partial state.
pub(super) fn register_tables(database: DatabaseEnv) -> Result<bool, WithdrawalCheckError> {
    let tx = database.tx()?;
    let backing_exists = table_exists(tx.entries::<WithdrawalBacking>())?;
    let token_backing_exists = table_exists(tx.entries::<WithdrawalTokenBacking>())?;
    let state_exists = table_exists(tx.entries::<WithdrawalCheckerState>())?;
    let deltas_exist = table_exists(tx.entries::<WithdrawalBlockDeltas>())?;
    drop(tx);

    let tables_are_new = match (
        backing_exists,
        token_backing_exists,
        state_exists,
        deltas_exist,
    ) {
        (false, false, false, false) => true,
        (true, true, true, true) => false,
        _ => {
            return Err(WithdrawalCheckError::InvalidState(
                "withdrawal check table schema is only partially present".to_string(),
            ));
        }
    };
    let mut database = Arc::new(database);
    database.create_tables_for::<WithdrawalCheckerTables>()?;
    Ok(tables_are_new)
}

fn table_exists(result: Result<usize, DatabaseError>) -> Result<bool, WithdrawalCheckError> {
    match result {
        Ok(_) => Ok(true),
        Err(DatabaseError::Open(_)) => Ok(false),
        Err(err) => Err(err.into()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Compact)]
#[doc(hidden)]
#[expect(unreachable_pub, reason = "custom Reth table value type")]
pub struct Checkpoint {
    pub(super) codec_version: u64,
    pub(super) zone: u64,
    pub(super) number: u64,
    pub(super) hash: B256,
}

impl Checkpoint {
    pub(super) const fn new(zone: u32, number: u64, hash: B256) -> Self {
        Self {
            codec_version: CODEC_VERSION,
            zone: zone as u64,
            number,
            hash,
        }
    }

    pub(super) fn into_validated(self) -> Result<Self, WithdrawalCheckError> {
        if self.codec_version != CODEC_VERSION {
            return Err(WithdrawalCheckError::InvalidState(format!(
                "unsupported checkpoint codec version {}",
                self.codec_version
            )));
        }
        u32::try_from(self.zone).map_err(|_| {
            WithdrawalCheckError::InvalidState(format!("checkpoint zone {} exceeds u32", self.zone))
        })?;
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Compact)]
pub(super) struct PreviousBacking {
    user: Address,
    token: Address,
    pub(super) value: Option<U256>,
}

impl PreviousBacking {
    pub(super) const fn new(key: BalanceKey, value: Option<U256>) -> Self {
        Self {
            user: key.user,
            token: key.token,
            value,
        }
    }

    pub(super) const fn key(self) -> BalanceKey {
        BalanceKey::new(self.user, self.token)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Compact)]
#[doc(hidden)]
#[expect(unreachable_pub, reason = "custom Reth table value type")]
pub struct BlockDelta {
    pub(super) codec_version: u64,
    pub(super) hash: B256,
    pub(super) parent_hash: B256,
    pub(super) previous: Vec<PreviousBacking>,
}

impl BlockDelta {
    pub(super) fn new(
        hash: B256,
        parent_hash: B256,
        previous: Vec<PreviousBacking>,
    ) -> Result<Self, WithdrawalCheckError> {
        u32::try_from(previous.len()).map_err(|_| {
            WithdrawalCheckError::InvalidState("too many balance deltas in one block".to_string())
        })?;
        Ok(Self {
            codec_version: CODEC_VERSION,
            hash,
            parent_hash,
            previous,
        })
    }

    pub(super) fn into_validated(self) -> Result<Self, WithdrawalCheckError> {
        if self.codec_version != CODEC_VERSION {
            return Err(WithdrawalCheckError::InvalidState(format!(
                "unsupported block delta codec version {}",
                self.codec_version
            )));
        }
        u32::try_from(self.previous.len()).map_err(|_| {
            WithdrawalCheckError::InvalidState(
                "too many previous balances in block delta".to_string(),
            )
        })?;
        if self.previous.iter().any(|entry| {
            entry
                .value
                .is_some_and(|previous_balance| previous_balance.is_zero())
        }) {
            return Err(WithdrawalCheckError::InvalidState(
                "block delta contains a persisted zero balance".to_string(),
            ));
        }
        if self
            .previous
            .windows(2)
            .any(|pair| pair[0].key() >= pair[1].key())
        {
            return Err(WithdrawalCheckError::InvalidState(
                "block delta balance keys are not strictly ordered".to_string(),
            ));
        }
        Ok(self)
    }
}

reth_codecs::impl_compression_for_compact!(Checkpoint, BlockDelta);
