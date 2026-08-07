use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256};
use tempfile::TempDir;

use super::promote_zone_replay_if_ready;
use crate::{
    model::{
        accounting::TokenAccounting,
        state::{ModelState, PortalIdentity, portal_address_for_zone},
    },
    runtime::{PersistentChecker, state::ReadyToAcknowledge},
    store::{
        db::{CheckerStore, FreshBootstrap, Initialization},
        value::{BootstrapState, StoreIdentity},
    },
};

const ZONE_ID: u32 = 7;

#[test]
fn zone_replay_promotes_only_at_one_exact_non_alerting_cut() {
    let (_directory, mut checker, verified_tip) = created_replay_checker();
    let later_head = BlockNumHash::new(1, B256::repeat_byte(0x91));

    assert!(
        !promote_zone_replay_if_ready(
            &mut checker,
            ReadyToAcknowledge::verified(verified_tip),
            later_head,
        )
        .unwrap()
    );
    assert!(matches!(
        checker.store.load_current().unwrap().bootstrap,
        BootstrapState::ZoneReplay { .. }
    ));

    assert!(
        !promote_zone_replay_if_ready(
            &mut checker,
            ReadyToAcknowledge::verified(later_head),
            verified_tip,
        )
        .unwrap()
    );
    assert!(matches!(
        checker.store.load_current().unwrap().bootstrap,
        BootstrapState::ZoneReplay { .. }
    ));

    assert!(
        promote_zone_replay_if_ready(
            &mut checker,
            ReadyToAcknowledge::verified(verified_tip),
            verified_tip,
        )
        .unwrap()
    );
    assert_eq!(
        checker.store.load_current().unwrap().bootstrap,
        BootstrapState::live()
    );
}

#[test]
fn alerting_zone_replay_never_promotes_at_the_canonical_head() {
    let (_directory, mut checker, verified_tip) = created_replay_checker();

    assert!(
        !promote_zone_replay_if_ready(
            &mut checker,
            ReadyToAcknowledge::alerted(verified_tip),
            verified_tip,
        )
        .unwrap()
    );
    assert!(matches!(
        checker.store.load_current().unwrap().bootstrap,
        BootstrapState::ZoneReplay { .. }
    ));
}

#[test]
fn caught_up_zone_replay_promotes_while_awaiting_later_portal_creation() {
    let (directory, identity, genesis_anchor) =
        replay_identity(BlockNumHash::new(43, B256::repeat_byte(0x50)));
    let initialization =
        Initialization::fresh(identity, FreshBootstrap::ZoneReplay { genesis_anchor });
    let verified_tip = initialization.verified_zone_tip;
    let store = CheckerStore::open(directory.path(), initialization).unwrap();
    let mut checker = PersistentChecker::from_bootstrap_store(store).unwrap();

    assert!(
        promote_zone_replay_if_ready(
            &mut checker,
            ReadyToAcknowledge::verified(verified_tip),
            verified_tip,
        )
        .unwrap()
    );
    let current = checker.store.load_current().unwrap();
    assert_eq!(current.bootstrap, BootstrapState::live());
    assert!(current.model.portal().created().is_none());
}

fn created_replay_checker() -> (TempDir, PersistentChecker, BlockNumHash) {
    let (directory, identity, genesis_anchor) =
        replay_identity(BlockNumHash::new(42, B256::repeat_byte(0x42)));
    let verified_tip = BlockNumHash::new(0, identity.zone_genesis_hash());
    let initialization = Initialization::new(
        identity,
        BootstrapState::zone_replay(genesis_anchor),
        verified_tip,
        genesis_anchor,
        ModelState::created_with_zone_token_for_test(
            identity.portal_identity(),
            TokenAccounting::ZERO,
        ),
    );
    let store = CheckerStore::open(directory.path(), initialization).unwrap();
    let checker = PersistentChecker::from_bootstrap_store(store).unwrap();
    (directory, checker, verified_tip)
}

fn replay_identity(creation: BlockNumHash) -> (TempDir, StoreIdentity, BlockNumHash) {
    let directory = TempDir::new().unwrap();
    let zone_genesis = B256::repeat_byte(0x10);
    let genesis_anchor = BlockNumHash::new(42, B256::repeat_byte(0x42));
    let portal_identity = PortalIdentity::new(
        portal_address_for_zone(ZONE_ID),
        ZONE_ID,
        Address::repeat_byte(0x20),
    );
    let identity = StoreIdentity::new(
        4242,
        zone_genesis,
        portal_identity,
        31337,
        Address::repeat_byte(0x30),
        creation,
    );
    (directory, identity, genesis_anchor)
}
