use alloy_consensus::{Header, Signed, TxLegacy, TxReceipt as _};
use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256, Bloom, U256};
use alloy_rlp::Encodable as _;
use alloy_sol_types::{SolCall as _, SolValue as _};
use reth_primitives_traits::{RecoveredBlock, SealedBlock};
use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
use tempo_primitives::{
    Block, BlockBody, TempoHeader, TempoPrimitives, TempoReceipt, TempoTxEnvelope, TempoTxType,
    transaction::envelope::TEMPO_SYSTEM_TX_SIGNATURE,
};
use tempo_zone_contracts::IZoneInbox;

use super::{
    DEPOSIT_AMOUNT, DevelopmentFixture, INITIAL_TOKEN, PreGenesisFixture,
    l1::{ordinary_deposit, protocol_log},
    rpc::AuthenticatedBlock,
};
use crate::model::{
    constants::{TEMPO_STATE_ADDRESS, ZONE_INBOX_ADDRESS},
    state_layout::{
        DEFAULT_FEE_TOKEN_ACCESS, INBOX_PROCESSED_DEPOSIT_HASH_ACCESS,
        INBOX_PROCESSED_DEPOSIT_NUMBER_ACCESS, OUTBOX_LAST_BATCH_INDEX_ACCESS,
        OUTBOX_LAST_BATCH_QUEUE_HASH_ACCESS, TEMPO_BLOCK_HASH_ACCESS, TEMPO_BLOCK_NUMBER_ACCESS,
        tip20_total_supply_access,
    },
};

type ZoneProvider = MockEthProvider<TempoPrimitives>;

struct GenesisProgress {
    processed_deposit_queue_hash: B256,
    processed_deposit_number: u64,
    withdrawal_queue_hash: B256,
    withdrawal_batch_index: u64,
}

impl GenesisProgress {
    const ZERO: Self = Self {
        processed_deposit_queue_hash: B256::ZERO,
        processed_deposit_number: 0,
        withdrawal_queue_hash: B256::ZERO,
        withdrawal_batch_index: 0,
    };
}

impl DevelopmentFixture {
    pub(in crate::runtime::bootstrap::tests::e2e) fn creation_zone_block(
        &self,
    ) -> (RecoveredBlock<Block>, Vec<TempoReceipt>, ZoneProvider) {
        let enabled = IZoneInbox::EnabledToken {
            token: self.initial_token,
            name: "Initial Token".into(),
            symbol: "INIT".into(),
            currency: "USD".into(),
        };
        let mut encoded_header = Vec::new();
        self.creation.header.header().encode(&mut encoded_header);
        let advance = TempoTxEnvelope::Legacy(Signed::new_unhashed(
            TxLegacy {
                to: ZONE_INBOX_ADDRESS.into(),
                input: IZoneInbox::advanceTempoCall {
                    header: encoded_header.into(),
                    deposits: Vec::new(),
                    decryptions: Vec::new(),
                    enabledTokens: vec![enabled.clone()],
                }
                .abi_encode()
                .into(),
                ..Default::default()
            },
            TEMPO_SYSTEM_TX_SIGNATURE,
        ));
        let mut block = Block {
            header: TempoHeader {
                inner: Header {
                    number: 1,
                    parent_hash: self.zone_genesis,
                    ..Default::default()
                },
                ..Default::default()
            },
            body: BlockBody {
                transactions: vec![advance],
                ..Default::default()
            },
        };
        let receipts = vec![TempoReceipt {
            tx_type: TempoTxType::Legacy,
            success: true,
            cumulative_gas_used: 0,
            logs: vec![
                protocol_log(
                    TEMPO_STATE_ADDRESS,
                    tempo_zone_contracts::TempoState::TempoBlockFinalized {
                        blockHash: self.creation.tip().hash,
                        blockNumber: self.creation.tip().number,
                        stateRoot: self.creation.header.header().inner.state_root,
                    },
                ),
                protocol_log(
                    ZONE_INBOX_ADDRESS,
                    IZoneInbox::TokenEnabled {
                        token: enabled.token,
                        name: enabled.name,
                        symbol: enabled.symbol,
                        currency: enabled.currency,
                    },
                ),
                protocol_log(
                    ZONE_INBOX_ADDRESS,
                    IZoneInbox::TempoAdvanced {
                        tempoBlockHash: self.creation.tip().hash,
                        tempoBlockNumber: self.creation.tip().number,
                        depositsProcessed: U256::ZERO,
                        newProcessedDepositQueueHash: B256::ZERO,
                        lastProcessedDepositNumber: 0,
                    },
                ),
            ],
        }];
        block.header.inner.receipts_root = TempoReceipt::calculate_receipt_root_no_memo(&receipts);
        block.header.inner.logs_bloom = receipts
            .iter()
            .fold(Bloom::ZERO, |bloom, receipt| bloom | receipt.bloom());
        let block = RecoveredBlock::new_sealed(SealedBlock::seal_slow(block), vec![Address::ZERO]);
        let exact = exact_zone_state(
            block.hash(),
            self.creation.tip(),
            self.initial_token,
            U256::ZERO,
        );
        (block, receipts, exact)
    }
}

