use alloy_primitives::{Address, B256};
use reth_db::{
    Database, TableSet,
    cursor::{DbCursorRO, DbCursorRW},
    transaction::{DbTx, DbTxMut},
};
use tempfile::TempDir;
use zone_checker_kernel::{
    ImportedFacts, PortalIdentity, State, StateDelta, ZoneFacts, ZoneOperation, apply_imported,
    apply_zone,
};

use super::{
    BlockNumHash, ChainCut, Coverage, CoverageGapReason, Finding, FindingKey, Identity,
    JournalEntry, MetaValue, Persistence, PersistenceError, SCHEMA_VERSION, codec,
    schema::{Checkpoints, Findings, Journal, Meta, MetaKey, PersistenceTables},
};

fn block(number: u64, byte: u8) -> BlockNumHash {
    BlockNumHash {
        number,
        hash: B256::repeat_byte(byte),
    }
}

fn identity() -> Identity {
    Identity {
        l1_chain_id: 1,
        zone_chain_id: 2,
        zone_id: 7,
        portal: Address::repeat_byte(0x70),
        creation_block: B256::repeat_byte(0xc0),
        creation_height: 0,
    }
}

fn state() -> State {
    State::awaiting(PortalIdentity {
        portal: identity().portal,
        zone_id: identity().zone_id,
        initial_token: Address::repeat_byte(0x11),
    })
}

fn bootstrap() -> ChainCut {
    ChainCut {
        zone: block(0, 0x10),
        tempo: block(0, 0x20),
    }
}

fn entry(number: u64, parent: BlockNumHash) -> JournalEntry {
    JournalEntry {
        zone: block(number, 0x10u8.wrapping_add(number as u8)),
        parent,
        imported_tempo: block(number, 0x20u8.wrapping_add(number as u8)),
        imported_tempo_parent: block(
            number.saturating_sub(1),
            0x20u8.wrapping_add(number.saturating_sub(1) as u8),
        ),
        delta: StateDelta::default(),
    }
}

fn create() -> (TempDir, Persistence) {
    let directory = tempfile::tempdir().unwrap();
    let (store, snapshot) =
        Persistence::create(directory.path(), identity(), bootstrap(), state()).unwrap();
    assert_eq!(snapshot.meta.verified_zone_tip, bootstrap().zone);
    (directory, store)
}

fn apply(store: &Persistence, number: u64, parent: BlockNumHash) -> BlockNumHash {
    let snapshot = store.load(identity()).unwrap();
    let candidate = apply_zone(
        apply_imported(&snapshot.state, &ImportedFacts::default()).unwrap(),
        &ZoneFacts {
            operations: vec![ZoneOperation::UpdateTempoGasRate(u128::from(number))],
            ..ZoneFacts::default()
        },
    )
    .unwrap();
    let value = JournalEntry {
        delta: candidate.delta,
        ..entry(number, parent)
    };
    let tip = value.zone;
    store
        .apply(identity(), value, tip, Coverage::Complete)
        .unwrap();
    tip
}

fn finding(zone: BlockNumHash) -> (FindingKey, Finding) {
    let key = FindingKey {
        zone,
        operation: 3,
        code: 9,
    };
    let expected = vec![1, 2];
    let actual = vec![3, 4];
    let mut canonical = vec![];
    canonical.extend((expected.len() as u32).to_be_bytes());
    canonical.extend(&expected);
    canonical.extend((actual.len() as u32).to_be_bytes());
    canonical.extend(&actual);
    let value = Finding {
        zone,
        parent: block(zone.number - 1, 0x10 + zone.number as u8 - 1),
        imported_tempo: Some(block(zone.number, 0x20 + zone.number as u8)),
        category: 2,
        code: key.code,
        location: 4,
        operation: key.operation,
        expected,
        actual,
        evidence_len: u32::try_from(canonical.len()).unwrap(),
        evidence_digest: alloy_primitives::keccak256(canonical),
        summary: "authenticated divergence".into(),
    };
    (key, value)
}

