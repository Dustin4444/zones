use alloy_consensus::{Header, Sealable as _};
use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256, U256};
use alloy_provider::{DynProvider, Provider as _, ProviderBuilder};
use alloy_transport::mock::Asserter;
use reth_primitives_traits::{RecoveredBlock, SealedBlock};
use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
use reth_storage_api::{StateProviderBox, errors::provider::ProviderResult};
use tempo_alloy::TempoNetwork;
use tempo_primitives::{Block, BlockBody, TempoHeader, TempoPrimitives};

use super::{
    L1_NUMBER, L1RpcBlock, PORTAL, exact_zone_state_with_supply, imported_child_header,
    l1_provider, l1_provider_with_collateral, user_withdrawal_receipt, zone_block,
    zone_block_with_user_withdrawal, zone_receipt,
};
use crate::{
    check::{
        finding::{CheckError, Finding, FixedStateFinding, ObservationFinding},
        pipeline::InMemoryChecker,
    },
    model::{
        accounting::TokenAccounting,
        state::{ModelState, PortalIdentity, portal_address_for_zone},
        state_layout::{
            INBOX_PROCESSED_DEPOSIT_HASH_ACCESS, OUTBOX_LAST_BATCH_QUEUE_HASH_ACCESS,
            TEMPO_BLOCK_HASH_ACCESS, TEMPO_BLOCK_NUMBER_ACCESS,
        },
    },
    observe::{
        AcquisitionError, AcquisitionSource, EnvelopeRule, ExactStateLookup, L1BlockObservation,
        L2BlockObservation, observe_l1, observe_l2_block,
    },
};

type TestStateProvider = MockEthProvider<TempoPrimitives>;

fn l1_provider_missing_block() -> DynProvider<TempoNetwork> {
    let asserter = Asserter::new();
    asserter.push_success(&Option::<L1RpcBlock>::None);
    ProviderBuilder::new_with_network::<TempoNetwork>()
        .connect_mocked_client(asserter)
        .erased()
}

fn exact_zone_state(imported: &TempoHeader, tempo_hash: B256) -> TestStateProvider {
    let provider = TestStateProvider::new();
    provider.add_account(
        TEMPO_BLOCK_HASH_ACCESS.address,
        ExtendedAccount::new(0, U256::ZERO).extend_storage([
            (
                TEMPO_BLOCK_HASH_ACCESS.storage_key(),
                U256::from_be_slice(tempo_hash.as_slice()),
            ),
            (
                TEMPO_BLOCK_NUMBER_ACCESS.storage_key(),
                U256::from(imported.inner.number),
            ),
        ]),
    );
    provider.add_account(
        INBOX_PROCESSED_DEPOSIT_HASH_ACCESS.address,
        ExtendedAccount::new(0, U256::ZERO),
    );
    provider.add_account(
        OUTBOX_LAST_BATCH_QUEUE_HASH_ACCESS.address,
        ExtendedAccount::new(0, U256::ZERO),
    );
    provider
}

struct UnavailableExactState;

impl ExactStateLookup for UnavailableExactState {
    fn state_by_exact_block_hash(&self, block_hash: B256) -> ProviderResult<StateProviderBox> {
        Err(reth_storage_api::errors::provider::ProviderError::StateForHashNotFound(block_hash))
    }
}

fn checker_for_empty_child(
    imported: &TempoHeader,
    zone_parent: B256,
    creation_hash: B256,
) -> InMemoryChecker {
    let identity = PortalIdentity::new(portal_address_for_zone(7), 7, Address::repeat_byte(0x77));
    InMemoryChecker::new(
        ModelState::awaiting_creation(identity),
        creation_hash,
        BlockNumHash::new(0, zone_parent),
        BlockNumHash::new(imported.inner.number - 1, imported.inner.parent_hash),
    )
}

fn assert_checker_at_parent(
    checker: &InMemoryChecker,
    model: &ModelState,
    zone_parent: B256,
    tempo_parent: B256,
) {
    assert_eq!(checker.model(), model);
    assert_eq!(checker.zone_tip(), BlockNumHash::new(0, zone_parent));
    assert_eq!(
        checker.tempo_tip(),
        BlockNumHash::new(L1_NUMBER - 1, tempo_parent)
    );
}

async fn empty_observations(
    imported: &TempoHeader,
    zone_parent: B256,
) -> (
    L2BlockObservation,
    L1BlockObservation,
    DynProvider<TempoNetwork>,
) {
    let block = zone_block(1, zone_parent, imported);
    let receipt = zone_receipt(imported);
    let l2 = observe_l2_block(&block, std::slice::from_ref(&receipt)).unwrap();
    let provider = l1_provider(imported);
    let l1 = observe_l1(
        &provider,
        l2.inputs().advance_tempo().imported_header(),
        portal_address_for_zone(7),
    )
    .await
    .unwrap();
    (l2, l1, provider)
}

