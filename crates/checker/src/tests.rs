use std::{collections::BTreeMap, sync::Arc};

use alloy_consensus::{Header, Sealable as _, Signed, TxLegacy};
use alloy_primitives::{Address, B256, Bytes, Log, U256};
use alloy_provider::ProviderBuilder;
use alloy_rlp::Encodable as _;
use alloy_rpc_types_eth::{BlockTransactions, Header as RpcHeader};
use alloy_sol_types::{SolCall as _, SolEvent as _};
use alloy_transport::mock::Asserter;
use reth_execution_types::{Chain, ExecutionOutcome};
use reth_primitives_traits::{RecoveredBlock, SealedBlock};
use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
use reth_storage_api::{StateProviderBox, errors::provider::ProviderResult};
use tempo_alloy::{
    TempoNetwork,
    rpc::{TempoHeaderResponse, TempoTransactionReceipt},
};
use tempo_primitives::{
    BlockBody, TempoHeader, TempoTxEnvelope, TempoTxType,
    transaction::envelope::TEMPO_SYSTEM_TX_SIGNATURE,
};
use tempo_zone_contracts::{IZoneInbox, TempoState};

use super::*;
use crate::{
    model::{
        constants::{TEMPO_STATE_ADDRESS, ZONE_INBOX_ADDRESS},
        state_layout::{
            INBOX_PROCESSED_DEPOSIT_HASH_ACCESS, OUTBOX_LAST_BATCH_QUEUE_HASH_ACCESS,
            TEMPO_BLOCK_HASH_ACCESS, TEMPO_BLOCK_NUMBER_ACCESS,
        },
    },
    observe::AcquisitionError,
};

mod pipeline;

const PORTAL: Address = Address::repeat_byte(0x42);
const L1_NUMBER: u64 = 100;
type TestProvider = MockEthProvider<TempoPrimitives>;
type L1RpcBlock = alloy_rpc_types_eth::Block<
    alloy_rpc_types_eth::Transaction<TempoTxEnvelope>,
    TempoHeaderResponse,
>;

struct UnavailableZoneState;

impl ExactStateLookup for UnavailableZoneState {
    fn state_by_exact_block_hash(&self, block_hash: B256) -> ProviderResult<StateProviderBox> {
        Err(reth_storage_api::errors::provider::ProviderError::StateForHashNotFound(block_hash))
    }
}

#[test]
fn checker_mode_parse_display_default() {
    assert_eq!(CheckerMode::default(), CheckerMode::Off);
    assert_eq!("off".parse::<CheckerMode>().unwrap(), CheckerMode::Off);
    assert_eq!(
        "OBSERVE".parse::<CheckerMode>().unwrap(),
        CheckerMode::Observe
    );
    assert!("enforce".parse::<CheckerMode>().is_err());
    assert_eq!(CheckerMode::Off.to_string(), "off");
    assert_eq!(CheckerMode::Observe.to_string(), "observe");
}

#[test]
fn acknowledgement_state_never_crosses_the_first_observation_gap() {
    let first_tip = BlockNumHash::new(1, B256::repeat_byte(0x01));
    let later_tip = BlockNumHash::new(2, B256::repeat_byte(0x02));
    let mut state = AcknowledgementState::default();

    assert_eq!(state.record(Ok(first_tip)).unwrap(), Some(first_tip));
    assert!(matches!(
        state.record(Err(AcquisitionError::missing(
            AcquisitionSource::L1Block,
            B256::repeat_byte(0xee),
        )
        .into())),
        Err(ObservationError::Acquisition(AcquisitionError::Missing {
            kind: AcquisitionSource::L1Block,
            ..
        }))
    ));
    assert_eq!(state, AcknowledgementState::Blocked);
    assert_eq!(state.record(Ok(later_tip)).unwrap(), None);
}

fn imported_header(number: u64) -> TempoHeader {
    imported_child_header(number, B256::ZERO)
}

