use std::{collections::BTreeMap, sync::Arc};

use alloy_consensus::{BlockHeader as _, Header, Sealable as _, Signed, TxLegacy, TxReceipt as _};
use alloy_primitives::{Address, B256, Bloom, Bytes, Log, Signature, U256};
use alloy_provider::{DynProvider, Provider as _, ProviderBuilder};
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
use tempo_contracts::precompiles::ITIP20;
use tempo_primitives::{
    Block, BlockBody, TempoHeader, TempoPrimitives, TempoReceipt, TempoTxEnvelope, TempoTxType,
    transaction::envelope::TEMPO_SYSTEM_TX_SIGNATURE,
};
use tempo_zone_contracts::{IZoneInbox, IZoneOutbox, TempoState};

use super::*;
use crate::{
    model::{
        constants::{TEMPO_STATE_ADDRESS, ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS},
        state_layout::{
            INBOX_PROCESSED_DEPOSIT_HASH_ACCESS, OUTBOX_LAST_BATCH_QUEUE_HASH_ACCESS,
            TEMPO_BLOCK_HASH_ACCESS, TEMPO_BLOCK_NUMBER_ACCESS, tip20_total_supply_access,
        },
    },
    observe::ExactStateLookup,
};

mod pipeline;
mod runtime;

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

fn zone_block_with_user_withdrawal(
    number: u64,
    parent_hash: B256,
    imported: &TempoHeader,
    sender: Address,
    token: Address,
) -> RecoveredBlock<Block> {
    zone_block_with_user_withdrawal_marker(number, parent_hash, imported, sender, token, 0)
}

fn zone_block_with_user_withdrawal_marker(
    number: u64,
    parent_hash: B256,
    imported: &TempoHeader,
    sender: Address,
    token: Address,
    fork_marker: u8,
) -> RecoveredBlock<Block> {
    let user = TempoTxEnvelope::Legacy(Signed::new_unhashed(
        TxLegacy {
            to: ZONE_OUTBOX_ADDRESS.into(),
            ..Default::default()
        },
        Signature::new(U256::ONE, U256::from(2), false),
    ));
    let receipts = [
        zone_receipt(imported),
        user_withdrawal_receipt(sender, token),
    ];
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
            transactions: vec![advance_transaction(imported), user],
            ..Default::default()
        },
    };
    seal_zone_block(block, vec![Address::ZERO, sender], &receipts)
}

fn zone_block_with_marker(
    number: u64,
    parent_hash: B256,
    imported: &TempoHeader,
    fork_marker: u8,
) -> RecoveredBlock<Block> {
    let advance = advance_transaction(imported);
    let receipts = [zone_receipt(imported)];
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
    seal_zone_block(block, vec![Address::ZERO], &receipts)
}

fn seal_zone_block(
    mut block: Block,
    senders: Vec<Address>,
    receipts: &[TempoReceipt],
) -> RecoveredBlock<Block> {
    block.header.inner.receipts_root = TempoReceipt::calculate_receipt_root_no_memo(receipts);
    block.header.inner.logs_bloom = receipts
        .iter()
        .fold(Bloom::ZERO, |bloom, receipt| bloom | receipt.bloom());
    RecoveredBlock::new_sealed(SealedBlock::seal_slow(block), senders)
}

fn with_zone_receipts(
    block: RecoveredBlock<Block>,
    receipts: &[TempoReceipt],
) -> RecoveredBlock<Block> {
    let senders = block.senders().to_vec();
    seal_zone_block(block.into_block(), senders, receipts)
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

fn user_withdrawal_receipt(sender: Address, token: Address) -> TempoReceipt {
    TempoReceipt {
        tx_type: TempoTxType::Legacy,
        success: true,
        cumulative_gas_used: 0,
        logs: vec![
            Log {
                address: ZONE_OUTBOX_ADDRESS,
                data: IZoneOutbox::TempoGasRateUpdated { tempoGasRate: 1 }.encode_log_data(),
            },
            Log {
                address: ZONE_OUTBOX_ADDRESS,
                data: IZoneOutbox::WithdrawalRequested {
                    withdrawalIndex: 0,
                    sender,
                    token,
                    to: Address::repeat_byte(0x54),
                    amount: 10,
                    fee: 50_000,
                    memo: B256::ZERO,
                    gasLimit: 0,
                    fallbackNonce: 1,
                    data: Bytes::new(),
                    revealTo: Bytes::new(),
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

fn exact_zone_state_with_supply(
    imported: &TempoHeader,
    token: Address,
    supply: U256,
) -> TestProvider {
    let provider = zone_state(imported);
    provider.add_account(
        token,
        account_with_storage([(tip20_total_supply_access(token).storage_key(), supply)]),
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

fn l1_provider_with_collateral(
    imported: &TempoHeader,
    collateral: U256,
) -> DynProvider<TempoNetwork> {
    l1_provider_with_collateral_sequence(&[(imported, collateral)])
}

fn l1_provider_with_collateral_sequence(
    blocks: &[(&TempoHeader, U256)],
) -> DynProvider<TempoNetwork> {
    let asserter = Asserter::new();
    for (imported, collateral) in blocks {
        asserter.push_success(&Some(l1_rpc_block(imported)));
        asserter.push_success(&Some(Vec::<TempoTransactionReceipt>::new()));
        asserter.push_success(&Bytes::from(ITIP20::balanceOfCall::abi_encode_returns(
            collateral,
        )));
    }
    ProviderBuilder::new_with_network::<TempoNetwork>()
        .connect_mocked_client(asserter)
        .erased()
}
