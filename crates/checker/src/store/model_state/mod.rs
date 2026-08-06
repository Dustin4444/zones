//! Narrow typed bridge from the pure model into the checker storage schema.

mod decode;
mod encode;
mod projection;
mod rows;
pub(crate) mod update;

use std::collections::BTreeMap;

use alloy_primitives::Address;

use crate::model::{
    ownership::{BatchStateError, PortalQueueIdError},
    state::PortalIdentity,
    validation::AuthoritativeStateError,
};
use crate::store::{schema::ModelKey, value::ModelValue};

#[cfg(test)]
use crate::model::state::ModelState;

pub(crate) use decode::assemble_model;
pub(crate) use encode::flatten_model;

pub(crate) type ModelRows = BTreeMap<ModelKey, ModelValue>;

fn cursor(hash: alloy_primitives::B256, number: u64) -> crate::store::value::CursorValue {
    crate::store::value::CursorValue { hash, number }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ModelPersistenceError {
    #[error("persisted model is missing required {0}")]
    Missing(&'static str),
    #[error("persisted model contains only part of {0}")]
    Partial(&'static str),
    #[error("persisted model key {key:?} has mismatched value {value:?}")]
    KeyValueMismatch {
        key: ModelKey,
        value: Box<ModelValue>,
    },
    #[error("persisted {kind} address {actual} disagrees with configured {expected}")]
    AddressIdentityMismatch {
        kind: &'static str,
        expected: Address,
        actual: Address,
    },
    #[error("persisted {kind} zone {actual} disagrees with configured {expected}")]
    ZoneIdentityMismatch {
        kind: &'static str,
        expected: u32,
        actual: u32,
    },
    #[error("persisted {kind} identity {actual:?} disagrees with configured {expected:?}")]
    PortalIdentityMismatch {
        kind: &'static str,
        expected: PortalIdentity,
        actual: PortalIdentity,
    },
    #[error("persisted {kind} identifier is zero")]
    ZeroIdentifier { kind: &'static str },
    #[error("persisted model contains duplicate {kind} identity")]
    Duplicate { kind: &'static str },
    #[error(transparent)]
    Withdrawal(#[from] crate::model::encoding::WithdrawalDataError),
    #[error(transparent)]
    Batch(#[from] BatchStateError),
    #[error(transparent)]
    PortalQueue(#[from] PortalQueueIdError),
    #[error(transparent)]
    Authoritative(#[from] AuthoritativeStateError),
}

#[cfg(test)]
pub(crate) fn model_bytes(rows: &ModelRows) -> Vec<(Vec<u8>, Vec<u8>)> {
    use reth_codecs::Compress;
    use reth_db_api::table::Encode;

    rows.iter()
        .map(|(key, value)| (key.encode(), value.clone().compress()))
        .collect()
}

#[cfg(test)]
pub(crate) fn validate_round_trip(state: &ModelState) -> Result<(), ModelPersistenceError> {
    let identity = state.portal().identity();
    let rows = flatten_model(state)?;
    let decoded = assemble_model(identity, rows)?;
    if &decoded == state {
        Ok(())
    } else {
        Err(ModelPersistenceError::Partial("model round trip"))
    }
}

#[cfg(test)]
mod tests;
