use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use alloy_consensus::Header;
use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256, Bytes, U256};
use reth_execution_types::{Chain, ExecutionOutcome};
use reth_exex::ExExNotification;
use reth_primitives_traits::{RecoveredBlock, SealedBlock};
use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
use tempfile::TempDir;
use tempo_primitives::{Block, TempoHeader, TempoPrimitives};

use super::{
    DrainedNotifications, drain_one, open_durable_cut_once, reconcile_durable_cut, startup_exit,
};
use crate::{
    CheckerConfig,
    model::{
        accounting::TokenAccounting,
        state::{ModelState, PortalIdentity, portal_address_for_zone},
        state_layout::DEFAULT_FEE_TOKEN_ACCESS,
    },
    runtime::{RuntimeError, RuntimeStatus},
    store::{
        db::{CheckerStore, Initialization},
        schema::FindingKey,
        value::{BootstrapState, FindingKind, FindingRecord, FindingStatus, StoreIdentity},
    },
};

const ZONE_ID: u32 = 7;
type ZoneProvider = MockEthProvider<TempoPrimitives>;

#[tokio::test]
async fn durable_alert_opens_and_reconciles_without_l1() {
    let directory = TempDir::new().unwrap();
    let (config, zone_chain_id, database_path, canonical, finding) =
        durable_alert_fixture(&directory);
    let (mut checker, _disconnected_l1) =
        open_durable_cut_once(&config, zone_chain_id, &database_path, &canonical)
            .await
            .unwrap();
    let mut status = RuntimeStatus::new();

    let ready = reconcile_durable_cut(&mut checker, &canonical, &mut status)
        .await
        .unwrap();

    assert_eq!(ready.tip().number, 2);
    assert!(ready.is_alerting());
    assert!(status.is_alerting());
    assert_eq!(
        checker
            .current_snapshot_for_test()
            .active_alert
            .unwrap()
            .finding,
        finding
    );
}

#[test]
fn startup_failure_retains_the_last_exact_drained_acknowledgement() {
    let genesis = BlockNumHash::new(0, B256::repeat_byte(0x10));
    let old = notification_chain(1, genesis.hash, 0x21);
    let new = notification_chain(1, genesis.hash, 0x22);
    let old_tip = chain_tip(&old);
    let new_tip = chain_tip(&new);
    let mut drained = DrainedNotifications::default();

    drain_one(
        Ok(Some(ExExNotification::ChainCommitted { new: old.clone() })),
        &mut drained,
    )
    .unwrap();
    assert_eq!(drained.acknowledge, Some(old_tip));

    drain_one(
        Ok(Some(ExExNotification::ChainReorged {
            old,
            new: new.clone(),
        })),
        &mut drained,
    )
    .unwrap();
    assert_eq!(drained.acknowledge, Some(new_tip));

    drain_one(
        Ok(Some(ExExNotification::ChainReverted { old: new })),
        &mut drained,
    )
    .unwrap();
    assert_eq!(drained.acknowledge, Some(genesis));

    let exit = startup_exit(eyre::eyre!("preparation failed"), &drained);
    assert_eq!(exit.acknowledgement_for_test(), Some(genesis));
}

#[test]
fn raw_stream_closure_and_failure_preserve_prior_drained_progress() {
    let prior = BlockNumHash::new(7, B256::repeat_byte(0x70));
    let mut drained = DrainedNotifications {
        count: 7,
        acknowledge: Some(prior),
    };
    assert!(matches!(
        drain_one(Ok(None), &mut drained),
        Err(RuntimeError::NotificationStreamClosedDuringBootstrap)
    ));
    assert_eq!((drained.count, drained.acknowledge), (7, Some(prior)));

    assert!(matches!(
        drain_one(Err(eyre::eyre!("stream failed")), &mut drained),
        Err(RuntimeError::BootstrapNotificationStream { .. })
    ));
    assert_eq!((drained.count, drained.acknowledge), (7, Some(prior)));
}

fn durable_alert_fixture(
    directory: &TempDir,
) -> (CheckerConfig, u64, PathBuf, ZoneProvider, FindingKey) {
    let zone_chain_id = zone_primitives::constants::zone_chain_id(ZONE_ID);
    let genesis = BlockNumHash::new(0, B256::repeat_byte(0x10));
    let tempo = BlockNumHash::new(10, B256::repeat_byte(0x60));
    let portal = PortalIdentity::new(
        portal_address_for_zone(ZONE_ID),
        ZONE_ID,
        Address::repeat_byte(0x20),
    );
    let identity = StoreIdentity::new(
        zone_chain_id,
        genesis.hash,
        portal,
        31_337,
        crate::model::constants::ZONE_FACTORY_ADDRESS,
        BlockNumHash::new(1, B256::repeat_byte(0x50)),
    );
    let initialization = Initialization::new(
        identity,
        BootstrapState::live(),
        genesis,
        tempo,
        ModelState::created_with_zone_token_for_test(portal, TokenAccounting::ZERO),
    );
    let store = CheckerStore::open(directory.path(), initialization).unwrap();
    let finding_tip = BlockNumHash::new(1, B256::repeat_byte(0x61));
    let finding = FindingKey::new(finding_tip.number, finding_tip.hash, 0);
    let record = FindingRecord::new(
        genesis.hash,
        Some(tempo),
        FindingStatus::Canonical,
        FindingKind::MissingSupply(portal.initial_token()),
    )
    .unwrap();
    store.activate_finding(finding, record, genesis).unwrap();
    let database_path = store.path().to_owned();
    drop(store);

    let canonical = canonical_provider([
        genesis,
        finding_tip,
        BlockNumHash::new(2, B256::repeat_byte(0x62)),
    ]);
    let config = CheckerConfig {
        l1_rpc_url: "not a URL".into(),
        portal_address: portal.portal(),
        portal_creation_block_hash: identity.portal_creation_block().hash,
        zone_id: ZONE_ID,
        database_path: Some(database_path.clone()),
    };
    (config, zone_chain_id, database_path, canonical, finding)
}

fn canonical_provider(tips: impl IntoIterator<Item = BlockNumHash>) -> ZoneProvider {
    let provider = ZoneProvider::new();
    for tip in tips {
        provider.add_header(
            tip.hash,
            TempoHeader {
                inner: Header {
                    number: tip.number,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
    }
    let token = Address::repeat_byte(0x20);
    provider.add_account(
        DEFAULT_FEE_TOKEN_ACCESS.address,
        ExtendedAccount::new(0, U256::ZERO).extend_storage([(
            DEFAULT_FEE_TOKEN_ACCESS.storage_key(),
            U256::from_be_slice(B256::left_padding_from(token.as_slice()).as_slice()),
        )]),
    );
    provider
}

fn notification_chain(number: u64, parent_hash: B256, marker: u8) -> Arc<Chain<TempoPrimitives>> {
    let block = Block {
        header: TempoHeader {
            inner: Header {
                number,
                parent_hash,
                extra_data: Bytes::from(vec![marker]),
                ..Default::default()
            },
            ..Default::default()
        },
        body: Default::default(),
    };
    let block = RecoveredBlock::new_sealed(SealedBlock::seal_slow(block), Vec::new());
    let outcome = ExecutionOutcome::new(
        Default::default(),
        vec![Vec::new()],
        number,
        Default::default(),
    );
    Arc::new(Chain::new(vec![block], outcome, BTreeMap::new()))
}

fn chain_tip(chain: &Chain<TempoPrimitives>) -> BlockNumHash {
    let (&number, block) = chain.blocks().last_key_value().unwrap();
    BlockNumHash::new(number, block.hash())
}