fn imported_child_header(number: u64, parent_hash: B256) -> TempoHeader {
    let receipts_root = alloy_consensus::proofs::calculate_receipt_root::<
        alloy_consensus::ReceiptWithBloom<tempo_primitives::TempoReceipt<Log>>,
    >(&[]);
    TempoHeader {
        inner: Header {
            number,
            parent_hash,
            receipts_root,
            base_fee_per_gas: Some(0),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn encode_header(header: &TempoHeader) -> Bytes {
    let mut encoded = Vec::new();
    header.encode(&mut encoded);
    encoded.into()
}

fn zone_block(number: u64, parent_hash: B256, imported: &TempoHeader) -> RecoveredBlock<Block> {
    zone_block_with_marker(number, parent_hash, imported, 0)
}

fn zone_block_with_marker(
    number: u64,
    parent_hash: B256,
    imported: &TempoHeader,
    fork_marker: u8,
) -> RecoveredBlock<Block> {
    let advance = advance_transaction(imported);
    let block = Block {
        header: TempoHeader {
            inner: Header {
                number,
                parent_hash,
                extra_data: Bytes::from(vec![fork_marker]),
                ..Default::default()
            },
            ..Default::default()
        },
        body: BlockBody {
            transactions: vec![advance],
            ..Default::default()
        },
    };
    RecoveredBlock::new_sealed(SealedBlock::seal_slow(block), vec![Address::ZERO])
}

fn advance_transaction(imported: &TempoHeader) -> TempoTxEnvelope {
    let calldata = IZoneInbox::advanceTempoCall {
        header: encode_header(imported),
        deposits: Vec::new(),
        decryptions: Vec::new(),
        enabledTokens: Vec::new(),
    }
    .abi_encode();
    TempoTxEnvelope::Legacy(Signed::new_unhashed(
        TxLegacy {
            gas_limit: 0,
            to: ZONE_INBOX_ADDRESS.into(),
            input: calldata.into(),
            ..Default::default()
        },
        TEMPO_SYSTEM_TX_SIGNATURE,
    ))
}

fn zone_receipt(imported: &TempoHeader) -> TempoReceipt {
    let imported_hash = imported.hash_slow();
    TempoReceipt {
        tx_type: TempoTxType::Legacy,
        success: true,
        cumulative_gas_used: 0,
        logs: vec![
            Log {
                address: TEMPO_STATE_ADDRESS,
                data: TempoState::TempoBlockFinalized {
                    blockHash: imported_hash,
                    blockNumber: imported.inner.number,
                    stateRoot: imported.inner.state_root,
                }
                .encode_log_data(),
            },
            Log {
                address: ZONE_INBOX_ADDRESS,
                data: IZoneInbox::TempoAdvanced {
                    tempoBlockHash: imported_hash,
                    tempoBlockNumber: imported.inner.number,
                    depositsProcessed: U256::ZERO,
                    newProcessedDepositQueueHash: B256::ZERO,
                    lastProcessedDepositNumber: 0,
                }
                .encode_log_data(),
            },
        ],
    }
}

fn chain(
    blocks: Vec<RecoveredBlock<Block>>,
    receipt_sets: Vec<Vec<TempoReceipt>>,
) -> Arc<Chain<TempoPrimitives>> {
    let first_block = blocks
        .first()
        .map(|block| block.header().number())
        .unwrap_or_default();
    let outcome = ExecutionOutcome::new(
        Default::default(),
        receipt_sets,
        first_block,
        Default::default(),
    );
    Arc::new(Chain::new(blocks, outcome, BTreeMap::new()))
}

fn account_with_storage(storage: impl IntoIterator<Item = (B256, U256)>) -> ExtendedAccount {
    ExtendedAccount::new(0, U256::ZERO).extend_storage(storage)
}

fn zone_state(imported: &TempoHeader) -> TestProvider {
    let provider = TestProvider::new();
    provider.add_account(
        TEMPO_BLOCK_HASH_ACCESS.address,
        account_with_storage([
            (
                TEMPO_BLOCK_HASH_ACCESS.storage_key(),
                U256::from_be_slice(imported.hash_slow().as_slice()),
            ),
            (
                TEMPO_BLOCK_NUMBER_ACCESS.storage_key(),
                U256::from(imported.inner.number),
            ),
        ]),
    );
    provider.add_account(
        INBOX_PROCESSED_DEPOSIT_HASH_ACCESS.address,
        account_with_storage([]),
    );
    provider.add_account(
        OUTBOX_LAST_BATCH_QUEUE_HASH_ACCESS.address,
        account_with_storage([]),
    );
    provider
}

fn l1_rpc_block(imported: &TempoHeader) -> L1RpcBlock {
    alloy_rpc_types_eth::Block {
        header: TempoHeaderResponse {
            inner: RpcHeader {
                hash: imported.hash_slow(),
                inner: imported.clone(),
                total_difficulty: None,
                size: None,
            },
            timestamp_millis: 0,
        },
        uncles: Vec::new(),
        transactions:
            BlockTransactions::<alloy_rpc_types_eth::Transaction<TempoTxEnvelope>>::Hashes(
                Vec::new(),
            ),
        withdrawals: None,
    }
}

fn l1_provider(imported: &TempoHeader) -> DynProvider<TempoNetwork> {
    let asserter = Asserter::new();
    asserter.push_success(&Some(l1_rpc_block(imported)));
    asserter.push_success(&Some(Vec::<TempoTransactionReceipt>::new()));
    ProviderBuilder::new_with_network::<TempoNetwork>()
        .connect_mocked_client(asserter)
        .erased()
}

#[tokio::test]
async fn committed_notification_wires_zone_and_imported_tempo_observers() {
    let imported = imported_header(L1_NUMBER);
    let block = zone_block(1, B256::repeat_byte(0x11), &imported);
    let expected_tip = BlockNumHash::new(1, block.hash());
    let notification = ExExNotification::ChainCommitted {
        new: chain(vec![block], vec![vec![zone_receipt(&imported)]]),
    };
    let provider = zone_state(&imported);
    let mut l1 = Some(l1_provider(&imported));

    let tip = process_notification(&notification, &provider, &mut l1, "", PORTAL)
        .await
        .unwrap();

    assert_eq!(tip, expected_tip);
}

#[tokio::test]
async fn exact_zone_state_gap_blocks_live_observation_acknowledgement() {
    let imported = imported_header(L1_NUMBER);
    let block = zone_block(1, B256::repeat_byte(0x19), &imported);
    let notification = ExExNotification::ChainCommitted {
        new: chain(vec![block], vec![vec![zone_receipt(&imported)]]),
    };
    let mut l1 = Some(l1_provider(&imported));

    assert!(matches!(
        process_notification(&notification, &UnavailableZoneState, &mut l1, "", PORTAL).await,
        Err(ObservationError::Acquisition(
            AcquisitionError::Unavailable {
                kind: AcquisitionSource::ExactZoneState,
                ..
            }
        ))
    ));
}

#[tokio::test]
async fn notification_receipt_set_gap_preserves_acquisition_class() {
    let imported = imported_header(L1_NUMBER);
    let block = zone_block(1, B256::repeat_byte(0x22), &imported);
    let notification = ExExNotification::ChainCommitted {
        new: Arc::new(Chain::new(
            vec![block],
            ExecutionOutcome::<TempoReceipt>::default(),
            BTreeMap::new(),
        )),
    };
    let mut l1 = None;

    assert!(matches!(
        process_notification(&notification, &TestProvider::new(), &mut l1, "", PORTAL,).await,
        Err(ObservationError::Acquisition(
            AcquisitionError::Inconsistent {
                kind: AcquisitionSource::ZoneNotificationReceipts,
                ..
            }
        ))
    ));
}

#[tokio::test]
async fn reverted_notification_returns_the_parent_tip_without_observation() {
    let parent_hash = B256::repeat_byte(0x33);
    let imported = imported_header(L1_NUMBER);
    let block = zone_block(7, parent_hash, &imported);
    let notification = ExExNotification::ChainReverted {
        old: chain(vec![block], vec![vec![zone_receipt(&imported)]]),
    };
    let mut l1 = None;

    let tip = process_notification(&notification, &TestProvider::new(), &mut l1, "", PORTAL)
        .await
        .unwrap();

    assert_eq!(tip, BlockNumHash::new(6, parent_hash));
}

#[tokio::test]
async fn reorg_logs_the_old_chain_then_observes_only_the_replacement() {
    let parent_hash = B256::repeat_byte(0x44);
    let imported = imported_header(L1_NUMBER);
    let old = zone_block_with_marker(7, parent_hash, &imported, 1);
    let replacement = zone_block_with_marker(7, parent_hash, &imported, 2);
    assert_ne!(old.hash(), replacement.hash());
    let replacement_tip = BlockNumHash::new(7, replacement.hash());
    let notification = ExExNotification::ChainReorged {
        // An intentionally incomplete old receipt set proves the reverted side
        // is never sent through the authenticated observation adapters.
        old: chain(vec![old], vec![]),
        new: chain(vec![replacement], vec![vec![zone_receipt(&imported)]]),
    };
    let provider = zone_state(&imported);
    let mut l1 = Some(l1_provider(&imported));

    let tip = process_notification(&notification, &provider, &mut l1, "", PORTAL)
        .await
        .unwrap();

    assert_eq!(tip, replacement_tip);
}
