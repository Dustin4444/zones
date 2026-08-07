use std::{collections::BTreeMap, sync::Arc};

use alloy_consensus::Header;
use alloy_eips::BlockNumHash;
use alloy_primitives::{B256, Bytes};
use reth_execution_types::{Chain, ExecutionOutcome};
use reth_exex::ExExNotification;
use reth_primitives_traits::{RecoveredBlock, SealedBlock};
use tempo_primitives::{Block, TempoHeader, TempoPrimitives, TempoReceipt};

use super::{RetainedNotificationDriver, RuntimeExit};

#[test]
fn terminal_handoff_keeps_current_ack_before_the_buffered_fifo() {
    let first = single_block_chain(0x11);
    let second = single_block_chain(0x22);
    let mut driver = RetainedNotificationDriver::new();
    driver.retain(ExExNotification::ChainCommitted { new: first.clone() });
    driver.retain(ExExNotification::ChainCommitted {
        new: second.clone(),
    });
    let current = BlockNumHash::new(9, B256::repeat_byte(0x90));

    let mut exit = RuntimeExit::with_ack_and_driver(eyre::eyre!("terminal"), current, driver);

    assert_eq!(exit.acknowledgement_for_test(), Some(current));
    assert!(Arc::ptr_eq(&pop_committed(&mut exit), &first));
    assert!(Arc::ptr_eq(&pop_committed(&mut exit), &second));
    assert!(exit.pop_buffered_for_test().is_none());
}

fn single_block_chain(marker: u8) -> Arc<Chain<TempoPrimitives>> {
    let block = Block {
        header: TempoHeader {
            inner: Header {
                number: 1,
                extra_data: Bytes::from(vec![marker]),
                ..Default::default()
            },
            ..Default::default()
        },
        body: Default::default(),
    };
    let block = RecoveredBlock::new_sealed(SealedBlock::seal_slow(block), Vec::new());
    let outcome = ExecutionOutcome::<TempoReceipt>::new(
        Default::default(),
        vec![Vec::new()],
        1,
        Default::default(),
    );
    Arc::new(Chain::new(vec![block], outcome, BTreeMap::new()))
}

fn pop_committed(exit: &mut RuntimeExit) -> Arc<Chain<TempoPrimitives>> {
    let Some(ExExNotification::ChainCommitted { new }) = exit.pop_buffered_for_test() else {
        panic!("terminal FIFO did not retain a committed notification");
    };
    new
}