#[tokio::test]
async fn committed_state_is_the_next_parent_and_sparse_followup_preserves_it() {
    let tempo_parent = B256::repeat_byte(0x90);
    let zone_parent = B256::repeat_byte(0x91);
    let imported = imported_child_header(L1_NUMBER, tempo_parent);
    let sender = Address::repeat_byte(0x53);
    let token = Address::repeat_byte(0x20);
    let portal = portal_address_for_zone(7);
    let parent_supply = U256::from(100_000_u64);
    let committed_supply = U256::from(49_990_u64);
    let model = ModelState::created_with_zone_token_for_test(
        PortalIdentity::new(portal, 7, token),
        TokenAccounting {
            supply: parent_supply,
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::ZERO,
        },
    );
    let parent_model = model.clone();
    let block = zone_block_with_user_withdrawal(1, zone_parent, &imported, sender);
    let receipts = [
        zone_receipt(&imported),
        user_withdrawal_receipt(sender, token),
    ];
    let l2 = observe_l2_block(&block, &receipts).unwrap();
    let l1_provider = l1_provider_with_collateral(&imported, parent_supply);
    let l1 = observe_l1(
        &l1_provider,
        l2.inputs().advance_tempo().imported_header(),
        portal,
    )
    .await
    .unwrap();
    let mut checker = InMemoryChecker::new(
        model,
        B256::repeat_byte(0xcc),
        BlockNumHash::new(0, zone_parent),
        BlockNumHash::new(L1_NUMBER - 1, tempo_parent),
    );
    let exact_state = exact_zone_state_with_supply(&imported, token, committed_supply);

    checker
        .check_block(&l1_provider, &exact_state, &l1, &l2)
        .await
        .unwrap();

    let first_zone_tip = checker.zone_tip();
    let first_tempo_tip = checker.tempo_tip();
    let committed_model = checker.model().clone();
    assert_ne!(committed_model, parent_model);
    assert_eq!(
        committed_model.token(token).unwrap().accounting(),
        TokenAccounting {
            supply: committed_supply,
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::from(10),
        }
    );
    let next_imported = imported_child_header(L1_NUMBER + 1, first_tempo_tip.hash);
    let next_block = zone_block(
        first_zone_tip.number + 1,
        first_zone_tip.hash,
        &next_imported,
    );
    let next_receipt = zone_receipt(&next_imported);
    let next_l2 = observe_l2_block(&next_block, std::slice::from_ref(&next_receipt)).unwrap();
    // The original parent requires 100_000 collateral. This exact 50_000
    // succeeds only after block one commits S=49_990 and W=10.
    let next_l1_provider = l1_provider_with_collateral(&next_imported, U256::from(50_000_u64));
    let next_l1 = observe_l1(
        &next_l1_provider,
        next_l2.inputs().advance_tempo().imported_header(),
        portal,
    )
    .await
    .unwrap();
    assert_eq!(next_l2.parent_hash(), first_zone_tip.hash);
    assert_eq!(
        next_l2
            .inputs()
            .advance_tempo()
            .imported_header()
            .header()
            .inner
            .parent_hash,
        first_tempo_tip.hash
    );
    let next_exact_state = exact_zone_state_with_supply(&next_imported, token, committed_supply);

    checker
        .check_block(&next_l1_provider, &next_exact_state, &next_l1, &next_l2)
        .await
        .unwrap();

    assert_eq!(checker.model(), &committed_model);
    assert_eq!(
        checker.zone_tip(),
        BlockNumHash::new(next_l2.block_number(), next_l2.block_hash())
    );
    assert_eq!(
        checker.tempo_tip(),
        BlockNumHash::new(next_imported.inner.number, next_imported.hash_slow())
    );
}

#[tokio::test]
async fn exact_state_finding_leaves_model_and_both_tips_at_the_parent() {
    let tempo_parent = B256::repeat_byte(0xa0);
    let zone_parent = B256::repeat_byte(0xa1);
    let imported = imported_child_header(L1_NUMBER, tempo_parent);
    let (l2, l1, l1_provider) = empty_observations(&imported, zone_parent).await;
    let mut checker = checker_for_empty_child(&imported, zone_parent, B256::repeat_byte(0xcc));
    let parent_model = checker.model().clone();
    let wrong_hash = B256::repeat_byte(0xee);
    let exact_state = exact_zone_state(&imported, wrong_hash);

    let error = checker
        .check_block(&l1_provider, &exact_state, &l1, &l2)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        CheckError::Finding { finding, .. }
            if matches!(
                finding.as_ref(),
                Finding::FixedState(FixedStateFinding::TempoBlockHash {
                    expected,
                    actual,
                }) if *expected == imported.hash_slow() && *actual == wrong_hash
            )
    ));
    assert_checker_at_parent(&checker, &parent_model, zone_parent, tempo_parent);
}