impl PreGenesisFixture {
    /// First Zone child after a genesis anchor that already authenticated the
    /// Portal token and one queued deposit. Neither chain repeats the token
    /// enablement at this boundary.
    pub(super) fn first_post_genesis_block(
        &self,
    ) -> (
        AuthenticatedBlock,
        RecoveredBlock<Block>,
        Vec<TempoReceipt>,
        ZoneProvider,
    ) {
        let imported = AuthenticatedBlock::new(12, self.anchor.tip().hash, Vec::new());
        let deposit = ordinary_deposit(self.initial_token);
        let mut encoded_header = Vec::new();
        imported.header.header().encode(&mut encoded_header);
        let advance = TempoTxEnvelope::Legacy(Signed::new_unhashed(
            TxLegacy {
                to: ZONE_INBOX_ADDRESS.into(),
                input: IZoneInbox::advanceTempoCall {
                    header: encoded_header.into(),
                    deposits: vec![IZoneInbox::QueuedDeposit {
                        depositType: IZoneInbox::DepositType::Deposit,
                        depositData: deposit.abi_encode().into(),
                    }],
                    decryptions: vec![IZoneInbox::DecryptionData {
                        sharedSecret: B256::ZERO,
                        sharedSecretYParity: 2,
                        cpProof: IZoneInbox::ChaumPedersenProof {
                            s: B256::ZERO,
                            c: B256::ZERO,
                        },
                    }],
                    enabledTokens: Vec::new(),
                }
                .abi_encode()
                .into(),
                ..Default::default()
            },
            TEMPO_SYSTEM_TX_SIGNATURE,
        ));
        let receipts = vec![TempoReceipt {
            tx_type: TempoTxType::Legacy,
            success: true,
            cumulative_gas_used: 0,
            logs: vec![
                protocol_log(
                    TEMPO_STATE_ADDRESS,
                    tempo_zone_contracts::TempoState::TempoBlockFinalized {
                        blockHash: imported.tip().hash,
                        blockNumber: imported.tip().number,
                        stateRoot: imported.header.header().inner.state_root,
                    },
                ),
                protocol_log(
                    ZONE_INBOX_ADDRESS,
                    IZoneInbox::DepositProcessed {
                        depositHash: self.deposit_queue_hash,
                        sender: deposit.sender,
                        to: Address::repeat_byte(0x61),
                        token: deposit.token,
                        amount: deposit.amount,
                        memo: B256::repeat_byte(0x62),
                    },
                ),
                protocol_log(
                    ZONE_INBOX_ADDRESS,
                    IZoneInbox::TempoAdvanced {
                        tempoBlockHash: imported.tip().hash,
                        tempoBlockNumber: imported.tip().number,
                        depositsProcessed: U256::from(1),
                        newProcessedDepositQueueHash: self.deposit_queue_hash,
                        lastProcessedDepositNumber: 1,
                    },
                ),
            ],
        }];
        let mut block = Block {
            header: TempoHeader {
                inner: Header {
                    number: 1,
                    parent_hash: self.zone_genesis,
                    ..Default::default()
                },
                ..Default::default()
            },
            body: BlockBody {
                transactions: vec![advance],
                ..Default::default()
            },
        };
        block.header.inner.receipts_root = TempoReceipt::calculate_receipt_root_no_memo(&receipts);
        block.header.inner.logs_bloom = receipts
            .iter()
            .fold(Bloom::ZERO, |bloom, receipt| bloom | receipt.bloom());
        let block = RecoveredBlock::new_sealed(SealedBlock::seal_slow(block), vec![Address::ZERO]);
        let exact = exact_zone_state_at(
            block.hash(),
            imported.tip(),
            self.initial_token,
            self.deposit_queue_hash,
            1,
            U256::from(DEPOSIT_AMOUNT),
        );
        (imported, block, receipts, exact)
    }
}

pub(crate) fn zone_provider(zone_genesis: B256, anchor: BlockNumHash) -> ZoneProvider {
    zone_provider_with_initial_token(zone_genesis, anchor, INITIAL_TOKEN)
}

