use alloy_consensus::{Header, ReceiptWithBloom, Signed, TxLegacy, transaction::Recovered};
use alloy_eips::{BlockNumHash, Encodable2718 as _};
use alloy_primitives::{Address, B256, Bloom, Bytes, Log, Signature, U64, U256};
use alloy_provider::{DynProvider, Provider as _, ProviderBuilder};
use alloy_rpc_types_eth::{
    Block, BlockTransactions, Header as RpcHeader, Log as RpcLog, Transaction, TransactionReceipt,
};
use alloy_sol_types::SolCall as _;
use alloy_transport::mock::Asserter;
use tempo_alloy::{
    TempoNetwork,
    rpc::{TempoHeaderResponse, TempoTransactionReceipt},
};
use tempo_contracts::precompiles::ITIP20;
use tempo_primitives::{TempoHeader, TempoReceipt, TempoTxEnvelope, TempoTxType};

use super::{DEPOSIT_AMOUNT, DevelopmentFixture, L1_CHAIN_ID, PreGenesisFixture};
use crate::observe::ImportedTempoHeader;

type L1RpcBlock = Block<Transaction<TempoTxEnvelope>, TempoHeaderResponse>;

#[derive(Clone)]
pub(crate) struct AuthenticatedBlock {
    pub(super) header: ImportedTempoHeader,
    response: L1RpcBlock,
    receipts: Vec<TempoTransactionReceipt>,
}

impl AuthenticatedBlock {
    pub(crate) fn new(number: u64, parent_hash: B256, logs: Vec<Log>) -> Self {
        let envelope = TempoTxEnvelope::Legacy(Signed::new_unhashed(
            TxLegacy {
                nonce: number,
                ..Default::default()
            },
            Signature::test_signature(),
        ));
        let transaction_hash = envelope.trie_hash();
        let rpc_logs = logs
            .into_iter()
            .enumerate()
            .map(|(index, inner)| RpcLog {
                inner,
                block_hash: None,
                block_number: None,
                block_timestamp: None,
                transaction_hash: Some(transaction_hash),
                transaction_index: Some(0),
                log_index: Some(index as u64),
                removed: false,
            })
            .collect::<Vec<_>>();
        let mut bloom = Bloom::ZERO;
        for log in &rpc_logs {
            bloom.accrue_log(&log.inner);
        }
        let receipt = TempoTransactionReceipt {
            inner: TransactionReceipt {
                inner: ReceiptWithBloom::new(
                    TempoReceipt::<RpcLog> {
                        tx_type: TempoTxType::Legacy,
                        success: true,
                        cumulative_gas_used: 21_000,
                        logs: rpc_logs,
                    },
                    bloom,
                ),
                transaction_hash,
                transaction_index: Some(0),
                block_hash: None,
                block_number: None,
                gas_used: 21_000,
                effective_gas_price: 0,
                blob_gas_used: None,
                blob_gas_price: None,
                from: Address::repeat_byte(0x11),
                to: None,
                contract_address: None,
            },
            fee_token: None,
            fee_payer: Address::ZERO,
        };
        let consensus = receipt
            .inner
            .inner
            .clone()
            .map_receipt(|receipt| receipt.map_logs(Into::into));
        let tempo = TempoHeader {
            inner: Header {
                number,
                parent_hash,
                receipts_root: alloy_consensus::proofs::calculate_receipt_root(
                    std::slice::from_ref(&consensus),
                ),
                transactions_root: alloy_consensus::proofs::calculate_transaction_root(
                    std::slice::from_ref(&envelope),
                ),
                logs_bloom: *consensus.bloom_ref(),
                base_fee_per_gas: Some(1),
                ..Default::default()
            },
            ..Default::default()
        };
        let header = ImportedTempoHeader::for_test(tempo);
        let hash = header.hash();
        let mut receipt = receipt;
        receipt.inner.block_hash = Some(hash);
        receipt.inner.block_number = Some(number);
        for log in &mut receipt.inner.inner.receipt.logs {
            log.block_hash = Some(hash);
            log.block_number = Some(number);
        }
        let transaction = Transaction {
            inner: Recovered::new_unchecked(envelope, Address::repeat_byte(0x11)),
            block_hash: Some(hash),
            block_number: Some(number),
            transaction_index: Some(0),
            effective_gas_price: None,
            block_timestamp: None,
        };
        let response = Block {
            header: TempoHeaderResponse {
                inner: RpcHeader {
                    hash,
                    inner: header.header().clone(),
                    total_difficulty: None,
                    size: None,
                },
                timestamp_millis: 0,
            },
            uncles: Vec::new(),
            transactions: BlockTransactions::Full(vec![transaction]),
            withdrawals: None,
        };
        Self {
            header,
            response,
            receipts: vec![receipt],
        }
    }

