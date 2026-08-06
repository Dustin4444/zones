use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256, U256};
use reth_exex::ExExNotification;
use reth_storage_api::{StateProviderBox, errors::provider::ProviderResult};
use tempfile::TempDir;
use tempo_primitives::{Block, TempoHeader, TempoPrimitives, TempoReceipt};

use super::{
    L1_NUMBER, TestProvider, UnavailableZoneState, chain, exact_zone_state_with_supply,
    imported_child_header, l1_provider_with_collateral, l1_provider_with_collateral_sequence,
    user_withdrawal_receipt, zone_block, zone_block_with_marker, zone_block_with_user_withdrawal,
    zone_block_with_user_withdrawal_marker, zone_receipt,
};
use crate::{
    check::pipeline::PreparedBlock,
    model::{
        accounting::TokenAccounting,
        state::{ModelState, PortalIdentity, portal_address_for_zone},
    },
    observe::ExactStateLookup,
    runtime::{L1Client, LiveChecker, RuntimeError},
    store::{
        db::{CheckerStore, Initialization},
        error::StoreError,
        value::{BootstrapState, StoreIdentity},
    },
};

mod alert;
mod atomicity;
mod loop_retry;
mod replay;
mod startup;

const ZONE_ID: u32 = 7;
const INITIAL_SUPPLY: u64 = 100_000;
const POST_WITHDRAWAL_SUPPLY: u64 = 49_990;

struct LiveFixture {
    directory: TempDir,
    initialization: Initialization,
    token: Address,
    imported: TempoHeader,
    block: reth_primitives_traits::RecoveredBlock<Block>,
    receipts: Vec<TempoReceipt>,
    checker: LiveChecker,
}

impl LiveFixture {
    fn new() -> Self {
        let directory = TempDir::new().unwrap();
        let initialization = live_initialization();
        let token = Address::repeat_byte(0x20);
        let imported = imported_child_header(L1_NUMBER, B256::repeat_byte(0x90));
        let sender = Address::repeat_byte(0x53);
        let block = zone_block_with_user_withdrawal(
            1,
            initialization.verified_zone_tip.hash,
            &imported,
            sender,
        );
        let receipts = vec![
            zone_receipt(&imported),
            user_withdrawal_receipt(sender, token),
        ];

        let store = CheckerStore::open(directory.path(), initialization.clone()).unwrap();
        drop(store);
        let store = CheckerStore::open_existing(directory.path(), initialization.identity).unwrap();
        let checker = LiveChecker::from_store(store).unwrap();
        Self {
            directory,
            initialization,
            token,
            imported,
            block,
            receipts,
            checker,
        }
    }

    async fn prepare(&self) -> PreparedBlock {
        let provider = l1_provider_with_collateral(&self.imported, U256::from(INITIAL_SUPPLY));
        let zone_state = exact_zone_state_with_supply(
            &self.imported,
            self.token,
            U256::from(POST_WITHDRAWAL_SUPPLY),
        );
        self.checker
            .prepare_block(&provider, &zone_state, &self.block, &self.receipts)
            .await
            .unwrap()
    }

    fn notification(&self) -> ExExNotification<TempoPrimitives> {
        ExExNotification::ChainCommitted {
            new: chain(vec![self.block.clone()], vec![self.receipts.clone()]),
        }
    }
}

fn live_initialization() -> Initialization {
    let zone_parent = B256::repeat_byte(0x91);
    let tempo_parent = B256::repeat_byte(0x90);
    let token = Address::repeat_byte(0x20);
    let portal_identity = PortalIdentity::new(portal_address_for_zone(ZONE_ID), ZONE_ID, token);
    let identity = StoreIdentity::new(
        4242,
        zone_parent,
        portal_identity,
        31337,
        Address::repeat_byte(0x30),
        B256::repeat_byte(0xcc),
    );
    Initialization::new(
        identity,
        BootstrapState::live(),
        BlockNumHash::new(0, zone_parent),
        BlockNumHash::new(L1_NUMBER - 1, tempo_parent),
        ModelState::created_with_zone_token_for_test(
            portal_identity,
            TokenAccounting {
                supply: U256::from(INITIAL_SUPPLY),
                deposit_liability: U256::ZERO,
                withdrawal_liability: U256::ZERO,
            },
        ),
    )
}

struct TwoBlockState {
    first_hash: B256,
    first: TestProvider,
    second_hash: B256,
    second: TestProvider,
}

impl ExactStateLookup for TwoBlockState {
    fn state_by_exact_block_hash(&self, block_hash: B256) -> ProviderResult<StateProviderBox> {
        if block_hash == self.first_hash {
            return self.first.state_by_exact_block_hash(block_hash);
        }
        if block_hash == self.second_hash {
            return self.second.state_by_exact_block_hash(block_hash);
        }
        Err(reth_storage_api::errors::provider::ProviderError::StateForHashNotFound(block_hash))
    }
}
