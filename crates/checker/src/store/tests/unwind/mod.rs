use super::*;
use crate::store::{error::ParentTips, value::BlockBeforeImage};
use reth_db::cursor::DbCursorRO;

mod atomicity;
mod corruption;
mod restoration;

type DurableHistory = (Vec<(u64, B256)>, Vec<(ChangesetKey, BeforeImage)>);

fn apply_token_child(
    store: &CheckerStore,
    zone_parent: BlockNumHash,
    tempo_parent: BlockNumHash,
    zone_byte: u8,
    tempo_byte: u8,
    token: Address,
    amount: u64,
) -> (BlockNumHash, BlockNumHash) {
    let zone_child = tip(zone_parent.number + 1, zone_byte);
    let tempo_child = tip(tempo_parent.number + 1, tempo_byte);
    let commit = block(
        zone_parent,
        tempo_parent,
        zone_byte,
        tempo_byte,
        vec![ModelMutation::put(ModelKey::Token(token), token_value(amount)).unwrap()],
    );
    assert_eq!(store.apply_block(commit).unwrap(), WriteOutcome::Applied);
    (zone_child, tempo_child)
}

fn assert_child_journal(store: &CheckerStore, child: BlockNumHash, rows: usize) {
    let tx = store.database().tx().unwrap();
    assert_eq!(
        tx.get::<CheckerCanonical>(child.number)
            .unwrap()
            .unwrap()
            .into_inner(),
        child.hash
    );
    for ordinal in 0..u32::try_from(rows).unwrap() {
        assert!(
            tx.get::<CheckerChangesets>(ChangesetKey::new(child.number, child.hash, ordinal))
                .unwrap()
                .is_some()
        );
    }
    tx.commit().unwrap();
}

fn durable_history(store: &CheckerStore) -> DurableHistory {
    let tx = store.database().tx().unwrap();
    let mut canonical = tx.cursor_read::<CheckerCanonical>().unwrap();
    let canonical = canonical
        .walk(None)
        .unwrap()
        .map(|row| {
            let (height, hash) = row.unwrap();
            (height, hash.into_inner())
        })
        .collect();
    let mut changesets = tx.cursor_read::<CheckerChangesets>().unwrap();
    let changesets = changesets
        .walk(None)
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    tx.commit().unwrap();
    (canonical, changesets)
}