#[tokio::test]
async fn exact_state_acquisition_failure_is_not_a_finding_and_is_atomic() {
    let tempo_parent = B256::repeat_byte(0xb0);
    let zone_parent = B256::repeat_byte(0xb1);
    let imported = imported_child_header(L1_NUMBER, tempo_parent);
    let (l2, l1, l1_provider) = empty_observations(&imported, zone_parent).await;
    let mut checker = checker_for_empty_child(&imported, zone_parent, B256::repeat_byte(0xcc));
    let parent_model = checker.model().clone();

    let error = checker
        .check_block(&l1_provider, &UnavailableExactState, &l1, &l2)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        CheckError::Acquisition(AcquisitionError::Unavailable {
            kind: AcquisitionSource::ExactZoneState,
            ..
        })
    ));
    assert_checker_at_parent(&checker, &parent_model, zone_parent, tempo_parent);
}

#[tokio::test]
async fn configured_creation_block_without_creation_is_a_finding_before_acquisition() {
    let tempo_parent = B256::repeat_byte(0xc0);
    let zone_parent = B256::repeat_byte(0xc1);
    let imported = imported_child_header(L1_NUMBER, tempo_parent);
    let (l2, l1, l1_provider) = empty_observations(&imported, zone_parent).await;
    let mut checker = checker_for_empty_child(&imported, zone_parent, imported.hash_slow());

    let error = checker
        .check_block(&l1_provider, &UnavailableExactState, &l1, &l2)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        CheckError::Finding { finding, .. }
            if matches!(
                finding.as_ref(),
                Finding::PortalCreationMissing { block_hash }
                    if *block_hash == imported.hash_slow()
            )
    ));
    assert_eq!(checker.zone_tip(), BlockNumHash::new(0, zone_parent));
}

#[tokio::test]
async fn l1_observation_portal_must_match_the_configured_model_identity() {
    let tempo_parent = B256::repeat_byte(0xd0);
    let zone_parent = B256::repeat_byte(0xd1);
    let imported = imported_child_header(L1_NUMBER, tempo_parent);
    let block = zone_block(1, zone_parent, &imported);
    let receipt = zone_receipt(&imported);
    let l2 = observe_l2_block(&block, std::slice::from_ref(&receipt)).unwrap();
    let l1_provider = l1_provider(&imported);
    let l1 = observe_l1(
        &l1_provider,
        l2.inputs().advance_tempo().imported_header(),
        PORTAL,
    )
    .await
    .unwrap();
    let mut checker = checker_for_empty_child(&imported, zone_parent, B256::repeat_byte(0xcc));
    let expected = portal_address_for_zone(7);

    let error = checker
        .check_block(&l1_provider, &UnavailableExactState, &l1, &l2)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        CheckError::Finding { finding, .. }
            if matches!(
                finding.as_ref(),
                Finding::PortalObservationIdentityMismatch {
                    expected: actual_expected,
                    actual: PORTAL,
                } if *actual_expected == expected
            )
    ));
    assert_eq!(checker.zone_tip(), BlockNumHash::new(0, zone_parent));
}

#[tokio::test]
async fn collateral_uses_the_pre_zone_cut_before_same_block_burns_can_hide_a_deficit() {
    let tempo_parent = B256::repeat_byte(0xe0);
    let zone_parent = B256::repeat_byte(0xe1);
    let imported = imported_child_header(L1_NUMBER, tempo_parent);
    let sender = Address::repeat_byte(0x53);
    let token = Address::repeat_byte(0x20);
    let portal = portal_address_for_zone(7);
    let identity = PortalIdentity::new(portal, 7, token);
    let parent_supply = U256::from(100_000_u64);
    let collateral = parent_supply - U256::ONE;
    let model = ModelState::created_with_zone_token_for_test(
        identity,
        TokenAccounting {
            supply: parent_supply,
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::ZERO,
        },
    );
    let block = zone_block_with_user_withdrawal(1, zone_parent, &imported, sender);
    let receipts = [
        zone_receipt(&imported),
        user_withdrawal_receipt(sender, token),
    ];
    let l2 = observe_l2_block(&block, &receipts).unwrap();
    let l1_provider = l1_provider_with_collateral(&imported, collateral);
    let l1 = observe_l1(
        &l1_provider,
        l2.inputs().advance_tempo().imported_header(),
        portal,
    )
    .await
    .unwrap();
    let mut checker = InMemoryChecker::new(
        model.clone(),
        B256::repeat_byte(0xcc),
        BlockNumHash::new(0, zone_parent),
        BlockNumHash::new(L1_NUMBER - 1, tempo_parent),
    );

    let error = checker
        .check_block(&l1_provider, &UnavailableExactState, &l1, &l2)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        CheckError::Finding { finding, .. }
            if matches!(
                finding.as_ref(),
                Finding::CollateralDeficit {
                    token: actual_token,
                    required,
                    actual,
                } if *actual_token == token && *required == parent_supply && *actual == collateral
            )
    ));
    assert_checker_at_parent(&checker, &model, zone_parent, tempo_parent);
}

