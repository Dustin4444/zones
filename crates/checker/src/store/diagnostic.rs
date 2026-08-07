//! Read-only inspection of one retained canonical model-key transition.

use alloy_eips::BlockNumHash;
use reth_db::Database;

use super::{
    db::{CheckerStore, finish_read, read_snapshot, validate_model_cut_coherence},
    error::{StoreError, StoreResult},
    history::{read_changeset_group, reconstruct_from, restore_changeset_rows},
    model_state::assemble_model,
    schema::ModelKey,
    value::{BeforeImage, ModelValue},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistoricalKeyChange {
    pub(crate) zone_before: BlockNumHash,
    pub(crate) zone_after: BlockNumHash,
    pub(crate) tempo_before: BlockNumHash,
    pub(crate) tempo_after: BlockNumHash,
    pub(crate) changeset_ordinal: Option<u32>,
    pub(crate) before: Option<ModelValue>,
    pub(crate) after: Option<ModelValue>,
}

impl CheckerStore {
    /// Reconstruct the target and its parent inside one read transaction.
    pub(crate) fn diagnose_key(
        &self,
        target: u64,
        selected: ModelKey,
    ) -> StoreResult<HistoricalKeyChange> {
        let tx = self.db.tx()?;
        let result = (|| {
            if target == 0 {
                return Err(StoreError::GenesisDiagnosticTarget);
            }
            let current = read_snapshot(&tx, self.identity, self.path())?;
            let after = reconstruct_from(&tx, self.identity, current, target)?;
            let group = read_changeset_group(
                &tx,
                after.verified_zone_tip.number,
                after.verified_zone_tip.hash,
            )?;
            let changeset_ordinal = changed_ordinal(&group, selected);
            let after_value = after.model_rows.get(&selected).cloned();
            let mut before_rows = after.model_rows;
            let block = restore_changeset_rows(
                &tx,
                after.verified_zone_tip,
                after.imported_tempo_tip,
                &group,
                &mut before_rows,
            )?;
            let before_model =
                assemble_model(self.identity.portal_identity(), before_rows.clone())?;
            validate_model_cut_coherence(
                &tx,
                self.identity,
                None,
                block.prior_verified_zone_tip,
                block.prior_imported_tempo_tip,
                &before_model,
            )?;
            Ok(HistoricalKeyChange {
                zone_before: block.prior_verified_zone_tip,
                zone_after: after.verified_zone_tip,
                tempo_before: block.prior_imported_tempo_tip,
                tempo_after: after.imported_tempo_tip,
                changeset_ordinal,
                before: before_rows.get(&selected).cloned(),
                after: after_value,
            })
        })();
        finish_read(tx, result)
    }
}

fn changed_ordinal(
    group: &[(super::schema::ChangesetKey, BeforeImage)],
    selected: ModelKey,
) -> Option<u32> {
    group.iter().find_map(|(stored, image)| match image {
        BeforeImage::Model { key, .. } if *key == selected => Some(stored.ordinal),
        BeforeImage::Block(_) | BeforeImage::Model { .. } => None,
    })
}
