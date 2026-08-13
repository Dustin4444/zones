//! Coverage tests.

use super::*;

#[test]
fn observed_tip_marks_retained_history_for_recovery() {
    let (_directory, store) = create();
    let observed = block(3, 0x33);
    let snapshot = store
        .record_observed_tip(&current(&store), observed)
        .unwrap();
    assert_eq!(snapshot.observed_zone_tip, observed);
    assert_eq!(snapshot.coverage, Coverage::Recovering);
    assert!(matches!(
        store.record_observed_tip(&current(&store), block(0, 0x44)),
        Err(PersistenceError::Invalid(_))
    ));
}

#[test]
fn observed_tip_extends_an_active_divergence_gap() {
    let (_directory, store) = create();
    let first = block(1, 0x11);
    let through = block(3, 0x13);
    let (key, value) = finding(first);
    store
        .record_divergence(&current(&store), key, value, first)
        .unwrap();

    let meta = store
        .record_observed_tip(&current(&store), through)
        .unwrap();
    assert_eq!(meta.observed_zone_tip, through);
    assert_eq!(
        meta.coverage,
        Coverage::Gap {
            first_unchecked: first,
            observed_through: through,
        }
    );
}

#[test]
fn observed_tip_cannot_move_before_an_active_finding() {
    let (_directory, store) = create();
    let first = block(1, 0x11);
    let (key, value) = finding(first);
    store
        .record_divergence(&current(&store), key, value, block(3, 0x13))
        .unwrap();

    assert!(matches!(
        store.record_observed_tip(&current(&store), bootstrap().zone),
        Err(PersistenceError::Invalid(_))
    ));
}

#[test]
fn gap_requires_a_matching_active_finding() {
    let (_directory, store) = create();
    let first = block(1, 0x11);
    let mut meta = current(&store).meta;
    meta.observed_zone_tip = first;
    meta.coverage = Coverage::Gap {
        first_unchecked: first,
        observed_through: first,
    };
    let tx = store.db.tx_mut().unwrap();
    tx.put::<Meta>(MetaKey::Metadata, MetaValue::Metadata(Box::new(meta)))
        .unwrap();
    tx.commit().unwrap();

    assert!(matches!(store.load(), Err(PersistenceError::Invalid(_))));
}
