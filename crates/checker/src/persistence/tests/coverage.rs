//! Coverage tests.

use super::*;

#[test]
fn gap_is_durable_before_acknowledgement_advances() {
    let (directory, store) = create();
    let first = block(1, 0x31);
    let through = block(4, 0x34);
    store
        .record_gap(
            &current(&store),
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
        .record_gap(&current(&store), one, three, reason.clone())
        .unwrap();
    assert!(matches!(
        store.apply(
            &current(&store),
            entry(1, bootstrap().zone),
            one,
            Coverage::Complete
        ),
        Err(PersistenceError::Invalid(_))
    ));
    store
        .apply(
            &current(&store),
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
            &current(&store),
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
        .apply(&current(&store), entry(3, two), three, Coverage::Complete)
        .unwrap();
    assert_eq!(snapshot.meta.verified_zone_tip, three);
    assert_eq!(snapshot.meta.acknowledged_zone_tip, three);
    assert_eq!(snapshot.meta.coverage, Coverage::Complete);
}