    pub(crate) fn tip(&self) -> BlockNumHash {
        BlockNumHash::new(self.header.number(), self.header.hash())
    }
}

pub(crate) struct RpcScript {
    asserter: Asserter,
}

impl RpcScript {
    pub(crate) fn new() -> Self {
        Self {
            asserter: Asserter::new(),
        }
    }

    pub(crate) fn provider(&self) -> DynProvider<TempoNetwork> {
        ProviderBuilder::new_with_network::<TempoNetwork>()
            .connect_mocked_client(self.asserter.clone())
            .erased()
    }

    pub(crate) fn push_chain_id(&self) {
        self.asserter.push_success(&U64::from(L1_CHAIN_ID));
    }

    pub(crate) fn push_chain_id_value(&self, chain_id: u64) {
        self.asserter.push_success(&U64::from(chain_id));
    }

    fn push_header(&self, block: &AuthenticatedBlock) {
        self.asserter.push_success(&Some(block.response.clone()));
    }

    pub(crate) fn push_missing_block(&self) {
        self.asserter.push_success(&Option::<L1RpcBlock>::None);
    }

    pub(crate) fn push_observation(&self, block: &AuthenticatedBlock) {
        self.push_header(block);
        self.asserter.push_success(&Some(block.receipts.clone()));
    }

    pub(crate) fn push_balance(&self, balance: U256) {
        self.asserter
            .push_success(&Bytes::from(ITIP20::balanceOfCall::abi_encode_returns(
                &balance,
            )));
    }

    pub(crate) fn push_full_fresh_replay(&self, fixture: &PreGenesisFixture) {
        self.push_chain_id();
        self.push_complete_l1_replay(fixture);
    }

    pub(crate) fn push_full_resume(&self, fixture: &PreGenesisFixture) {
        self.push_complete_l1_replay(fixture);
    }

    fn push_complete_l1_replay(&self, fixture: &PreGenesisFixture) {
        self.push_header(&fixture.anchor);
        self.push_header(&fixture.creation);
        self.push_observation(&fixture.creation);
        self.push_balance(U256::ZERO);
        self.push_observation(&fixture.anchor);
        self.push_balance(U256::from(DEPOSIT_AMOUNT));
    }

    pub(crate) fn push_creation_authentication_prefix(&self, fixture: &PreGenesisFixture) {
        self.push_chain_id();
        self.push_header(&fixture.anchor);
        self.push_header(&fixture.creation);
        self.push_observation(&fixture.creation);
    }

    pub(crate) fn push_resume_after_creation(&self, fixture: &PreGenesisFixture) {
        self.push_header(&fixture.anchor);
        self.push_observation(&fixture.anchor);
        self.push_balance(U256::from(DEPOSIT_AMOUNT));
    }

    pub(crate) fn push_development_fresh(&self, fixture: &DevelopmentFixture) {
        self.push_chain_id();
        self.push_header(&fixture.anchor);
        self.push_header(&fixture.creation);
        self.push_observation(&fixture.creation);
        self.push_balance(U256::ZERO);
    }

    pub(crate) fn assert_consumed(&self) {
        assert!(
            self.asserter.read_q().is_empty(),
            "mock RPC script retained unused responses"
        );
    }
}