#[tokio::test]
async fn post_zone_supply_detects_unauthorized_mint_and_burn_and_keeps_the_parent() {
    let tempo_parent = B256::repeat_byte(0xf0);
    let zone_parent = B256::repeat_byte(0xf1);
    let imported = imported_child_header(L1_NUMBER, tempo_parent);
    let token = Address::repeat_byte(0x20);
    let portal = portal_address_for_zone(7);
    let identity = PortalIdentity::new(portal, 7, token);
    let expected_supply = U256::from(100_000_u64);
    let model = ModelState::created_with_zone_token_for_test(
        identity,
        TokenAccounting {
            supply: expected_supply,
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::ZERO,
        },
    );
    let block = zone_block(1, zone_parent, &imported);
    let receipt = zone_receipt(&imported);
    let l2 = observe_l2_block(&block, std::slice::from_ref(&receipt)).unwrap();
    for actual_supply in [expected_supply + U256::ONE, expected_supply - U256::ONE] {
        let l1_provider = l1_provider_with_collateral(&imported, expected_supply);
        let l1 = observe_l1(
            &l1_provider,
            l2.inputs().advance_tempo().imported_header(),
            portal,
        )
        .await
        .unwrap();
        let exact_state = exact_zone_state_with_supply(&imported, token, actual_supply);
        let mut checker = InMemoryChecker::new(
            model.clone(),
            B256::repeat_byte(0xcc),
            BlockNumHash::new(0, zone_parent),
            BlockNumHash::new(L1_NUMBER - 1, tempo_parent),
        );

        let error = checker
            .check_block(&l1_provider, &exact_state, &l1, &l2)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            CheckError::Finding { finding, .. }
                if matches!(
                    finding.as_ref(),
                    Finding::SupplyMismatch {
                        token: actual_token,
                        expected,
                        actual,
                    } if *actual_token == token
                        && *expected == expected_supply
                        && *actual == actual_supply
                )
        ));
        assert_checker_at_parent(&checker, &model, zone_parent, tempo_parent);
    }
}

#[tokio::test]
async fn malformed_l2_envelope_is_an_atomic_observation_finding() {
    let tempo_parent = B256::repeat_byte(0x61);
    let zone_parent = B256::repeat_byte(0x62);
    let imported = imported_child_header(L1_NUMBER, tempo_parent);
    let block = Block {
        header: TempoHeader {
            inner: Header {
                number: 1,
                parent_hash: zone_parent,
                ..Default::default()
            },
            ..Default::default()
        },
        body: BlockBody::default(),
    };
    let block = RecoveredBlock::new_sealed(SealedBlock::seal_slow(block), Vec::new());
    let l1_provider = l1_provider(&imported);
    let mut checker = checker_for_empty_child(&imported, zone_parent, B256::repeat_byte(0xcc));
    let parent_model = checker.model().clone();

    let error = checker
        .observe_and_check_block(&l1_provider, &UnavailableExactState, &block, &[])
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        CheckError::Finding { finding, .. }
            if matches!(
                finding.as_ref(),
                Finding::Observation(observation)
                    if matches!(
                        observation.as_ref(),
                        ObservationFinding::InvalidEnvelope {
                            rule: EnvelopeRule::AdvancePresent,
                            ..
                        }
                    )
            )
    ));
    assert_checker_at_parent(&checker, &parent_model, zone_parent, tempo_parent);
}

#[tokio::test]
async fn l1_observation_acquisition_is_an_atomic_acquisition_error() {
    let tempo_parent = B256::repeat_byte(0x71);
    let zone_parent = B256::repeat_byte(0x72);
    let imported = imported_child_header(L1_NUMBER, tempo_parent);
    let block = zone_block(1, zone_parent, &imported);
    let receipt = zone_receipt(&imported);
    let l1_provider = l1_provider_missing_block();
    let mut checker = checker_for_empty_child(&imported, zone_parent, B256::repeat_byte(0xcc));
    let parent_model = checker.model().clone();

    let error = checker
        .observe_and_check_block(
            &l1_provider,
            &UnavailableExactState,
            &block,
            std::slice::from_ref(&receipt),
        )
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        CheckError::Acquisition(AcquisitionError::Missing {
            kind: AcquisitionSource::L1Block,
            ..
        })
    ));
    assert_checker_at_parent(&checker, &parent_model, zone_parent, tempo_parent);
}
