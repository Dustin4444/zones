use alloy_consensus::{Header, Sealable as _, Signed, TxLegacy, TxReceipt as _};
use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256, Bloom, Bytes, FixedBytes, Log, Signature, U256, keccak256};
use alloy_provider::{DynProvider, Provider as _, ProviderBuilder};
use alloy_rlp::Encodable as _;
use alloy_sol_types::{SolCall, SolEvent, SolValue as _};
use alloy_transport::mock::Asserter;
use reth_primitives_traits::{RecoveredBlock, SealedBlock};
use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
use tempo_alloy::TempoNetwork;
use tempo_contracts::precompiles::ITIP20;
use tempo_primitives::{
    Block, BlockBody, TempoHeader, TempoPrimitives, TempoReceipt, TempoTxEnvelope, TempoTxType,
    transaction::envelope::TEMPO_SYSTEM_TX_SIGNATURE,
};
use tempo_zone_contracts::{IZoneInbox, IZoneOutbox, TempoState, ZONE_FACTORY_ADDRESS, ZonePortal};

use crate::{
    check::{finding::CheckError, pipeline::InMemoryChecker},
    model::{
        accounting::TokenAccounting,
        constants::{TEMPO_STATE_ADDRESS, ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS},
        encoding::{CompressedYParity, DepositPayload, OrdinaryDeposit},
        events::{L1ProtocolEvent, classify_l1_protocol_event},
        ownership::{FinalizedWithdrawal, WithdrawalIdentity},
        state::{ModelState, PortalIdentity, portal_address_for_zone},
        state_layout::{
            INBOX_PROCESSED_DEPOSIT_HASH_ACCESS, INBOX_PROCESSED_DEPOSIT_NUMBER_ACCESS,
            OUTBOX_LAST_BATCH_INDEX_ACCESS, OUTBOX_LAST_BATCH_QUEUE_HASH_ACCESS,
            TEMPO_BLOCK_HASH_ACCESS, TEMPO_BLOCK_NUMBER_ACCESS, tip20_total_supply_access,
        },
    },
    observe::{
        AuthenticatedTransaction, DecodedPortalCall, L1BlockObservation, L2BlockObservation,
        ProtocolChain, decode_portal_call, observe_l2_block,
    },
};

pub(super) const ZONE_ID: u32 = 7;
pub(super) const ZONE_NUMBER: u64 = 41;
pub(super) const TEMPO_NUMBER: u64 = 100;
pub(super) const ZONE_PARENT: B256 = B256::repeat_byte(0x91);
pub(super) const TEMPO_PARENT: B256 = B256::repeat_byte(0x90);
pub(super) const INITIAL_TOKEN: Address = Address::repeat_byte(0x20);
pub(super) const SECOND_TOKEN: Address = Address::repeat_byte(0x21);

pub(super) type L1Transaction = (B256, Option<DecodedPortalCall>, Vec<L1ProtocolEvent>);

#[derive(Debug, Clone)]
pub(super) struct ZoneUserTransaction {
    pub(super) sender: Address,
    pub(super) logs: Vec<Log>,
}

#[derive(Debug, Clone)]
pub(super) struct ZoneFinalization {
    pub(super) encrypted_senders: Vec<Bytes>,
    pub(super) event: Log,
}

#[derive(Debug, Clone)]
pub(super) struct ExactPostState {
    pub(super) tempo_hash: Option<B256>,
    pub(super) tempo_number: Option<u64>,
    pub(super) processed_hash: B256,
    pub(super) processed_number: u64,
    pub(super) withdrawal_hash: B256,
    pub(super) withdrawal_batch_index: u64,
    pub(super) supplies: Vec<(Address, U256)>,
}

impl ExactPostState {
    pub(super) fn from_model(model: &ModelState) -> Self {
        let zone = model.zone();
        Self {
            tempo_hash: None,
            tempo_number: None,
            processed_hash: zone.processed_deposit_cursor().hash(),
            processed_number: zone.processed_deposit_cursor().number(),
            withdrawal_hash: zone.last_batch().withdrawal_queue_hash(),
            withdrawal_batch_index: zone.last_batch().withdrawal_batch_index(),
            supplies: Vec::new(),
        }
    }

