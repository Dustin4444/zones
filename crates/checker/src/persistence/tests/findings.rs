//! Findings tests.

use super::*;

#[test]
fn same_height_finding_is_idempotent_but_conflicting_evidence_is_rejected() {
    let (_directory, store) = create();
    let (key, value) = finding(block(1, 0x11));
    store
        .record_divergence(&current(&store), key, value.clone(), key.zone)
        .unwrap();
    store
        .record_divergence(&current(&store), key, value.clone(), key.zone)
        .unwrap();

    let mut conflicting = value;
    conflicting.details.actual = Some(Datum::Code(5));
    assert!(matches!(
        store.record_divergence(&current(&store), key, conflicting, key.zone),
        Err(PersistenceError::Invalid(_))
    ));
    assert_eq!(store.load().unwrap().meta.active_finding, Some(key));
}

#[test]
fn finding_identity_ignores_summary_but_separates_codes() {
    let (_directory, store) = create();
    let (key, value) = finding(block(1, 0x11));
    let mut reworded = value.clone();
    reworded.summary = "new display wording".into();
    store
        .record_divergence(&current(&store), key, value, key.zone)
        .unwrap();
    store
        .record_divergence(&current(&store), key, reworded, key.zone)
        .unwrap();

    let mut other_key = key;
    other_key.code += 1;
    let mut other = finding(block(1, 0x11)).1;
    other.details.code = other_key.code;
    store
        .record_divergence(&current(&store), other_key, other, other_key.zone)
        .unwrap();
    assert_eq!(store.load().unwrap().meta.active_finding, Some(other_key));
}

#[test]
fn finding_rejects_forged_evidence_and_wrong_coordinates() {
    let (_directory, store) = create();
    let (key, value) = finding(block(1, 0x11));
    let mut forged = value.clone();
    forged.evidence_len += 1;
    assert!(
        store
            .record_divergence(&current(&store), key, forged, key.zone)
            .is_err()
    );
    let mut forged = value.clone();
    forged.evidence_digest = B256::ZERO;
    assert!(
        store
            .record_divergence(&current(&store), key, forged, key.zone)
            .is_err()
    );
    let (wrong_key, wrong) = finding(block(2, 0x12));
    assert!(
        store
            .record_divergence(&current(&store), wrong_key, wrong, wrong_key.zone)
            .is_err()
    );
    let mut wrong_parent = value;
    wrong_parent.parent.hash = B256::ZERO;
    assert!(
        store
            .record_divergence(&current(&store), key, wrong_parent, key.zone)
            .is_err()
    );
}

#[test]
fn finding_retains_typed_state_location_and_multi_block_tempo_coordinate() {
    let (_directory, store) = create();
    let zone = block(1, 0x11);
    let token = Address::repeat_byte(0x44);
    let (key, value) = super::super::make_finding(
        zone,
        bootstrap().zone,
        Some((block(3, 0x23), bootstrap().tempo)),
        FindingDetails {
            category: FindingCategory::StateMismatch,
            code: 12,
            location: Some(FindingLocation::State(crate::kernel::StateKey::Token(
                token,
            ))),
            expected: Some(Datum::Address(token)),
            actual: None,
        },
        "typed state evidence".into(),
    )
    .unwrap();
    let prior = store.load().unwrap();
    store
        .record_divergence(&prior, key, value, key.zone)
        .unwrap();

    let tx = store.db.tx().unwrap();
    let persisted = tx.get::<Findings>(key).unwrap().unwrap();
    assert_eq!(
        persisted.details.location,
        Some(FindingLocation::State(crate::kernel::StateKey::Token(
            token
        )))
    );
}

#[test]
fn stale_orphan_cannot_be_installed_as_active_finding() {
    let (_directory, store) = create();
    let old = block(1, 0x41);
    let (key, value) = finding(old);
    store
        .record_divergence(&current(&store), key, value, old)
        .unwrap();
    store.reorg(&current(&store), bootstrap().zone).unwrap();
    let replacement = apply(&store, 1, bootstrap().zone);
    assert_ne!(replacement, old);

    let mut meta = store.load().unwrap().meta;
    meta.active_finding = Some(key);
    let tx = store.db.tx_mut().unwrap();
    tx.put::<Meta>(MetaKey::Metadata, MetaValue::Metadata(Box::new(meta)))
        .unwrap();
    tx.commit().unwrap();
    assert!(matches!(store.load(), Err(PersistenceError::Invalid(_))));
}

#[test]
fn repeated_divergence_preserves_the_farthest_observed_tip() {
    let (directory, store) = create();
    let first = block(1, 0x11);
    let through = block(3, 0x13);
    let (key, value) = finding(first);
    store
        .record_divergence(&current(&store), key, value.clone(), through)
        .unwrap();
    let snapshot = store
        .record_divergence(&current(&store), key, value, first)
        .unwrap();
    assert_eq!(snapshot.meta.observed_zone_tip, through);
    assert_eq!(
        snapshot.meta.coverage,
        Coverage::Gap {
            first_unchecked: first,
            observed_through: through,
        }
    );

    drop(store);
    let (_, reopened) = Persistence::open(directory.path(), identity()).unwrap();
    assert_eq!(reopened.meta.coverage, snapshot.meta.coverage);
}
