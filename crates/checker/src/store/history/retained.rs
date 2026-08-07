//! Reverse reconstruction and integrity validation for retained canonical history.

use alloy_eips::BlockNumHash;
use alloy_primitives::B256;
use reth_db::{cursor::DbCursorRO, transaction::DbTx};

use crate::store::{
    db::validate_model_cut_coherence,
    error::{StoreError, StoreResult},
    model_state::{ModelRows, assemble_model},
    operations::validate_model_value,
    schema::{ChangesetKey, CheckerCanonical, CheckerChangesets},
    value::{BeforeImage, BlockBeforeImage, StoreIdentity},
};

use super::HistoricalSnapshot;

pub(in crate::store) fn reconstruct_from<TX: DbTx>(
    tx: &TX,
    identity: StoreIdentity,
    current: crate::store::db::StoreSnapshot,
    target: u64,
) -> StoreResult<HistoricalSnapshot> {
    if target > current.verified_zone_tip.number {
        return Err(StoreError::FutureTarget {
            target,
            current: current.verified_zone_tip.number,
        });
    }
    let mut zone_tip = current.verified_zone_tip;
    let mut tempo_tip = current.imported_tempo_tip;
    let mut model = current.model;
    let mut rows = current.model_rows;
    while zone_tip.number > target {
        let canonical = required_canonical(tx, zone_tip.number)?;
        if canonical != zone_tip.hash {
            return Err(StoreError::CanonicalConflict {
                height: zone_tip.number,
                expected: zone_tip.hash,
                actual: canonical,
            });
        }
        let block = unwind_changeset(tx, zone_tip, tempo_tip, &mut rows)?;
        zone_tip = block.prior_verified_zone_tip;
        tempo_tip = block.prior_imported_tempo_tip;
        model = assemble_model(identity.portal_identity(), rows.clone())?;
        validate_model_cut_coherence(tx, identity, None, zone_tip, tempo_tip, &model)?;
    }
    let canonical = required_canonical(tx, target)?;
    if canonical != zone_tip.hash {
        return Err(StoreError::CanonicalConflict {
            height: target,
            expected: zone_tip.hash,
            actual: canonical,
        });
    }
    Ok(HistoricalSnapshot {
        verified_zone_tip: zone_tip,
        imported_tempo_tip: tempo_tip,
        model,
        model_rows: rows,
    })
}

fn unwind_changeset<TX: DbTx>(
    tx: &TX,
    child_zone: BlockNumHash,
    child_tempo: BlockNumHash,
    rows: &mut ModelRows,
) -> StoreResult<BlockBeforeImage> {
    let group = read_changeset_group(tx, child_zone.number, child_zone.hash)?;
    restore_changeset_rows(tx, child_zone, child_tempo, &group, rows)
}

pub(in crate::store) fn restore_changeset_rows<TX: DbTx>(
    tx: &TX,
    child_zone: BlockNumHash,
    child_tempo: BlockNumHash,
    group: &[(ChangesetKey, BeforeImage)],
    rows: &mut ModelRows,
) -> StoreResult<BlockBeforeImage> {
    let height = child_zone.number;
    let hash = child_zone.hash;
    let Some((_, BeforeImage::Block(block))) = group.first() else {
        return invalid_changeset(height, hash, "ordinal zero is not block metadata");
    };
    let block = *block;
    require_parent_links(tx, child_zone, child_tempo, block)?;
    if group.len() != block.mutation_count as usize + 1 {
        return invalid_changeset(height, hash, "row count differs from block metadata");
    }

    let mut previous = None;
    for (expected_ordinal, (stored_key, image)) in (1..=block.mutation_count).zip(&group[1..]) {
        if stored_key.ordinal != expected_ordinal {
            return invalid_changeset(height, hash, "changeset ordinals are not consecutive");
        }
        let BeforeImage::Model { key, value } = image else {
            return invalid_changeset(height, hash, "mutation ordinal contains block metadata");
        };
        if previous.is_some_and(|prior| *key <= prior) {
            return invalid_changeset(height, hash, "model keys are duplicate or unordered");
        }
        if rows.get(key) == value.as_deref() {
            return invalid_changeset(height, hash, "before-image equals the child model row");
        }
        if let Some(value) = value {
            validate_model_value(*key, value)?;
        }
        match value {
            Some(value) => {
                rows.insert(*key, value.as_ref().clone());
            }
            None => {
                rows.remove(key);
            }
        }
        previous = Some(*key);
    }
    Ok(block)
}

