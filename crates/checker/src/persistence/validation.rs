//! Validation for durable checker records and their continuity.

use crate::{
    kernel::{Datum, Finding as FindingDetails, FindingLocation, State, validate},
    persistence::{
        BlockNumHash, Checkpoint, CheckpointId, Coverage, Finding, FindingKey, Identity, Metadata,
        PersistenceError, Result, invalid,
    },
};

/// Validate state families, kernel invariants, and durable identity binding.
pub(super) fn validate_state(state: &State, identity: Identity) -> Result<()> {
    state
        .validate_families()
        .map_err(|e| invalid(e.to_string()))?;
    validate(state).map_err(|e| invalid(format!("invariant {e:?}")))?;
    let Some(portal) = state.portal() else {
        return Err(invalid("missing Portal identity"));
    };
    let portal_identity = portal.identity();
    if portal_identity.portal != identity.portal || portal_identity.zone_id != identity.zone_id {
        return Err(PersistenceError::Identity);
    }
    Ok(())
}

/// Validate a checkpoint key, cut, and embedded checker state.
pub(super) fn validate_checkpoint(
    id: CheckpointId,
    checkpoint: &Checkpoint,
    identity: Identity,
) -> Result<()> {
    if id != CheckpointId::from(checkpoint.cut.zone) {
        return Err(invalid("checkpoint key does not match its embedded cut"));
    }
    validate_state(&checkpoint.state, identity)
}

/// Encode an optional finding datum for stable evidence hashing.
fn canonical_datum(value: Option<&Datum>) -> Vec<u8> {
    value.map(Datum::canonical_bytes).unwrap_or_default()
}

/// Derive bounded canonical evidence metadata for a finding.
fn finding_evidence(
    details: &FindingDetails,
) -> Result<(usize, usize, u32, alloy_primitives::B256)> {
    let expected = canonical_datum(details.expected.as_ref());
    let actual = canonical_datum(details.actual.as_ref());
    let mut canonical = Vec::with_capacity(8 + expected.len() + actual.len());
    canonical.extend(
        u32::try_from(expected.len())
            .map_err(|_| invalid("expected too large"))?
            .to_be_bytes(),
    );
    canonical.extend_from_slice(&expected);
    canonical.extend(
        u32::try_from(actual.len())
            .map_err(|_| invalid("actual too large"))?
            .to_be_bytes(),
    );
    canonical.extend_from_slice(&actual);
    Ok((
        expected.len(),
        actual.len(),
        u32::try_from(canonical.len()).map_err(|_| invalid("evidence too large"))?,
        alloy_primitives::keccak256(canonical),
    ))
}

/// Derive the durable operation coordinate represented by a finding location.
fn finding_operation(location: Option<&FindingLocation>) -> u32 {
    match location {
        Some(FindingLocation::Operation(operation))
        | Some(FindingLocation::ImportedOperation(operation)) => *operation,
        Some(FindingLocation::State(_) | FindingLocation::Block) | None => 0,
    }
}

/// Build a durable finding and its stable evidence key.
pub(crate) fn make_finding(
    zone: BlockNumHash,
    parent: BlockNumHash,
    imported: Option<(BlockNumHash, BlockNumHash)>,
    details: FindingDetails,
    summary: String,
) -> Result<(FindingKey, Finding)> {
    let operation = finding_operation(details.location.as_ref());
    let key = FindingKey {
        zone,
        operation,
        code: details.code,
    };
    let (_, _, evidence_len, evidence_digest) = finding_evidence(&details)?;
    let (imported_tempo, imported_tempo_parent) = imported.unzip();
    let finding = Finding {
        zone,
        parent,
        imported_tempo,
        imported_tempo_parent,
        details,
        evidence_len,
        evidence_digest,
        summary,
    };
    validate_finding(key, &finding, None)?;
    Ok((key, finding))
}

