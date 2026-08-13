//! Loading tests.

use super::*;

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
    let snapshot = store.load().unwrap();
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
    assert!(matches!(store.load(), Err(PersistenceError::Invalid(_))));
}

#[test]
fn restart_and_reorg_replay_accept_multi_block_tempo_imports() {
    let (directory, store) = create();
    let prior = store.load().unwrap();
    let zone = block(1, 0x11);
    let imported = block(3, 0x23);
    let snapshot = store
        .apply(
            &prior,
            JournalEntry {
                zone,
                parent: bootstrap().zone,
                imported_tempo: imported,
                imported_tempo_parent: bootstrap().tempo,
                delta: StateDelta::default(),
            },
            zone,
            Coverage::Complete,
        )
        .unwrap();
    drop(store);

    let (store, reopened) = Persistence::open(directory.path(), identity()).unwrap();
    assert_eq!(reopened, snapshot);
    let reconstructed = store.reorg(&reopened, zone).unwrap();
    assert_eq!(reconstructed.meta.imported_tempo_tip, imported);
    assert_eq!(reconstructed.state, snapshot.state);
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
    assert!(matches!(store.load(), Err(PersistenceError::Invalid(_))));

    let (_directory, store) = create();
    let tx = store.db.tx_mut().unwrap();
    tx.put::<Journal>(1, entry(1, one)).unwrap();
    tx.commit().unwrap();
    assert!(matches!(store.load(), Err(PersistenceError::Invalid(_))));
}

#[test]
fn restart_checks_active_history_but_defers_orphan_audit_validation() {
    let (_directory, store) = create();
    let one = apply(&store, 1, bootstrap().zone);
    let snapshot = store.load().unwrap();
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
    assert!(matches!(store.load(), Err(PersistenceError::Invalid(_))));

    let (_directory, store) = create();
    let bad_id = super::super::CheckpointId {
        height: 9,
        hash: B256::repeat_byte(9),
    };
    let tx = store.db.tx_mut().unwrap();
    tx.put::<Checkpoints>(
        bad_id,
        super::super::Checkpoint {
            cut: bootstrap(),
            state: state(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    assert!(store.load().is_ok());

    let (_directory, store) = create();
    let (key, mut value) = finding(block(1, 0x11));
    value.details.code += 1;
    let tx = store.db.tx_mut().unwrap();
    tx.put::<Findings>(key, value).unwrap();
    tx.commit().unwrap();
    assert!(store.load().is_ok());
}

#[test]
fn stale_checkpoint_from_an_orphaned_branch_is_skipped() {
    let (_directory, store) = create();
    let one_a = apply(&store, 1, bootstrap().zone);
    let snapshot = store.load().unwrap();
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
    store.reorg(&current(&store), bootstrap().zone).unwrap();

    let mut replacement = entry(1, bootstrap().zone);
    replacement.zone = block(1, 0xb1);
    store
        .apply(
            &current(&store),
            replacement,
            block(1, 0xb1),
            Coverage::Complete,
        )
        .unwrap();
    let snapshot = store.reorg(&current(&store), block(1, 0xb1)).unwrap();
    assert_eq!(
        snapshot.meta.active_checkpoint,
        super::super::CheckpointId::from(block(1, 0xb1))
    );
    assert_eq!(snapshot.meta.verified_zone_tip, block(1, 0xb1));
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
    let checkpoint_bytes = codec::encode(&super::super::Checkpoint {
        cut: bootstrap(),
        state: state(),
    })
    .unwrap();
    assert!(checkpoint_bytes.len() < 1_024);

    let mut parent = bootstrap().zone;
    for number in 1..=256 {
        parent = apply(&store, number, parent);
        if number % 64 == 0 {
            let snapshot = store.load().unwrap();
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
