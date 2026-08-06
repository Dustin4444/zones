use std::collections::BTreeMap;

use crate::store::{schema::ModelKey, value::ModelValue};

use super::{
    ModelPersistenceError, ModelRows, projection,
    rows::{portal_rows, zone_rows},
};
use crate::model::state::ModelState;

pub(crate) fn flatten_model(state: &ModelState) -> Result<ModelRows, ModelPersistenceError> {
    crate::model::validation::validate_authoritative(state)?;
    let identity = state.portal().identity();
    let mut rows = BTreeMap::new();

    for (key, value) in portal_rows(state.portal()) {
        if let Some(value) = value {
            put(&mut rows, key, value)?;
        }
    }
    for (key, value) in zone_rows(state.zone()) {
        put(&mut rows, key, value)?;
    }

    for (token, value) in state.tokens() {
        put_projected(&mut rows, projection::token(*token, value))?;
    }

    for (id, owner) in state.pending_deposits() {
        put_projected(
            &mut rows,
            projection::pending_deposit(identity, *id, Some(owner))?,
        )?;
    }
    for (id, owner) in state.withdrawals() {
        put_projected(
            &mut rows,
            projection::withdrawal(identity, *id, Some(owner))?,
        )?;
    }
    for (id, owner) in state.fallback_owners() {
        put_projected(
            &mut rows,
            projection::fallback_owner(identity, *id, Some(owner))?,
        )?;
    }
    for (id, owner) in state.batches() {
        put_projected(&mut rows, projection::batch(identity, *id, Some(owner))?)?;
    }
    for (id, owner) in state.portal_refunds() {
        put_projected(
            &mut rows,
            projection::portal_refund(identity, *id, Some(owner))?,
        )?;
    }
    for (id, owner) in state.inbox_refunds() {
        put_projected(
            &mut rows,
            projection::inbox_refund(identity, *id, Some(owner))?,
        )?;
    }
    Ok(rows)
}

fn put_projected(
    rows: &mut ModelRows,
    (key, value): projection::ProjectedRow,
) -> Result<(), ModelPersistenceError> {
    let Some(value) = value else {
        return Err(ModelPersistenceError::Missing(
            "value for present model owner",
        ));
    };
    put(rows, key, value)
}

fn put(
    rows: &mut ModelRows,
    key: ModelKey,
    value: ModelValue,
) -> Result<(), ModelPersistenceError> {
    if rows.insert(key, value).is_some() {
        Err(ModelPersistenceError::Duplicate { kind: "model key" })
    } else {
        Ok(())
    }
}
