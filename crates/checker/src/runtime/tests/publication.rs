//! Publication tests.

use super::*;

#[test]
fn production_publication_rejects_existing_target_and_reopens() {
    let parent = tempfile::tempdir().unwrap();
    let occupied = parent.path().join("occupied");
    fs::create_dir(&occupied).unwrap();
    assert!(
        Persistence::create_atomic(&occupied, identity(), anchor(), State::awaiting(portal()),)
            .is_err()
    );

    let target = parent.path().join("checkpoint");
    let snapshot =
        Persistence::create_atomic(&target, identity(), anchor(), State::awaiting(portal()))
            .unwrap();
    let (_, reopened) = Persistence::open(&target, identity()).unwrap();
    assert_eq!(snapshot, reopened);
}

#[test]
fn failed_production_publication_removes_staging_directory() {
    let parent = tempfile::tempdir().unwrap();
    let target = parent.path().join("checkpoint");
    let mut wrong_identity = identity();
    wrong_identity.zone_id += 1;

    assert!(
        Persistence::create_atomic(&target, wrong_identity, anchor(), State::awaiting(portal()),)
            .is_err()
    );
    assert!(!target.exists());
    assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
}
