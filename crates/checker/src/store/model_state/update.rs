use std::collections::BTreeMap;

use crate::store::{schema::ModelKey, value::ModelValue};

use super::{
    ModelPersistenceError, projection,
    rows::{portal_rows, zone_rows},
};
use crate::model::{
    state::PortalIdentity,
    transition::{ImportedTempoStateUpdate, LogicalMutationRef, ModelStateUpdate},
};

pub(crate) type ModelRowChanges = BTreeMap<ModelKey, Option<ModelValue>>;

/// Lower a typed logical update into the exact physical key families. Coarse
/// Portal and Zone replacements become independent rows so unchanged physical
/// values can be discarded by the store before capturing before-images.
pub(crate) fn lower_update(
    identity: PortalIdentity,
    update: &ModelStateUpdate,
) -> Result<ModelRowChanges, ModelPersistenceError> {
    let mut changes = BTreeMap::new();
    update.try_visit_mutations(|mutation| lower_one(identity, &mut changes, mutation))?;
    Ok(changes)
}

/// Lower the phase-specific Portal cut used by pre-genesis L1 bootstrap.
pub(crate) fn lower_imported_update(
    identity: PortalIdentity,
    update: &ImportedTempoStateUpdate,
) -> Result<ModelRowChanges, ModelPersistenceError> {
    let mut changes = BTreeMap::new();
    update.try_visit_mutations(|mutation| lower_one(identity, &mut changes, mutation))?;
    Ok(changes)
}

fn lower_one(
    identity: PortalIdentity,
    changes: &mut ModelRowChanges,
    mutation: LogicalMutationRef<'_>,
) -> Result<(), ModelPersistenceError> {
    match mutation {
        LogicalMutationRef::Portal(portal) => {
            require_portal_identity(identity, portal.identity(), "Portal update")?;
            for (key, value) in portal_rows(portal) {
                set(changes, key, value)?;
            }
        }
        LogicalMutationRef::Zone(zone) => {
            for (key, value) in zone_rows(zone) {
                set(changes, key, Some(value))?;
            }
        }
        LogicalMutationRef::Token(token, value) => {
            set_projected(changes, projection::token(token, value))?;
        }
        LogicalMutationRef::PendingDeposit(id, value) => {
            set_projected(changes, projection::pending_deposit(identity, id, value)?)?;
        }
        LogicalMutationRef::Withdrawal(id, value) => {
            set_projected(changes, projection::withdrawal(identity, id, value)?)?;
        }
        LogicalMutationRef::Batch(id, value) => {
            set_projected(changes, projection::batch(identity, id, value)?)?;
        }
        LogicalMutationRef::FallbackOwner(id, value) => {
            set_projected(changes, projection::fallback_owner(identity, id, value)?)?;
        }
        LogicalMutationRef::PortalRefund(id, value) => {
            set_projected(changes, projection::portal_refund(identity, id, value)?)?;
        }
        LogicalMutationRef::InboxRefund(id, value) => {
            set_projected(changes, projection::inbox_refund(identity, id, value)?)?;
        }
    }
    Ok(())
}

fn set_projected(
    changes: &mut ModelRowChanges,
    (key, value): projection::ProjectedRow,
) -> Result<(), ModelPersistenceError> {
    set(changes, key, value)
}

fn set(
    changes: &mut ModelRowChanges,
    key: ModelKey,
    value: Option<ModelValue>,
) -> Result<(), ModelPersistenceError> {
    if changes.insert(key, value).is_some() {
        Err(ModelPersistenceError::Duplicate {
            kind: "lowered model key",
        })
    } else {
        Ok(())
    }
}

fn require_portal_identity(
    expected: PortalIdentity,
    actual: PortalIdentity,
    kind: &'static str,
) -> Result<(), ModelPersistenceError> {
    if expected == actual {
        Ok(())
    } else {
        Err(ModelPersistenceError::PortalIdentityMismatch {
            kind,
            expected,
            actual,
        })
    }
}
