use std::{num::NonZeroU64, path::Path};

use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256, U256};
use alloy_provider::DynProvider;
use tempo_alloy::TempoNetwork;

use crate::{
    CheckerConfig,
    model::{
        constants::ZONE_FACTORY_ADDRESS,
        events::Portal,
        ownership::DepositId,
        state::{PortalIdentity, TokenPhase, portal_address_for_zone},
    },
    observe::observe_l1,
    runtime::{PersistentChecker, state::ReadyToAcknowledge},
    store::{
        db::{CheckerStore, FreshBootstrap, Initialization, StoreSnapshot},
        operations::WriteOutcome,
        value::{BootstrapState, StoreIdentity},
    },
};

use self::l1::{creation_logs, ordinary_deposit, ordinary_queue_hash, protocol_log};
pub(super) use self::{
    rpc::RpcScript,
    zone::{
        zone_provider, zone_provider_with_genesis_progress, zone_provider_with_genesis_supply,
        zone_provider_with_initial_token,
    },
};
use rpc::AuthenticatedBlock;

const ZONE_ID: u32 = 7;
pub(super) const L1_CHAIN_ID: u64 = 31337;
pub(super) const INITIAL_TOKEN: Address = Address::repeat_byte(0x20);
const CONFIG_GAS: u64 = 77;
pub(super) const DEPOSIT_AMOUNT: u128 = 700;

#[derive(Debug, Clone, Copy)]
pub(super) enum L1ReplayCheckpoint {
    Initialized,
    Creation,
    Anchor,
}

mod l1;
mod rpc;
mod zone;

pub(super) fn assert_pre_genesis_snapshot(
    checker: &PersistentChecker,
    fixture: &PreGenesisFixture,
) -> StoreSnapshot {
    let snapshot = checker.current_snapshot_for_test();
    assert_eq!(
        snapshot.verified_zone_tip,
        BlockNumHash::new(0, fixture.zone_genesis)
    );
    assert_eq!(snapshot.imported_tempo_tip, fixture.anchor.tip());
    assert_eq!(
        snapshot.bootstrap,
        BootstrapState::zone_replay(fixture.anchor.tip())
    );
    let portal = snapshot.model.portal().created().unwrap();
    assert_eq!(portal.identity(), fixture.portal_identity());
    assert_eq!(portal.config().bounceback_gas(), CONFIG_GAS);
    assert_eq!(portal.deposit_cursor().hash(), fixture.deposit_queue_hash);
    assert_eq!(portal.deposit_cursor().number(), 1);
    let deposit_id = DepositId {
        portal: fixture.portal,
        deposit_number: NonZeroU64::new(1).unwrap(),
    };
    assert!(snapshot.model.pending_deposit(deposit_id).is_some());
    let token = snapshot.model.token(fixture.initial_token).unwrap();
    assert_eq!(token.phase(), TokenPhase::ZoneEnabled);
    assert_eq!(
        token.accounting().deposit_liability,
        U256::from(DEPOSIT_AMOUNT)
    );
    snapshot
}

pub(super) async fn persist_l1_prefix(
    fixture: &PreGenesisFixture,
    path: &Path,
    checkpoint: L1ReplayCheckpoint,
) {
    let initialization = Initialization::fresh(
        fixture.identity(),
        FreshBootstrap::L1Replay {
            creation_parent: fixture.creation_parent(),
        },
    );
    let store = CheckerStore::create_fresh_at(path, initialization).unwrap();
    let mut checker = PersistentChecker::from_bootstrap_store(store).unwrap();
    if matches!(checkpoint, L1ReplayCheckpoint::Initialized) {
        return;
    }

    let script = RpcScript::new();
    script.push_observation(&fixture.creation);
    script.push_balance(U256::ZERO);
    if matches!(checkpoint, L1ReplayCheckpoint::Anchor) {
        script.push_observation(&fixture.anchor);
        script.push_balance(U256::from(DEPOSIT_AMOUNT));
    }
    let provider = script.provider();
    replay_one_l1_block(&mut checker, &provider, fixture.portal, &fixture.creation).await;
    if matches!(checkpoint, L1ReplayCheckpoint::Anchor) {
        replay_one_l1_block(&mut checker, &provider, fixture.portal, &fixture.anchor).await;
    }
    script.assert_consumed();
}

async fn replay_one_l1_block(
    checker: &mut PersistentChecker,
    provider: &DynProvider<TempoNetwork>,
    portal: Address,
    block: &AuthenticatedBlock,
) {
    let observation = observe_l1(provider, &block.header, portal).await.unwrap();
    let prepared = checker
        .mirror
        .prepare_imported_bootstrap(provider, &observation, &block.header)
        .await
        .unwrap();
    let current = checker.store.load_current().unwrap();
    let commit = checker
        .store
        .bootstrap_l1_commit(
            current.bootstrap,
            prepared.parent_tempo_tip(),
            prepared.child_tempo_tip(),
            prepared.state_update(),
        )
        .unwrap();
    assert_eq!(
        checker.store.apply_bootstrap(commit).unwrap(),
        WriteOutcome::Applied
    );
    checker.mirror.apply_prepared_imported(prepared);
}