pub(crate) fn zone_provider_with_initial_token(
    zone_genesis: B256,
    anchor: BlockNumHash,
    initial_token: Address,
) -> ZoneProvider {
    zone_provider_with_genesis_configuration(
        zone_genesis,
        anchor,
        initial_token,
        GenesisProgress::ZERO,
    )
}

pub(crate) fn zone_provider_with_genesis_supply(
    zone_genesis: B256,
    anchor: BlockNumHash,
    token: Address,
    supply: U256,
) -> ZoneProvider {
    let provider = zone_provider(zone_genesis, anchor);
    provider.add_account(
        token,
        ExtendedAccount::new(0, U256::ZERO)
            .extend_storage([(tip20_total_supply_access(token).storage_key(), supply)]),
    );
    provider
}

pub(crate) fn zone_provider_with_genesis_progress(
    zone_genesis: B256,
    anchor: BlockNumHash,
    processed_deposit_queue_hash: B256,
    processed_deposit_number: u64,
    withdrawal_queue_hash: B256,
    withdrawal_batch_index: u64,
) -> ZoneProvider {
    zone_provider_with_genesis_configuration(
        zone_genesis,
        anchor,
        INITIAL_TOKEN,
        GenesisProgress {
            processed_deposit_queue_hash,
            processed_deposit_number,
            withdrawal_queue_hash,
            withdrawal_batch_index,
        },
    )
}

fn zone_provider_with_genesis_configuration(
    zone_genesis: B256,
    anchor: BlockNumHash,
    initial_token: Address,
    progress: GenesisProgress,
) -> ZoneProvider {
    let provider = ZoneProvider::new();
    provider.add_header(
        zone_genesis,
        TempoHeader {
            inner: Header {
                number: 0,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    provider.add_account(
        TEMPO_BLOCK_HASH_ACCESS.address,
        ExtendedAccount::new(0, U256::ZERO).extend_storage([
            (
                TEMPO_BLOCK_HASH_ACCESS.storage_key(),
                U256::from_be_slice(anchor.hash.as_slice()),
            ),
            (
                TEMPO_BLOCK_NUMBER_ACCESS.storage_key(),
                U256::from(anchor.number),
            ),
        ]),
    );
    provider.add_account(
        INBOX_PROCESSED_DEPOSIT_HASH_ACCESS.address,
        ExtendedAccount::new(0, U256::ZERO).extend_storage([
            (
                INBOX_PROCESSED_DEPOSIT_HASH_ACCESS.storage_key(),
                U256::from_be_slice(progress.processed_deposit_queue_hash.as_slice()),
            ),
            (
                INBOX_PROCESSED_DEPOSIT_NUMBER_ACCESS.storage_key(),
                U256::from(progress.processed_deposit_number),
            ),
        ]),
    );
    provider.add_account(
        OUTBOX_LAST_BATCH_QUEUE_HASH_ACCESS.address,
        ExtendedAccount::new(0, U256::ZERO).extend_storage([
            (
                OUTBOX_LAST_BATCH_QUEUE_HASH_ACCESS.storage_key(),
                U256::from_be_slice(progress.withdrawal_queue_hash.as_slice()),
            ),
            (
                OUTBOX_LAST_BATCH_INDEX_ACCESS.storage_key(),
                U256::from(progress.withdrawal_batch_index),
            ),
        ]),
    );
    provider.add_account(
        DEFAULT_FEE_TOKEN_ACCESS.address,
        ExtendedAccount::new(0, U256::ZERO).extend_storage([(
            DEFAULT_FEE_TOKEN_ACCESS.storage_key(),
            U256::from_be_slice(B256::left_padding_from(initial_token.as_slice()).as_slice()),
        )]),
    );
    provider
}

fn exact_zone_state(
    block_hash: B256,
    tempo: BlockNumHash,
    token: Address,
    supply: U256,
) -> ZoneProvider {
    exact_zone_state_at(block_hash, tempo, token, B256::ZERO, 0, supply)
}

fn exact_zone_state_at(
    block_hash: B256,
    tempo: BlockNumHash,
    token: Address,
    processed_deposit_queue_hash: B256,
    processed_deposit_number: u64,
    supply: U256,
) -> ZoneProvider {
    let provider = zone_provider_with_genesis_progress(
        block_hash,
        tempo,
        processed_deposit_queue_hash,
        processed_deposit_number,
        B256::ZERO,
        0,
    );
    provider.add_account(
        token,
        ExtendedAccount::new(0, U256::ZERO)
            .extend_storage([(tip20_total_supply_access(token).storage_key(), supply)]),
    );
    provider
}