pub(in crate::store) fn read_changeset_group<TX: DbTx>(
    tx: &TX,
    height: u64,
    hash: B256,
) -> StoreResult<Vec<(ChangesetKey, BeforeImage)>> {
    let mut cursor = tx.cursor_read::<CheckerChangesets>()?;
    if let Some((key, _)) = cursor.seek(ChangesetKey::new(height, B256::ZERO, 0))?
        && key.zone_height == height
        && key.block_hash != hash
    {
        return invalid_changeset(
            height,
            hash,
            "changeset height contains a conflicting block hash",
        );
    }
    let metadata_key = ChangesetKey::new(height, hash, 0);
    let Some((key, metadata)) = cursor.seek(metadata_key)? else {
        return Err(StoreError::MissingChangeset {
            height,
            hash,
            ordinal: 0,
        });
    };
    if key != metadata_key {
        return invalid_changeset(height, hash, "changeset metadata is missing");
    }

    let BeforeImage::Block(block) = &metadata else {
        return invalid_changeset(height, hash, "ordinal zero is not block metadata");
    };
    let mutation_count = block.mutation_count;
    let mut rows = vec![(key, metadata)];
    for expected_ordinal in 1..=mutation_count {
        let Some((key, value)) = cursor.next()? else {
            return invalid_changeset(height, hash, "changeset mutation row is missing");
        };
        if key.zone_height != height || key.block_hash != hash {
            return invalid_changeset(height, hash, "changeset mutation row is missing");
        }
        if key.ordinal != expected_ordinal {
            return invalid_changeset(height, hash, "changeset ordinals are not consecutive");
        }
        rows.push((key, value));
    }
    if let Some((key, _)) = cursor.next()?
        && key.zone_height == height
    {
        let reason = if key.block_hash == hash {
            "changeset has surplus mutation rows"
        } else {
            "changeset height contains a conflicting block hash"
        };
        return invalid_changeset(height, hash, reason);
    }
    Ok(rows)
}

pub(super) fn validate_canonical_table<TX: DbTx>(
    tx: &TX,
    tip: BlockNumHash,
    genesis: B256,
) -> StoreResult<()> {
    let mut cursor = tx.cursor_read::<CheckerCanonical>()?;
    let mut expected = 0_u64;
    let mut last = None;
    for row in cursor.walk(None)? {
        let (height, hash) = row?;
        let hash = hash.into_inner();
        if height != expected {
            return Err(StoreError::CanonicalSequence(
                "height gap or row above verified tip",
            ));
        }
        if height == 0 && hash != genesis {
            return Err(StoreError::CanonicalConflict {
                height,
                expected: genesis,
                actual: hash,
            });
        }
        last = Some(BlockNumHash::new(height, hash));
        expected = expected
            .checked_add(1)
            .ok_or(StoreError::CanonicalSequence("height overflow"))?;
    }
    if last != Some(tip) {
        return Err(StoreError::CanonicalSequence(
            "canonical tail differs from verified tip",
        ));
    }
    Ok(())
}

pub(super) fn validate_all_changeset_keys<TX: DbTx>(tx: &TX, tip: u64) -> StoreResult<()> {
    let mut cursor = tx.cursor_read::<CheckerChangesets>()?;
    for row in cursor.walk(None)? {
        let (key, _) = row?;
        if key.zone_height == 0 || key.zone_height > tip {
            return invalid_changeset(
                key.zone_height,
                key.block_hash,
                "row is outside canonical history",
            );
        }
        let canonical = required_canonical(tx, key.zone_height)?;
        if canonical != key.block_hash {
            return Err(StoreError::CanonicalConflict {
                height: key.zone_height,
                expected: canonical,
                actual: key.block_hash,
            });
        }
    }
    Ok(())
}

fn require_parent_links<TX: DbTx>(
    tx: &TX,
    child_zone: BlockNumHash,
    child_tempo: BlockNumHash,
    block: BlockBeforeImage,
) -> StoreResult<()> {
    if block.prior_verified_zone_tip.number.checked_add(1) != Some(child_zone.number)
        || block.prior_imported_tempo_tip.number.checked_add(1) != Some(child_tempo.number)
    {
        return invalid_changeset(
            child_zone.number,
            child_zone.hash,
            "parent numbers are not adjacent",
        );
    }
    let parent_hash = required_canonical(tx, block.prior_verified_zone_tip.number)?;
    if parent_hash != block.prior_verified_zone_tip.hash {
        return Err(StoreError::CanonicalConflict {
            height: block.prior_verified_zone_tip.number,
            expected: block.prior_verified_zone_tip.hash,
            actual: parent_hash,
        });
    }
    Ok(())
}

pub(in crate::store) fn required_canonical<TX: DbTx>(tx: &TX, height: u64) -> StoreResult<B256> {
    tx.get::<CheckerCanonical>(height)?
        .map(crate::store::schema::CanonicalHash::into_inner)
        .ok_or(StoreError::MissingCanonical { height })
}

pub(in crate::store) fn invalid_changeset<T>(
    height: u64,
    hash: B256,
    reason: &'static str,
) -> StoreResult<T> {
    Err(StoreError::InvalidChangeset {
        height,
        hash,
        reason,
    })
}