/// Return whether optional imported Tempo coordinates extend the prior tip.
fn valid_imported_finding_coordinate(
    finding: &Finding,
    previous_imported_tempo_tip: BlockNumHash,
) -> bool {
    match (finding.imported_tempo, finding.imported_tempo_parent) {
        (None, None) => true,
        (Some(imported), Some(imported_parent)) => {
            imported.number > previous_imported_tempo_tip.number
                && imported_parent == previous_imported_tempo_tip
        }
        _ => false,
    }
}

/// Validate durable finding identity, evidence bounds, and optional continuity.
pub(super) fn validate_finding(
    key: FindingKey,
    finding: &Finding,
    meta: Option<&Metadata>,
) -> Result<()> {
    let (expected_len, actual_len, evidence_len, evidence_digest) =
        finding_evidence(&finding.details)?;
    if finding.zone != key.zone
        || finding_operation(finding.details.location.as_ref()) != key.operation
        || finding.details.code != key.code
        || expected_len > 256
        || actual_len > 256
        || finding.summary.len() > 1_024
        || finding.evidence_len != evidence_len
        || finding.evidence_digest != evidence_digest
    {
        return Err(invalid("finding is inconsistent or exceeds codec bounds"));
    }
    if let Some(meta) = meta {
        let next = meta
            .verified_zone_tip
            .number
            .checked_add(1)
            .ok_or_else(|| invalid("height overflow"))?;
        if finding.zone.number != next
            || finding.parent != meta.verified_zone_tip
            || !valid_imported_finding_coordinate(finding, meta.imported_tempo_tip)
        {
            return Err(invalid("finding is not at the next verified coordinate"));
        }
    }
    Ok(())
}

/// Validate metadata tips, checkpoints, and coverage coordinates.
pub(super) fn validate_metadata(meta: &Metadata) -> Result<()> {
    if meta.active_checkpoint.height > meta.verified_zone_tip.number
        || meta.acknowledged_zone_tip.number < meta.verified_zone_tip.number
    {
        return Err(invalid("metadata tips or active checkpoint are incoherent"));
    }
    match &meta.coverage {
        Coverage::Complete if meta.acknowledged_zone_tip != meta.verified_zone_tip => {
            Err(invalid("complete coverage tips differ"))
        }
        Coverage::Gap {
            first_unchecked,
            acknowledged_through,
            ..
        } if meta.verified_zone_tip.number.checked_add(1) != Some(first_unchecked.number)
            || *acknowledged_through != meta.acknowledged_zone_tip =>
        {
            Err(invalid("coverage gap coordinates are incoherent"))
        }
        _ => Ok(()),
    }
}

/// Validate one coverage-state transition caused by a verified child block.
pub(super) fn validate_coverage_advance(
    meta: &Metadata,
    child: BlockNumHash,
    acknowledged: BlockNumHash,
    next: &Coverage,
) -> Result<()> {
    if acknowledged.number < meta.acknowledged_zone_tip.number {
        return Err(invalid("acknowledged tip cannot regress"));
    }
    match (&meta.coverage, next) {
        (Coverage::Complete, Coverage::Complete) if acknowledged == child => Ok(()),
        (
            Coverage::Complete,
            Coverage::Gap {
                first_unchecked,
                acknowledged_through,
                ..
            },
        ) if child.number.checked_add(1) == Some(first_unchecked.number)
            && *acknowledged_through == acknowledged
            && acknowledged.number >= first_unchecked.number =>
        {
            Ok(())
        }
        (
            Coverage::Gap {
                first_unchecked,
                acknowledged_through,
                reason: _,
            },
            Coverage::Complete,
        ) if child == *first_unchecked
            && child == *acknowledged_through
            && acknowledged == *acknowledged_through =>
        {
            Ok(())
        }
        (
            Coverage::Gap {
                first_unchecked,
                acknowledged_through,
                reason,
            },
            Coverage::Gap {
                first_unchecked: next_first,
                acknowledged_through: next_through,
                reason: next_reason,
            },
        ) if child == *first_unchecked
            && child.number.checked_add(1) == Some(next_first.number)
            && *next_through == *acknowledged_through
            && acknowledged == *acknowledged_through
            && next_reason == reason =>
        {
            Ok(())
        }
        _ => Err(invalid("coverage transition is inconsistent")),
    }
}
