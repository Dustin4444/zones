//! Findings tests.

use super::*;

#[test]
fn same_height_finding_is_idempotent_but_conflicting_evidence_is_rejected() {
    let (_directory, store) = create();
    let (key, value) = finding(block(1, 0x11));
    store
        .record_finding(&current(&store), key, value.clone())
        .unwrap();
    store
        .record_finding(&current(&store), key, value.clone())
        .unwrap();

    let mut conflicting = value;
    conflicting.details.actual = Some(Datum::Code(5));
    assert!(matches!(
        store.record_finding(&current(&store), key, conflicting),
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
    store.record_finding(&current(&store), key, value).unwrap();
    store
        .record_finding(&current(&store), key, reworded)
        .unwrap();

    let mut other_key = key;
    other_key.code += 1;
    let mut other = finding(block(1, 0x11)).1;
    other.details.code = other_key.code;
    store
        .record_finding(&current(&store), other_key, other)
        .unwrap();
    assert_eq!(store.load().unwrap().meta.active_finding, Some(other_key));
}

#[test]
fn finding_rejects_forged_evidence_and_wrong_coordinates() {
    let (_directory, store) = create();
    let (key, value) = finding(block(1, 0x11));
    let mut forged = value.clone();
    forged.evidence_len += 1;
    assert!(store.record_finding(&current(&store), key, forged).is_err());
    let mut forged = value.clone();
    forged.evidence_digest = B256::ZERO;
    assert!(store.record_finding(&current(&store), key, forged).is_err());
    let (wrong_key, wrong) = finding(block(2, 0x12));
    assert!(
        store
            .record_finding(&current(&store), wrong_key, wrong)
            .is_err()
    );
    let mut wrong_parent = value;
    wrong_parent.parent.hash = B256::ZERO;
    assert!(
        store
            .record_finding(&current(&store), key, wrong_parent)
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
    store.record_finding(&prior, key, value).unwrap();

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
    store.record_finding(&current(&store), key, value).unwrap();
    store
        .record_gap(
            &current(&store),
            old,
            old,
            CoverageGapReason::NotCheckedAncestorDivergence,
        )
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