    pub(super) fn with_supply(mut self, token: Address, supply: impl Into<U256>) -> Self {
        self.supplies.push((token, supply.into()));
        self
    }
}

pub(super) fn portal() -> Address {
    portal_address_for_zone(ZONE_ID)
}

pub(super) fn identity(initial_token: Address) -> PortalIdentity {
    PortalIdentity::new(portal(), ZONE_ID, initial_token)
}

pub(super) fn created_model(accounting: TokenAccounting) -> ModelState {
    ModelState::created_with_zone_token_for_test(identity(INITIAL_TOKEN), accounting)
}

pub(super) fn imported_header(base_fee: u64) -> TempoHeader {
    TempoHeader {
        inner: Header {
            number: TEMPO_NUMBER,
            parent_hash: TEMPO_PARENT,
            state_root: B256::repeat_byte(0x33),
            base_fee_per_gas: Some(base_fee),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub(super) fn advance_logs(
    imported: &TempoHeader,
    mut middle: Vec<Log>,
    processed_hash: B256,
    processed_number: u64,
) -> Vec<Log> {
    let mut logs = Vec::with_capacity(middle.len() + 2);
    logs.push(zone_log(
        TEMPO_STATE_ADDRESS,
        TempoState::TempoBlockFinalized {
            blockHash: imported.hash_slow(),
            blockNumber: imported.inner.number,
            stateRoot: imported.inner.state_root,
        },
    ));
    logs.append(&mut middle);
    logs.push(zone_log(
        ZONE_INBOX_ADDRESS,
        IZoneInbox::TempoAdvanced {
            tempoBlockHash: imported.hash_slow(),
            tempoBlockNumber: imported.inner.number,
            depositsProcessed: U256::from(processed_number),
            newProcessedDepositQueueHash: processed_hash,
            lastProcessedDepositNumber: processed_number,
        },
    ));
    logs
}

pub(super) fn zone_log<E: SolEvent>(address: Address, event: E) -> Log {
    Log {
        address,
        data: event.encode_log_data(),
    }
}

pub(super) fn portal_event<E: SolEvent>(event: E) -> L1ProtocolEvent {
    classify_l1_protocol_event(portal(), &zone_log(portal(), event))
        .unwrap()
        .expect("Portal event must be model relevant")
}

pub(super) fn factory_event<E: SolEvent>(event: E) -> L1ProtocolEvent {
    classify_l1_protocol_event(portal(), &zone_log(ZONE_FACTORY_ADDRESS, event))
        .unwrap()
        .expect("factory event must be model relevant")
}

pub(super) fn direct_call<C: SolCall>(call: &C) -> DecodedPortalCall {
    decode_portal_call(
        &call.abi_encode(),
        AuthenticatedTransaction::new(ProtocolChain::TempoL1, 0, B256::ZERO),
    )
    .unwrap()
}

pub(super) fn l1_transaction(
    seed: u8,
    call: Option<DecodedPortalCall>,
    events: Vec<L1ProtocolEvent>,
) -> L1Transaction {
    (B256::repeat_byte(seed), call, events)
}

pub(super) fn zone_observation(
    imported: &TempoHeader,
    deposits: Vec<IZoneInbox::QueuedDeposit>,
    enabled_tokens: Vec<IZoneInbox::EnabledToken>,
    advance_logs: Vec<Log>,
    users: Vec<ZoneUserTransaction>,
    finalization: Option<ZoneFinalization>,
) -> L2BlockObservation {
    let ordinary_count = deposits
        .iter()
        .filter(|deposit| deposit.depositType == IZoneInbox::DepositType::Deposit)
        .count();
    let decryptions = (0..ordinary_count)
        .map(|_| IZoneInbox::DecryptionData {
            sharedSecret: B256::ZERO,
            sharedSecretYParity: 2,
            cpProof: IZoneInbox::ChaumPedersenProof {
                s: B256::ZERO,
                c: B256::ZERO,
            },
        })
        .collect();

    let advance = system_transaction(
        ZONE_INBOX_ADDRESS,
        IZoneInbox::advanceTempoCall {
            header: encode_header(imported),
            deposits,
            decryptions,
            enabledTokens: enabled_tokens,
        }
        .abi_encode()
        .into(),
    );
    let mut transactions = vec![advance];
    let mut senders = vec![Address::ZERO];
    let mut receipts = vec![receipt(advance_logs)];
    for (index, user) in users.into_iter().enumerate() {
        transactions.push(user_transaction(index as u64));
        senders.push(user.sender);
        receipts.push(receipt(user.logs));
    }
    if let Some(finalization) = finalization {
        transactions.push(system_transaction(
            ZONE_OUTBOX_ADDRESS,
            IZoneOutbox::finalizeWithdrawalBatchCall {
                count: U256::from(finalization.encrypted_senders.len()),
                blockNumber: ZONE_NUMBER,
                encryptedSenders: finalization.encrypted_senders,
            }
            .abi_encode()
            .into(),
        ));
        senders.push(Address::ZERO);
        receipts.push(receipt(vec![finalization.event]));
    }

    let receipts_root = TempoReceipt::calculate_receipt_root_no_memo(&receipts);
    let logs_bloom = receipts
        .iter()
        .fold(Bloom::ZERO, |bloom, receipt| bloom | receipt.bloom());
    let block = Block {
        header: TempoHeader {
            inner: Header {
                number: ZONE_NUMBER,
                parent_hash: ZONE_PARENT,
                receipts_root,
                logs_bloom,
                ..Default::default()
            },
            ..Default::default()
        },
        body: BlockBody {
            transactions,
            ..Default::default()
        },
    };
    let recovered = RecoveredBlock::new_sealed(SealedBlock::seal_slow(block), senders);
    observe_l2_block(&recovered, &receipts).unwrap()
}

pub(super) async fn run_valid_block(
    model: ModelState,
    imported: &TempoHeader,
    l1_transactions: Vec<L1Transaction>,
    l2: &L2BlockObservation,
    collateral: &[U256],
    exact: ExactPostState,
    creation_block: bool,
) -> InMemoryChecker {
    let (checker, result) = run_block(
        model,
        imported,
        l1_transactions,
        l2,
        collateral,
        exact,
        creation_block,
    )
    .await;
    result.unwrap();
    assert_eq!(
        checker.zone_tip(),
        BlockNumHash::new(l2.block_number(), l2.block_hash())
    );
    assert_eq!(
        checker.tempo_tip(),
        BlockNumHash::new(imported.inner.number, imported.hash_slow())
    );
    checker
}

pub(super) async fn run_block(
    model: ModelState,
    imported: &TempoHeader,
    l1_transactions: Vec<L1Transaction>,
    l2: &L2BlockObservation,
    collateral: &[U256],
    exact: ExactPostState,
    creation_block: bool,
) -> (InMemoryChecker, Result<(), CheckError>) {
    let l1 = L1BlockObservation::with_calls_for_test(
        imported.inner.number,
        imported.hash_slow(),
        portal(),
        l1_transactions,
    );
    let provider = collateral_provider(collateral);
    let exact_provider = exact_provider(imported, &exact);
    let creation_hash = if creation_block {
        imported.hash_slow()
    } else {
        B256::repeat_byte(0xcc)
    };
    let creation_number = if creation_block {
        TEMPO_NUMBER
    } else {
        TEMPO_NUMBER + 1
    };
    let mut checker = InMemoryChecker::new(
        model,
        BlockNumHash::new(creation_number, creation_hash),
        BlockNumHash::new(ZONE_NUMBER - 1, ZONE_PARENT),
        BlockNumHash::new(TEMPO_NUMBER - 1, TEMPO_PARENT),
    );
    let result = checker
        .check_block(&provider, &exact_provider, &l1, l2)
        .await;
    (checker, result)
}

pub(super) fn ordinary(token: Address, seed: u8, amount: u128) -> ZonePortal::Deposit {
    ZonePortal::Deposit {
        token,
        sender: Address::repeat_byte(seed),
        amount,
        tempoRefundRecipient: Address::repeat_byte(seed.wrapping_add(1)),
        keyIndex: U256::from(seed),
        encrypted: ZonePortal::DepositPayload {
            ephemeralPubkeyX: B256::repeat_byte(seed.wrapping_add(2)),
            ephemeralPubkeyYParity: if seed.is_multiple_of(2) { 2 } else { 3 },
            ciphertext: Bytes::from(vec![seed.wrapping_add(3); 64]),
            nonce: FixedBytes::repeat_byte(seed.wrapping_add(4)),
            tag: FixedBytes::repeat_byte(seed.wrapping_add(5)),
        },
    }
}

pub(super) fn queued_ordinary(deposit: &ZonePortal::Deposit) -> IZoneInbox::QueuedDeposit {
    IZoneInbox::QueuedDeposit {
        depositType: IZoneInbox::DepositType::Deposit,
        depositData: deposit.abi_encode().into(),
    }
}

pub(super) fn model_ordinary(deposit: &ZonePortal::Deposit) -> OrdinaryDeposit {
    OrdinaryDeposit::new(
        deposit.token,
        deposit.sender,
        deposit.amount,
        deposit.tempoRefundRecipient,
        deposit.keyIndex,
        DepositPayload::new(
            deposit.encrypted.ephemeralPubkeyX,
            match deposit.encrypted.ephemeralPubkeyYParity {
                2 => CompressedYParity::Even,
                3 => CompressedYParity::Odd,
                parity => panic!("fixture compressed Y parity must be 2 or 3, got {parity}"),
            },
            FixedBytes::from_slice(&deposit.encrypted.ciphertext),
            deposit.encrypted.nonce,
            deposit.encrypted.tag,
        ),
    )
}

pub(super) fn queued_bounce(
    deposit: &IZoneInbox::WithdrawalBounceBackDeposit,
) -> IZoneInbox::QueuedDeposit {
    IZoneInbox::QueuedDeposit {
        depositType: IZoneInbox::DepositType::WithdrawalBounceBack,
        depositData: deposit.abi_encode().into(),
    }
}

pub(super) fn independent_ordinary_queue_hash(
    deposit: &ZonePortal::Deposit,
    previous: B256,
) -> B256 {
    keccak256((IZoneInbox::DepositType::Deposit, deposit.clone(), previous).abi_encode_params())
}

pub(super) fn independent_bounce_queue_hash(
    deposit: &IZoneInbox::WithdrawalBounceBackDeposit,
    previous: B256,
) -> B256 {
    keccak256(
        (
            IZoneInbox::DepositType::WithdrawalBounceBack,
            deposit.clone(),
            previous,
        )
            .abi_encode_params(),
    )
}

pub(super) fn independent_sender_tag(sender: Address, transaction_hash: B256) -> B256 {
    let mut preimage = [0_u8; 52];
    preimage[..20].copy_from_slice(sender.as_slice());
    preimage[20..].copy_from_slice(transaction_hash.as_slice());
    keccak256(preimage)
}

pub(super) fn fallback_recipient(nonce: u64) -> Address {
    let mut bytes = [0_u8; 20];
    bytes[12..].copy_from_slice(&nonce.to_be_bytes());
    Address::from(bytes)
}

pub(super) fn independent_withdrawal_queue_hash(withdrawals: &[ZonePortal::Withdrawal]) -> B256 {
    if withdrawals.is_empty() {
        return B256::ZERO;
    }
    withdrawals
        .iter()
        .rev()
        .fold(tempo_zone_contracts::EMPTY_SENTINEL, |tail, withdrawal| {
            keccak256((withdrawal.clone(), tail).abi_encode_params())
        })
}

pub(super) fn portal_withdrawal(finalized: &FinalizedWithdrawal) -> ZonePortal::Withdrawal {
    let withdrawal = finalized.preimage();
    let (sender_tag, fallback_nonce) = match finalized.identity() {
        WithdrawalIdentity::User(identity) => (
            independent_sender_tag(identity.sender(), identity.tx_hash()),
            identity.fallback_nonce().get(),
        ),
        WithdrawalIdentity::FailedDeposit { .. } => {
            (independent_sender_tag(Address::ZERO, B256::ZERO), 0)
        }
    };
    ZonePortal::Withdrawal {
        token: withdrawal.token(),
        senderTag: sender_tag,
        to: withdrawal.to(),
        amount: withdrawal.amount(),
        memo: withdrawal.memo(),
        gasLimit: withdrawal.gas_limit(),
        fallbackNonce: fallback_nonce,
        callbackData: withdrawal.callback_data().clone(),
        encryptedSender: withdrawal.encrypted_sender().clone(),
    }
}

fn encode_header(header: &TempoHeader) -> Bytes {
    let mut encoded = Vec::new();
    header.encode(&mut encoded);
    encoded.into()
}

fn system_transaction(to: Address, input: Bytes) -> TempoTxEnvelope {
    TempoTxEnvelope::Legacy(Signed::new_unhashed(
        TxLegacy {
            gas_limit: 0,
            to: to.into(),
            input,
            ..Default::default()
        },
        TEMPO_SYSTEM_TX_SIGNATURE,
    ))
}

fn user_transaction(nonce: u64) -> TempoTxEnvelope {
    TempoTxEnvelope::Legacy(Signed::new_unhashed(
        TxLegacy {
            nonce,
            to: ZONE_OUTBOX_ADDRESS.into(),
            input: Bytes::from(vec![nonce as u8]),
            ..Default::default()
        },
        Signature::new(U256::ONE, U256::from(2), false),
    ))
}

fn receipt(logs: Vec<Log>) -> TempoReceipt {
    TempoReceipt {
        tx_type: TempoTxType::Legacy,
        success: true,
        cumulative_gas_used: 0,
        logs,
    }
}

fn collateral_provider(balances: &[U256]) -> DynProvider<TempoNetwork> {
    let asserter = Asserter::new();
    for balance in balances {
        asserter.push_success(&Bytes::from(ITIP20::balanceOfCall::abi_encode_returns(
            balance,
        )));
    }
    ProviderBuilder::new_with_network::<TempoNetwork>()
        .connect_mocked_client(asserter)
        .erased()
}

fn exact_provider(
    imported: &TempoHeader,
    expected: &ExactPostState,
) -> MockEthProvider<TempoPrimitives> {
    let provider = MockEthProvider::new();
    provider.add_account(
        TEMPO_BLOCK_HASH_ACCESS.address,
        ExtendedAccount::new(0, U256::ZERO).extend_storage([
            (
                TEMPO_BLOCK_HASH_ACCESS.storage_key(),
                U256::from_be_slice(
                    expected
                        .tempo_hash
                        .unwrap_or_else(|| imported.hash_slow())
                        .as_slice(),
                ),
            ),
            (
                TEMPO_BLOCK_NUMBER_ACCESS.storage_key(),
                U256::from(expected.tempo_number.unwrap_or(imported.inner.number)),
            ),
        ]),
    );
    provider.add_account(
        INBOX_PROCESSED_DEPOSIT_HASH_ACCESS.address,
        ExtendedAccount::new(0, U256::ZERO).extend_storage([
            (
                INBOX_PROCESSED_DEPOSIT_HASH_ACCESS.storage_key(),
                U256::from_be_slice(expected.processed_hash.as_slice()),
            ),
            (
                INBOX_PROCESSED_DEPOSIT_NUMBER_ACCESS.storage_key(),
                U256::from(expected.processed_number),
            ),
        ]),
    );
    provider.add_account(
        OUTBOX_LAST_BATCH_QUEUE_HASH_ACCESS.address,
        ExtendedAccount::new(0, U256::ZERO).extend_storage([
            (
                OUTBOX_LAST_BATCH_QUEUE_HASH_ACCESS.storage_key(),
                U256::from_be_slice(expected.withdrawal_hash.as_slice()),
            ),
            (
                OUTBOX_LAST_BATCH_INDEX_ACCESS.storage_key(),
                U256::from(expected.withdrawal_batch_index),
            ),
        ]),
    );
    for (token, supply) in &expected.supplies {
        provider.add_account(
            *token,
            ExtendedAccount::new(0, U256::ZERO)
                .extend_storage([(tip20_total_supply_access(*token).storage_key(), *supply)]),
        );
    }
    provider
}