pub(super) async fn apply_first_post_genesis_deposit(
    checker: &mut PersistentChecker,
    fixture: &PreGenesisFixture,
) -> ReadyToAcknowledge {
    let (imported, block, receipts, exact_state) = fixture.first_post_genesis_block();
    let script = RpcScript::new();
    script.push_observation(&imported);
    script.push_balance(U256::from(DEPOSIT_AMOUNT));
    let provider = script.provider();
    let prepared = checker
        .prepare_block(&provider, &exact_state, &block, &receipts)
        .await
        .unwrap();
    let durable = checker.commit_block(prepared).unwrap();
    let ready = checker.adopt_block(durable);
    script.assert_consumed();
    ready
}

pub(super) struct PreGenesisFixture {
    pub(super) zone_chain_id: u64,
    pub(super) zone_genesis: B256,
    pub(super) portal: Address,
    pub(super) initial_token: Address,
    pub(super) creation: AuthenticatedBlock,
    pub(super) anchor: AuthenticatedBlock,
    deposit_queue_hash: B256,
}

impl PreGenesisFixture {
    pub(super) fn new() -> Self {
        let portal = portal_address_for_zone(ZONE_ID);
        let initial_token = INITIAL_TOKEN;
        let creation = AuthenticatedBlock::new(
            10,
            B256::repeat_byte(0x09),
            creation_logs(portal, initial_token),
        );
        let deposit = ordinary_deposit(initial_token);
        let deposit_queue_hash = ordinary_queue_hash(&deposit);
        let anchor = AuthenticatedBlock::new(
            11,
            creation.tip().hash,
            vec![
                protocol_log(
                    portal,
                    Portal::BouncebackGasUpdated {
                        bouncebackGas: CONFIG_GAS,
                    },
                ),
                protocol_log(
                    portal,
                    Portal::DepositMade {
                        newCurrentDepositQueueHash: deposit_queue_hash,
                        sender: deposit.sender,
                        token: deposit.token,
                        netAmount: deposit.amount,
                        fee: 0,
                        keyIndex: deposit.keyIndex,
                        ephemeralPubkeyX: deposit.encrypted.ephemeralPubkeyX,
                        ephemeralPubkeyYParity: deposit.encrypted.ephemeralPubkeyYParity,
                        ciphertext: deposit.encrypted.ciphertext.clone(),
                        nonce: deposit.encrypted.nonce,
                        tag: deposit.encrypted.tag,
                        tempoRefundRecipient: deposit.tempoRefundRecipient,
                        depositNumber: 1,
                    },
                ),
            ],
        );
        Self {
            zone_chain_id: zone_primitives::constants::zone_chain_id(ZONE_ID),
            zone_genesis: B256::repeat_byte(0x70),
            portal,
            initial_token,
            creation,
            anchor,
            deposit_queue_hash,
        }
    }

    pub(super) fn config(&self, database_path: Option<std::path::PathBuf>) -> CheckerConfig {
        CheckerConfig {
            l1_rpc_url: "mock://unused".into(),
            portal_address: self.portal,
            portal_creation_block_hash: self.creation.tip().hash,
            zone_id: ZONE_ID,
            database_path,
        }
    }

    fn portal_identity(&self) -> PortalIdentity {
        PortalIdentity::new(self.portal, ZONE_ID, self.initial_token)
    }

    pub(super) fn identity(&self) -> StoreIdentity {
        StoreIdentity::new(
            self.zone_chain_id,
            self.zone_genesis,
            self.portal_identity(),
            L1_CHAIN_ID,
            ZONE_FACTORY_ADDRESS,
            self.creation.tip(),
        )
    }

    fn creation_parent(&self) -> BlockNumHash {
        BlockNumHash::new(
            self.creation.tip().number - 1,
            self.creation.header.header().inner.parent_hash,
        )
    }
}

pub(super) struct DevelopmentFixture {
    pub(super) zone_chain_id: u64,
    pub(super) zone_genesis: B256,
    portal: Address,
    initial_token: Address,
    pub(super) anchor: AuthenticatedBlock,
    pub(super) creation: AuthenticatedBlock,
}

impl DevelopmentFixture {
    pub(super) fn new() -> Self {
        let portal = portal_address_for_zone(ZONE_ID);
        let initial_token = INITIAL_TOKEN;
        let anchor = AuthenticatedBlock::new(10, B256::repeat_byte(0x09), Vec::new());
        let creation =
            AuthenticatedBlock::new(11, anchor.tip().hash, creation_logs(portal, initial_token));
        Self {
            zone_chain_id: zone_primitives::constants::zone_chain_id(ZONE_ID),
            zone_genesis: B256::repeat_byte(0x71),
            portal,
            initial_token,
            anchor,
            creation,
        }
    }

    pub(super) fn config(&self) -> CheckerConfig {
        CheckerConfig {
            l1_rpc_url: "mock://unused".into(),
            portal_address: self.portal,
            portal_creation_block_hash: self.creation.tip().hash,
            zone_id: ZONE_ID,
            database_path: None,
        }
    }

    pub(super) fn portal_identity(&self) -> PortalIdentity {
        PortalIdentity::new(self.portal, ZONE_ID, self.initial_token)
    }

    pub(super) fn identity(&self) -> StoreIdentity {
        StoreIdentity::new(
            self.zone_chain_id,
            self.zone_genesis,
            self.portal_identity(),
            L1_CHAIN_ID,
            ZONE_FACTORY_ADDRESS,
            self.creation.tip(),
        )
    }
}
