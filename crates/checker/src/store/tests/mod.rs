use std::{
    num::NonZeroU64,
    panic::{AssertUnwindSafe, catch_unwind},
};

use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256, FixedBytes, U256};
use reth_codecs::{Compress, Decompress};
use reth_db::{
    Database,
    transaction::{DbTx, DbTxMut},
};
use reth_db_api::tables::{RawKey, RawTable, RawValue};
use tempfile::TempDir;

use crate::model::{
    accounting::TokenAccounting,
    encoding::{CompressedYParity, DepositPayload, DepositQueueMember, OrdinaryDeposit},
    ownership::{DepositId, DepositOwner},
    state::{ModelState, PortalDepositCursor, PortalIdentity, TokenPhase, TokenState},
    transition::{
        ModelTransition,
        test_inputs::{
            AuthenticatedDepositOutcome, ImportedTempoOperation, deposit_prefix, imported_block,
            zone_block,
        },
    },
};

use super::{
    db::{CheckerStore, Initialization, LiveBlock, RefundCredit},
    error::StoreError,
    history::BlockCommit,
    model_state::{flatten_model, model_bytes},
    operations::{ModelMutation, WriteOutcome},
    schema::{
        CanonicalHash, ChangesetKey, CheckerCanonical, CheckerChangesets, CheckerFindings,
        CheckerMeta, CheckerModelState, FindingKey, MetaKey, ModelKey,
    },
    value::{
        BatchBoundaryValue, BatchMembersValue, BatchValue, BeforeImage, BootstrapState,
        CursorValue, FinalizedBatchValue, FindingKind, FindingRecord, FindingStatus, MetaValue,
        ModelValue, PortalSettlementValue, StoreIdentity, StoredTokenPhase, TokenValue,
        ZoneBatchAccumulatorValue,
    },
};

mod bootstrap;
mod findings;
mod history;
mod opening;
mod preflight;
mod refunds;
mod unwind;

#[derive(Debug, Clone, Copy)]
enum BootstrapPhase {
    L1Replay,
    ZoneReplay,
    Live,
}

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

fn tip(number: u64, byte: u8) -> BlockNumHash {
    BlockNumHash::new(number, hash(byte))
}

fn identity() -> StoreIdentity {
    identity_with_portal(Address::repeat_byte(0x40))
}

fn identity_with_portal(portal: Address) -> StoreIdentity {
    StoreIdentity::new(
        4242,
        hash(0x10),
        PortalIdentity::new(portal, 7, Address::repeat_byte(0x20)),
        31337,
        Address::repeat_byte(0x30),
        hash(0x50),
    )
}

fn initialization(phase: BootstrapPhase) -> Initialization {
    let identity = identity();
    let tempo = tip(10, 0x60);
    let bootstrap = match phase {
        BootstrapPhase::L1Replay => BootstrapState::l1_replay(Some(tempo)),
        BootstrapPhase::ZoneReplay => BootstrapState::zone_replay(tempo),
        BootstrapPhase::Live => BootstrapState::live(),
    };
    Initialization::new(
        identity,
        bootstrap,
        BlockNumHash::new(0, identity.zone_genesis_hash()),
        tempo,
        ModelState::created_with_zone_token_for_test(
            identity.portal_identity(),
            TokenAccounting::ZERO,
        ),
    )
}

fn initialization_with_pending_token(phase: BootstrapPhase) -> (Initialization, Address) {
    let mut initialization = initialization(phase);
    let token = Address::repeat_byte(0x21);
    let owner = DepositOwner::PendingOrdinary {
        preimage: OrdinaryDeposit::new(
            token,
            Address::repeat_byte(0x22),
            9,
            Address::repeat_byte(0x23),
            U256::from(24),
            DepositPayload::new(
                hash(0x25),
                CompressedYParity::Odd,
                FixedBytes::repeat_byte(0x26),
                FixedBytes::repeat_byte(0x27),
                FixedBytes::repeat_byte(0x28),
            ),
        ),
    };
    let cursor_hash = owner.queue_member().hash_after(B256::ZERO);

    initialization.model.seed_token_for_test(
        token,
        TokenState::for_test(
            TokenPhase::PendingZoneEnable,
            TokenAccounting {
                supply: U256::ZERO,
                deposit_liability: U256::from(9),
                withdrawal_liability: U256::ZERO,
            },
        ),
    );
    initialization.model.seed_pending_deposit_for_test(
        DepositId {
            portal: initialization.identity.portal_identity().portal(),
            deposit_number: NonZeroU64::new(1).unwrap(),
        },
        owner,
    );
    initialization
        .model
        .set_portal_deposit_cursor_for_test(PortalDepositCursor::new(cursor_hash, 1));
    (initialization, token)
}

fn open_test_store(phase: BootstrapPhase) -> (TempDir, Initialization, CheckerStore) {
    let directory = TempDir::new().unwrap();
    let initialization = initialization(phase);
    let store = CheckerStore::open(directory.path(), initialization.clone()).unwrap();
    (directory, initialization, store)
}

fn token_value(amount: u64) -> ModelValue {
    token_value_with_liabilities(amount, 0, 0)
}

fn token_value_with_liabilities(supply: u64, deposit: u128, withdrawal: u128) -> ModelValue {
    ModelValue::Token(TokenValue {
        phase: StoredTokenPhase::ZoneEnabled,
        supply: U256::from(supply),
        deposit_liability: U256::from(deposit),
        withdrawal_liability: U256::from(withdrawal),
    })
}

fn terminal_settlement_rows(
    tempo_height: u64,
    zone_height: u64,
    zone_hash: B256,
) -> [(ModelKey, ModelValue); 2] {
    let zero_cursor = CursorValue {
        hash: B256::ZERO,
        number: 0,
    };
    [
        (
            ModelKey::PortalSettlement,
            ModelValue::PortalSettlement(PortalSettlementValue {
                withdrawal_batch_index: 1,
                block_hash: zone_hash,
                last_synced_tempo_block_number: tempo_height,
                last_submitted_deposit_cursor: zero_cursor,
                zone_height: U256::from(zone_height),
                withdrawal_queue_head: U256::ZERO,
                withdrawal_queue_tail: U256::ZERO,
            }),
        ),
        (
            ModelKey::ZoneBatchAccumulator,
            ModelValue::ZoneBatchAccumulator(ZoneBatchAccumulatorValue {
                last_withdrawal_queue_hash: B256::ZERO,
                last_withdrawal_batch_index: 1,
                first_zone_parent_hash: zone_hash,
                first_processed_deposit: zero_cursor,
                first_withdrawal_index: 0,
            }),
        ),
    ]
}

fn block(
    zone_parent: BlockNumHash,
    tempo_parent: BlockNumHash,
    zone_byte: u8,
    tempo_byte: u8,
    mutations: Vec<ModelMutation>,
) -> BlockCommit {
    BlockCommit::from_mutations(
        zone_parent,
        tempo_parent,
        tip(zone_parent.number + 1, zone_byte),
        tip(tempo_parent.number + 1, tempo_byte),
        mutations,
    )
    .unwrap()
}