#[test]
fn bounded_versioned_codec_rejects_unknown_trailing_truncated_and_oversize() {
    assert_eq!(PersistenceTables::tables().count(), 4);
    let (_, value) = finding(block(1, 0x11));
    let encoded = codec::encode(&value).unwrap();
    assert_eq!(codec::decode::<Finding>(&encoded).unwrap(), value);

    let mut unknown = encoded.clone();
    unknown[0] ^= 1;
    assert!(matches!(
        codec::decode::<Finding>(&unknown),
        Err(codec::CodecError::Version(_))
    ));
    let mut trailing = encoded.clone();
    trailing.push(0);
    assert!(matches!(
        codec::decode::<Finding>(&trailing),
        Err(codec::CodecError::Malformed(_))
    ));
    assert!(codec::decode::<Finding>(&encoded[..encoded.len() - 1]).is_err());
    assert!(matches!(
        codec::decode::<Finding>(&vec![0; codec::MAX_VALUE_SIZE as usize + 1]),
        Err(codec::CodecError::Oversize)
    ));
}

#[test]
fn restart_replays_checkpoint_and_unbroken_journal_and_rejects_missing_rows() {
    let (directory, store) = create();
    let one = apply(&store, 1, bootstrap().zone);
    let two = apply(&store, 2, one);
    let snapshot = store.load(identity()).unwrap();
    store
        .checkpoint(
            identity(),
            ChainCut {
                zone: two,
                tempo: block(2, 0x22),
            },
            snapshot.state,
        )
        .unwrap();
    let three = apply(&store, 3, two);
    drop(store);

    let (store, snapshot) = Persistence::open(directory.path(), identity()).unwrap();
    assert_eq!(snapshot.meta.verified_zone_tip, three);
    let tx = store.db.tx_mut().unwrap();
    let mut cursor = tx.cursor_write::<Journal>().unwrap();
    cursor.seek_exact(3).unwrap();
    cursor.delete_current().unwrap();
    tx.commit().unwrap();
    assert!(matches!(
        store.load(identity()),
        Err(PersistenceError::Invalid(_))
    ));
}

#[test]
fn restart_rejects_conflicting_and_surplus_journal_rows() {
    let (_directory, store) = create();
    let one = apply(&store, 1, bootstrap().zone);
    let tx = store.db.tx_mut().unwrap();
    tx.put::<Journal>(
        1,
        JournalEntry {
            parent: block(0, 0xff),
            ..entry(1, bootstrap().zone)
        },
    )
    .unwrap();
    tx.commit().unwrap();
    assert!(matches!(
        store.load(identity()),
        Err(PersistenceError::Invalid(_))
    ));

    let (_directory, store) = create();
    let tx = store.db.tx_mut().unwrap();
    tx.put::<Journal>(1, entry(1, one)).unwrap();
    tx.commit().unwrap();
    assert!(matches!(
        store.load(identity()),
        Err(PersistenceError::Invalid(_))
    ));
}

