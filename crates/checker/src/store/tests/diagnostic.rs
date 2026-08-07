use reth_db::cursor::DbCursorRO;
use reth_db_api::table::Table;

use super::*;
use crate::diagnostic::{DiagnosticModelKey, diagnose_retained_model_change};

type RawRows = Vec<(Vec<u8>, Vec<u8>)>;

#[test]
fn retained_key_diagnostic_is_exact_and_leaves_every_table_byte_unchanged() {
    let (directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    let token = Address::repeat_byte(0xa1);
    let key = ModelKey::Token(token);
    let first = token_value(1);
    let second = token_value(2);

    store
        .apply_block(block(
            initialization.verified_zone_tip,
            initialization.imported_tempo_tip,
            0xa2,
            0xb2,
            vec![ModelMutation::put(key, first.clone()).unwrap()],
        ))
        .unwrap();
    store
        .apply_block(block(
            tip(1, 0xa2),
            tip(11, 0xb2),
            0xa3,
            0xb3,
            vec![ModelMutation::put(key, second.clone()).unwrap()],
        ))
        .unwrap();
    store
        .apply_block(block(
            tip(2, 0xa3),
            tip(12, 0xb3),
            0xa4,
            0xb4,
            vec![ModelMutation::delete(key)],
        ))
        .unwrap();

    let database_path = store.path().to_path_buf();
    let expected_bytes = database_bytes(&store);
    drop(store);

    let report =
        diagnose_retained_model_change(&database_path, 2, DiagnosticModelKey::Token(token))
            .unwrap();
    assert_eq!(report.zone_before, tip(1, 0xa2));
    assert_eq!(report.zone_after, tip(2, 0xa3));
    assert_eq!(report.tempo_before, tip(11, 0xb2));
    assert_eq!(report.tempo_after, tip(12, 0xb3));
    assert_eq!(report.changeset_ordinal, Some(1));
    assert_eq!(
        report.before.unwrap().encoded,
        format!("0x{}", alloy_primitives::hex::encode(first.compress()))
    );
    assert_eq!(
        report.after.unwrap().encoded,
        format!("0x{}", alloy_primitives::hex::encode(second.compress()))
    );

    let reopened = CheckerStore::open_existing_at(&database_path, initialization.identity).unwrap();
    assert_eq!(database_bytes(&reopened), expected_bytes);
    assert_eq!(
        reopened.load_current().unwrap().verified_zone_tip,
        tip(3, 0xa4)
    );
    drop(reopened);
    assert!(directory.path().exists());
}

#[test]
fn diagnostic_rejects_unretained_and_parentless_targets_without_creating_state() {
    let (directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    store
        .apply_block(block(
            initialization.verified_zone_tip,
            initialization.imported_tempo_tip,
            0xc1,
            0xd1,
            Vec::new(),
        ))
        .unwrap();
    let database_path = store.path().to_path_buf();
    let expected_bytes = database_bytes(&store);
    drop(store);

    let key = DiagnosticModelKey::PortalConfig;
    let unchanged = diagnose_retained_model_change(&database_path, 1, key).unwrap();
    assert_eq!(unchanged.zone_before, initialization.verified_zone_tip);
    assert_eq!(unchanged.zone_after, tip(1, 0xc1));
    assert_eq!(unchanged.tempo_before, initialization.imported_tempo_tip);
    assert_eq!(unchanged.tempo_after, tip(11, 0xd1));
    assert_eq!(unchanged.changeset_ordinal, None);
    assert_eq!(unchanged.before, unchanged.after);

    let future = diagnose_retained_model_change(&database_path, 2, key).unwrap_err();
    assert!(
        future
            .to_string()
            .contains("above current verified Zone height 1")
    );
    let genesis = diagnose_retained_model_change(&database_path, 0, key).unwrap_err();
    assert!(
        genesis
            .to_string()
            .contains("has no preceding Zone block boundary")
    );

    let reopened = CheckerStore::open_existing_at(&database_path, initialization.identity).unwrap();
    assert_eq!(database_bytes(&reopened), expected_bytes);
    drop(reopened);

    let missing = directory.path().join("missing-checker");
    assert!(diagnose_retained_model_change(&missing, 1, key).is_err());
    assert!(!missing.exists());
}

#[test]
fn diagnostic_rejects_a_malformed_target_changeset_instead_of_returning_partial_state() {
    let (_directory, initialization, store) = open_test_store(BootstrapPhase::ZoneReplay);
    let token = Address::repeat_byte(0xe1);
    store
        .apply_block(block(
            initialization.verified_zone_tip,
            initialization.imported_tempo_tip,
            0xe2,
            0xf2,
            vec![ModelMutation::put(ModelKey::Token(token), token_value(1)).unwrap()],
        ))
        .unwrap();
    let tx = store.database().tx_mut().unwrap();
    tx.delete::<CheckerChangesets>(ChangesetKey::new(1, hash(0xe2), 1), None)
        .unwrap();
    tx.commit().unwrap();
    let database_path = store.path().to_path_buf();
    drop(store);

    let error = diagnose_retained_model_change(database_path, 1, DiagnosticModelKey::Token(token))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("changeset mutation row is missing")
    );
}

fn database_bytes(store: &CheckerStore) -> Vec<(&'static str, RawRows)> {
    let tx = store.database().tx().unwrap();
    let image = vec![
        raw_rows::<CheckerMeta, _>(&tx),
        raw_rows::<CheckerCanonical, _>(&tx),
        raw_rows::<CheckerModelState, _>(&tx),
        raw_rows::<CheckerChangesets, _>(&tx),
        raw_rows::<CheckerFindings, _>(&tx),
    ];
    tx.commit().unwrap();
    image
}

fn raw_rows<T: Table, TX: DbTx>(tx: &TX) -> (&'static str, RawRows) {
    let mut cursor = tx.cursor_read::<RawTable<T>>().unwrap();
    let rows = cursor
        .walk(None)
        .unwrap()
        .map(|row| {
            let (key, value) = row.unwrap();
            (key.into_key(), value.into_value())
        })
        .collect();
    (T::NAME, rows)
}