#[test]
fn restart_rejects_corrupt_checkpoint_finding_and_pre_checkpoint_journal() {
    let (_directory, store) = create();
    let one = apply(&store, 1, bootstrap().zone);
    let snapshot = store.load(identity()).unwrap();
    store
        .checkpoint(
            identity(),
            ChainCut {
                zone: one,
                tempo: block(1, 0x21),
            },
            snapshot.state,
        )
        .unwrap();
    apply(&store, 2, one);
    let tx = store.db.tx_mut().unwrap();
    tx.put::<Journal>(
        1,
        JournalEntry {
            parent: block(0, 0xff),
            ..entry(1, bootstrap().zone)
        },
    )
    .unwrap();
    tx.commit().unwrap();
    assert!(matches!(
        store.load(identity()),
        Err(PersistenceError::Invalid(_))
    ));

    let (_directory, store) = create();
    let bad_id = super::CheckpointId {
        height: 9,
        hash: B256::repeat_byte(9),
    };
    let tx = store.db.tx_mut().unwrap();
    tx.put::<Checkpoints>(
        bad_id,
        super::Checkpoint {
            cut: bootstrap(),
            state: state(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    assert!(matches!(
        store.load(identity()),
        Err(PersistenceError::Invalid(_))
    ));

    let (_directory, store) = create();
    let (key, mut value) = finding(block(1, 0x11));
    value.code += 1;
    let tx = store.db.tx_mut().unwrap();
    tx.put::<Findings>(key, value).unwrap();
    tx.commit().unwrap();
    assert!(matches!(
        store.load(identity()),
        Err(PersistenceError::Invalid(_))
    ));
}

#[test]
fn reorg_before_after_and_across_checkpoints_reconstructs_exact_metadata() {
    let (_directory, store) = create();
    let one = apply(&store, 1, bootstrap().zone);
    let snapshot = store.load(identity()).unwrap();
    store
        .checkpoint(
            identity(),
            ChainCut {
                zone: one,
                tempo: block(1, 0x21),
            },
            snapshot.state,
        )
        .unwrap();
    let two = apply(&store, 2, one);
    let three = apply(&store, 3, two);

    assert_eq!(
        store.reorg(identity(), two).unwrap().meta.verified_zone_tip,
        two
    );
    let replacement_three = apply(&store, 3, two);
    assert_eq!(replacement_three, three);
    assert_eq!(
        store
            .reorg(identity(), bootstrap().zone)
            .unwrap()
            .meta
            .active_checkpoint
            .height,
        0
    );
    assert_eq!(
        store.load(identity()).unwrap().meta.verified_zone_tip,
        bootstrap().zone
    );
}

#[test]
fn same_height_finding_is_idempotent_but_conflicting_evidence_is_rejected() {
    let (_directory, store) = create();
    let (key, value) = finding(block(1, 0x11));
    store
        .record_finding(identity(), key, value.clone())
        .unwrap();
    store
        .record_finding(identity(), key, value.clone())
        .unwrap();

    let mut conflicting = value;
    conflicting.actual.push(5);
    assert!(matches!(
        store.record_finding(identity(), key, conflicting),
        Err(PersistenceError::Invalid(_))
    ));
    assert_eq!(
        store.load(identity()).unwrap().meta.active_finding,
        Some(key)
    );
}

#[test]
fn finding_identity_ignores_summary_but_separates_codes() {
    let (_directory, store) = create();
    let (key, value) = finding(block(1, 0x11));
    let mut reworded = value.clone();
    reworded.summary = "new display wording".into();
    store.record_finding(identity(), key, value).unwrap();
    store.record_finding(identity(), key, reworded).unwrap();

    let mut other_key = key;
    other_key.code += 1;
    let mut other = finding(block(1, 0x11)).1;
    other.code = other_key.code;
    store.record_finding(identity(), other_key, other).unwrap();
    assert_eq!(
        store.load(identity()).unwrap().meta.active_finding,
        Some(other_key)
    );
}

#[test]
fn finding_rejects_forged_evidence_and_wrong_coordinates() {
    let (_directory, store) = create();
    let (key, value) = finding(block(1, 0x11));
    let mut forged = value.clone();
    forged.evidence_len += 1;
    assert!(store.record_finding(identity(), key, forged).is_err());
    let mut forged = value.clone();
    forged.evidence_digest = B256::ZERO;
    assert!(store.record_finding(identity(), key, forged).is_err());
    let (wrong_key, wrong) = finding(block(2, 0x12));
    assert!(store.record_finding(identity(), wrong_key, wrong).is_err());
    let mut wrong_parent = value;
    wrong_parent.parent.hash = B256::ZERO;
    assert!(store.record_finding(identity(), key, wrong_parent).is_err());
}

#[test]
fn alert_descendant_reorg_preserves_or_removes_the_latch_by_exact_height() {
    let (_directory, store) = create();
    let finding_block = block(1, 0x41);
    let (key, value) = finding(finding_block);
    store.record_finding(identity(), key, value).unwrap();
    store
        .record_gap(
            identity(),
            finding_block,
            block(3, 0x43),
            CoverageGapReason::Other(9),
        )
        .unwrap();
    assert_eq!(
        store
            .reorg(identity(), block(2, 0x42))
            .unwrap()
            .meta
            .active_finding,
        Some(key)
    );
    assert!(matches!(
        store.reorg(identity(), block(1, 0xff)),
        Err(PersistenceError::Invalid(_))
    ));
    assert_eq!(
        store
            .reorg(identity(), bootstrap().zone)
            .unwrap()
            .meta
            .active_finding,
        None
    );
}

#[test]
fn deep_reorg_retains_orphan_finding_as_structural_audit_record() {
    let (directory, store) = create();
    let one = apply(&store, 1, bootstrap().zone);
    let _two = apply(&store, 2, one);
    let finding_block = block(3, 0x43);
    let (key, value) = finding(finding_block);
    store.record_finding(identity(), key, value).unwrap();
    store
        .record_gap(
            identity(),
            finding_block,
            block(4, 0x44),
            CoverageGapReason::NotCheckedAncestorDivergence,
        )
        .unwrap();
    assert_eq!(
        store
            .reorg(identity(), bootstrap().zone)
            .unwrap()
            .meta
            .active_finding,
        None
    );
    drop(store);
    let (_, reopened) = Persistence::open(directory.path(), identity()).unwrap();
    assert_eq!(reopened.meta.verified_zone_tip, bootstrap().zone);
    assert_eq!(reopened.meta.active_finding, None);
}

#[test]
fn stale_orphan_cannot_be_installed_as_active_finding() {
    let (_directory, store) = create();
    let old = block(1, 0x41);
    let (key, value) = finding(old);
    store.record_finding(identity(), key, value).unwrap();
    store
        .record_gap(
            identity(),
            old,
            old,
            CoverageGapReason::NotCheckedAncestorDivergence,
        )
        .unwrap();
    store.reorg(identity(), bootstrap().zone).unwrap();
    let replacement = apply(&store, 1, bootstrap().zone);
    assert_ne!(replacement, old);

    let mut meta = store.load(identity()).unwrap().meta;
    meta.active_finding = Some(key);
    let tx = store.db.tx_mut().unwrap();
    tx.put::<Meta>(MetaKey::State, MetaValue::State(Box::new(meta)))
        .unwrap();
    tx.commit().unwrap();
    assert!(matches!(
        store.load(identity()),
        Err(PersistenceError::Invalid(_))
    ));
}

#[test]
fn gap_is_durable_before_acknowledgement_advances() {
    let (directory, store) = create();
    let first = block(1, 0x31);
    let through = block(4, 0x34);
    store
        .record_gap(
            identity(),
            first,
            through,
            CoverageGapReason::MissingReceipts,
        )
        .unwrap();
    drop(store);
    let (_, snapshot) = Persistence::open(directory.path(), identity()).unwrap();
    assert_eq!(snapshot.meta.acknowledged_zone_tip, through);
    assert_eq!(
        snapshot.meta.coverage,
        Coverage::Gap {
            first_unchecked: first,
            acknowledged_through: through,
            reason: CoverageGapReason::MissingReceipts,
        }
    );
}

#[test]
fn partial_gap_recovery_never_regresses_acknowledgement_or_erases_the_suffix() {
    let (_directory, store) = create();
    let one = block(1, 0x11);
    let two = block(2, 0x12);
    let three = block(3, 0x13);
    let reason = CoverageGapReason::ProviderUnavailable;
    store
        .record_gap(identity(), one, three, reason.clone())
        .unwrap();
    assert!(matches!(
        store.apply(
            identity(),
            entry(1, bootstrap().zone),
            one,
            Coverage::Complete
        ),
        Err(PersistenceError::Invalid(_))
    ));
    store
        .apply(
            identity(),
            entry(1, bootstrap().zone),
            three,
            Coverage::Gap {
                first_unchecked: two,
                acknowledged_through: three,
                reason: reason.clone(),
            },
        )
        .unwrap();
    store
        .apply(
            identity(),
            entry(2, one),
            three,
            Coverage::Gap {
                first_unchecked: three,
                acknowledged_through: three,
                reason,
            },
        )
        .unwrap();
    let snapshot = store
        .apply(identity(), entry(3, two), three, Coverage::Complete)
        .unwrap();
    assert_eq!(snapshot.meta.verified_zone_tip, three);
    assert_eq!(snapshot.meta.acknowledged_zone_tip, three);
    assert_eq!(snapshot.meta.coverage, Coverage::Complete);
}

#[test]
fn stale_checkpoint_from_an_orphaned_branch_is_skipped() {
    let (_directory, store) = create();
    let one_a = apply(&store, 1, bootstrap().zone);
    let snapshot = store.load(identity()).unwrap();
    store
        .checkpoint(
            identity(),
            ChainCut {
                zone: one_a,
                tempo: block(1, 0x21),
            },
            snapshot.state,
        )
        .unwrap();
    apply(&store, 2, one_a);
    store.reorg(identity(), bootstrap().zone).unwrap();

    let mut replacement = entry(1, bootstrap().zone);
    replacement.zone = block(1, 0xb1);
    store
        .apply(identity(), replacement, block(1, 0xb1), Coverage::Complete)
        .unwrap();
    let snapshot = store.reorg(identity(), block(1, 0xb1)).unwrap();
    assert_eq!(snapshot.meta.active_checkpoint.height, 0);
    assert_eq!(snapshot.meta.verified_zone_tip, block(1, 0xb1));
}

#[test]
fn transaction_abort_leaves_apply_checkpoint_and_reorg_fully_old() {
    let (directory, store) = create();
    store.inject_abort();
    assert!(matches!(
        store.apply(
            identity(),
            entry(1, bootstrap().zone),
            block(1, 0x11),
            Coverage::Complete
        ),
        Err(PersistenceError::InjectedAbort)
    ));
    assert_eq!(
        store.load(identity()).unwrap().meta.verified_zone_tip,
        bootstrap().zone
    );

    let one = apply(&store, 1, bootstrap().zone);
    let snapshot = store.load(identity()).unwrap();
    store.inject_abort();
    assert!(matches!(
        store.checkpoint(
            identity(),
            ChainCut {
                zone: one,
                tempo: block(1, 0x21)
            },
            snapshot.state
        ),
        Err(PersistenceError::InjectedAbort)
    ));
    assert_eq!(
        store
            .load(identity())
            .unwrap()
            .meta
            .active_checkpoint
            .height,
        0
    );

    let two = apply(&store, 2, one);
    store.inject_abort();
    assert!(matches!(
        store.reorg(identity(), one),
        Err(PersistenceError::InjectedAbort)
    ));
    assert_eq!(store.load(identity()).unwrap().meta.verified_zone_tip, two);
    drop(store);
    let (_, reopened) = Persistence::open(directory.path(), identity()).unwrap();
    assert_eq!(reopened.meta.verified_zone_tip, two);
}

#[test]
fn finding_and_gap_abort_leave_latches_and_acknowledgement_fully_old() {
    let (directory, store) = create();
    let (key, value) = finding(block(1, 0x41));
    store.inject_abort();
    assert!(matches!(
        store.record_finding(identity(), key, value),
        Err(PersistenceError::InjectedAbort)
    ));
    assert_eq!(store.load(identity()).unwrap().meta.active_finding, None);

    store.inject_abort();
    assert!(matches!(
        store.record_gap(
            identity(),
            block(1, 0x41),
            block(3, 0x43),
            CoverageGapReason::MissingTempoData,
        ),
        Err(PersistenceError::InjectedAbort)
    ));
    drop(store);
    let (_, snapshot) = Persistence::open(directory.path(), identity()).unwrap();
    assert_eq!(snapshot.meta.acknowledged_zone_tip, bootstrap().zone);
    assert_eq!(snapshot.meta.coverage, Coverage::Complete);
    assert_eq!(snapshot.meta.active_finding, None);
}

#[test]
fn checkpoint_ids_are_immutable_including_the_bootstrap_checkpoint() {
    let (_directory, store) = create();
    let mut rows = state().rows().clone();
    let zone_checker_kernel::StateValue::Zone(mut zone) =
        rows[&zone_checker_kernel::StateKey::Zone].clone()
    else {
        unreachable!()
    };
    zone.tempo_gas_rate = 1;
    rows.insert(
        zone_checker_kernel::StateKey::Zone,
        zone_checker_kernel::StateValue::Zone(zone),
    );
    let conflicting = State::from_rows(rows).unwrap();
    assert!(matches!(
        store.checkpoint(identity(), bootstrap(), conflicting),
        Err(PersistenceError::Invalid(_))
    ));
    assert_eq!(store.load(identity()).unwrap().state, state());
}

#[test]
fn schema_version_is_probed_before_incompatible_metadata_is_opened_writable() {
    let (directory, store) = create();
    let tx = store.db.tx_mut().unwrap();
    tx.put::<Meta>(MetaKey::Version, MetaValue::Version(SCHEMA_VERSION + 1))
        .unwrap();
    tx.commit().unwrap();
    drop(store);
    let error = match Persistence::open(directory.path(), identity()) {
        Err(error) => error,
        Ok(_) => panic!("incompatible schema opened writable"),
    };
    assert!(
        matches!(error, PersistenceError::Schema { actual, .. } if actual == SCHEMA_VERSION + 1)
    );
}

#[test]
fn checkpoint_size_and_bounded_journal_replay_are_measured() {
    let (directory, store) = create();
    let checkpoint_bytes = codec::encode(&super::Checkpoint {
        cut: bootstrap(),
        state: state(),
    })
    .unwrap();
    assert!(checkpoint_bytes.len() < 1_024);

    let mut parent = bootstrap().zone;
    for number in 1..=256 {
        parent = apply(&store, number, parent);
        if number % 64 == 0 {
            let snapshot = store.load(identity()).unwrap();
            store
                .checkpoint(
                    identity(),
                    ChainCut {
                        zone: parent,
                        tempo: block(number, 0x20u8.wrapping_add(number as u8)),
                    },
                    snapshot.state,
                )
                .unwrap();
        }
    }
    drop(store);
    let started = std::time::Instant::now();
    let (_, snapshot) = Persistence::open(directory.path(), identity()).unwrap();
    assert_eq!(snapshot.meta.verified_zone_tip.number, 256);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}
