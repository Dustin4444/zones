use alloy_consensus::{
    Header, Sealable as _, SignableTransaction as _, Signed, TxLegacy, transaction::TxHashRef as _,
};
use alloy_primitives::{Address, B256, Bytes, Log, LogData, Signature, U256, b256};
use alloy_rlp::Encodable as _;
use alloy_sol_types::{SolCall as _, SolEvent as _};
use reth_primitives_traits::{RecoveredBlock, SealedBlock};
use tempo_primitives::{
    Block, BlockBody, TempoHeader, TempoReceipt, TempoTxEnvelope, TempoTxType,
    transaction::envelope::TEMPO_SYSTEM_TX_SIGNATURE,
};
use tempo_zone_contracts::{IZoneInbox, IZoneOutbox, TempoState};

use super::*;
use crate::{
    model::{
        constants::{TEMPO_STATE_ADDRESS, ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS},
        events::{L2ProtocolEvent, Outbox},
    },
    observe::{
        error::{
            AcquisitionError, AcquisitionSource, DataSource, EnvelopeRule, MismatchValue,
            ObservationError, OutputField, ProtocolChain,
        },
        state::ZonePostStateOutputs,
    },
};

const ZONE_NUMBER: u64 = 9;

fn imported_header() -> TempoHeader {
    TempoHeader {
        inner: Header {
            number: 100,
            state_root: B256::repeat_byte(0x31),
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

fn advance_transaction(to: Address) -> TempoTxEnvelope {
    advance_transaction_with_tokens(to, Vec::new())
}

fn advance_transaction_with_tokens(
    to: Address,
    enabled_tokens: Vec<IZoneInbox::EnabledToken>,
) -> TempoTxEnvelope {
    let calldata = IZoneInbox::advanceTempoCall {
        header: encode_header(&imported_header()),
        deposits: Vec::new(),
        decryptions: Vec::new(),
        enabledTokens: enabled_tokens,
    }
    .abi_encode();
    system_transaction(to, calldata.into())
}

fn token_enabled_log(symbol: &str) -> Log {
    Log {
        address: ZONE_INBOX_ADDRESS,
        data: IZoneInbox::TokenEnabled {
            token: Address::repeat_byte(0x71),
            name: "Token".into(),
            symbol: symbol.into(),
            currency: "USD".into(),
        }
        .encode_log_data(),
    }
}

fn finalization_transaction(block_number: u64) -> TempoTxEnvelope {
    let calldata = IZoneOutbox::finalizeWithdrawalBatchCall {
        count: U256::ZERO,
        blockNumber: block_number,
        encryptedSenders: Vec::new(),
    }
    .abi_encode();
    system_transaction(ZONE_OUTBOX_ADDRESS, calldata.into())
}

fn system_transaction(to: Address, input: Bytes) -> TempoTxEnvelope {
    TempoTxEnvelope::Legacy(Signed::new_unhashed(
        TxLegacy {
            chain_id: None,
            nonce: 0,
            gas_price: 0,
            gas_limit: 0,
            to: to.into(),
            value: U256::ZERO,
            input,
        },
        TEMPO_SYSTEM_TX_SIGNATURE,
    ))
}

fn user_transaction(input_tag: u8) -> TempoTxEnvelope {
    TempoTxEnvelope::Legacy(
        TxLegacy {
            to: Address::repeat_byte(input_tag).into(),
            input: Bytes::from(vec![input_tag]),
            ..Default::default()
        }
        .into_signed(Signature::new(U256::from(1), U256::from(2), false)),
    )
}

fn receipt(success: bool, logs: Vec<Log>) -> TempoReceipt<Log> {
    TempoReceipt {
        tx_type: TempoTxType::Legacy,
        success,
        cumulative_gas_used: 0,
        logs,
    }
}

fn required_advance_logs(hash_override: Option<B256>) -> Vec<Log> {
    let header = imported_header();
    let hash = hash_override.unwrap_or_else(|| header.hash_slow());
    vec![
        Log {
            address: TEMPO_STATE_ADDRESS,
            data: TempoState::TempoBlockFinalized {
                blockHash: header.hash_slow(),
                blockNumber: header.inner.number,
                stateRoot: header.inner.state_root,
            }
            .encode_log_data(),
        },
        Log {
            address: ZONE_INBOX_ADDRESS,
            data: IZoneInbox::TempoAdvanced {
                tempoBlockHash: hash,
                tempoBlockNumber: header.inner.number,
                depositsProcessed: U256::ZERO,
                newProcessedDepositQueueHash: B256::repeat_byte(0x41),
                lastProcessedDepositNumber: 12,
            }
            .encode_log_data(),
        },
    ]
}

fn recovered_block(
    transactions: Vec<TempoTxEnvelope>,
    senders: Vec<Address>,
) -> RecoveredBlock<Block> {
    let block = Block {
        header: TempoHeader {
            inner: Header {
                number: ZONE_NUMBER,
                ..Default::default()
            },
            ..Default::default()
        },
        body: BlockBody {
            transactions,
            ..Default::default()
        },
    };
    RecoveredBlock::new_sealed(SealedBlock::seal_slow(block), senders)
}

fn post_state(block_hash: B256) -> ZonePostStateOutputs {
    let header = imported_header();
    ZonePostStateOutputs::for_test(
        block_hash,
        header.hash_slow(),
        header.inner.number,
        B256::repeat_byte(0x41),
        12,
        B256::repeat_byte(0x51),
        3,
    )
}

fn observe_with_state<R>(
    block: &RecoveredBlock<Block>,
    receipts: &[R],
    post_state: ZonePostStateOutputs,
) -> Result<L2BlockObservation, ObservationError>
where
    R: alloy_consensus::TxReceipt<Log = Log>,
{
    super::observe_l2_block(block, receipts, |_| Ok(post_state))
}

fn basic_fixture() -> (
    RecoveredBlock<Block>,
    Vec<TempoReceipt<Log>>,
    ZonePostStateOutputs,
) {
    let block = recovered_block(
        vec![advance_transaction(ZONE_INBOX_ADDRESS)],
        vec![Address::ZERO],
    );
    let receipts = vec![receipt(true, required_advance_logs(None))];
    let state = post_state(block.hash());
    (block, receipts, state)
}

fn tempo_gas_rate_updated_log() -> Log {
    Log {
        address: ZONE_OUTBOX_ADDRESS,
        data: IZoneOutbox::TempoGasRateUpdated { tempoGasRate: 7 }.encode_log_data(),
    }
}

fn withdrawal_requested_log() -> Log {
    Log {
        address: ZONE_OUTBOX_ADDRESS,
        data: IZoneOutbox::WithdrawalRequested {
            withdrawalIndex: 4,
            sender: Address::repeat_byte(0x44),
            token: Address::repeat_byte(0x55),
            to: Address::repeat_byte(0x66),
            amount: 100,
            fee: 9,
            memo: B256::ZERO,
            gasLimit: 0,
            fallbackNonce: 3,
            data: Bytes::new(),
            revealTo: Bytes::new(),
        }
        .encode_log_data(),
    }
}

fn observe_user_logs(logs: Vec<Log>) -> (B256, L2BlockObservation) {
    let advance = advance_transaction(ZONE_INBOX_ADDRESS);
    let user = user_transaction(0x77);
    let user_hash = *user.tx_hash();
    let block = recovered_block(
        vec![advance, user],
        vec![Address::ZERO, Address::repeat_byte(0x44)],
    );
    let receipts = vec![
        receipt(true, required_advance_logs(None)),
        receipt(true, logs),
    ];
    let observation = observe_with_state(&block, &receipts, post_state(block.hash())).unwrap();
    (user_hash, observation)
}

#[test]
fn observes_header_derived_input_and_output_only_tempo_event() {
    let (block, receipts, state) = basic_fixture();
    let observation = observe_with_state(&block, &receipts, state).unwrap();

    assert_eq!(observation.block_hash, block.hash());
    assert_eq!(
        observation.inputs.advance_tempo.imported_header().hash(),
        imported_header().hash_slow()
    );
    assert_eq!(observation.outcomes.events.len(), 2);
}

#[test]
fn tempo_advanced_and_exact_state_cursors_are_retained_without_cross_comparison() {
    let (block, mut receipts, state) = basic_fixture();
    receipts[0].logs[1] = Log {
        address: ZONE_INBOX_ADDRESS,
        data: IZoneInbox::TempoAdvanced {
            tempoBlockHash: imported_header().hash_slow(),
            tempoBlockNumber: imported_header().inner.number,
            depositsProcessed: U256::ZERO,
            newProcessedDepositQueueHash: B256::repeat_byte(0xa1),
            lastProcessedDepositNumber: 91,
        }
        .encode_log_data(),
    };

    let observation = observe_with_state(&block, &receipts, state).unwrap();
    assert_eq!(
        observation.outcomes.post_state.processed_cursor_for_test(),
        (B256::repeat_byte(0x41), 12)
    );
    assert!(matches!(
        &observation.outcomes.events[1].event,
        L2ProtocolEvent::Inbox(Inbox::InboxEvents::TempoAdvanced(event))
            if event.newProcessedDepositQueueHash == B256::repeat_byte(0xa1)
                && event.lastProcessedDepositNumber == 91
    ));
}

#[test]
fn forged_tempo_advanced_cannot_replace_the_calldata_header() {
    let (block, mut receipts, state) = basic_fixture();
    let forged_hash = B256::repeat_byte(0xee);
    receipts[0].logs = required_advance_logs(Some(forged_hash));

    let error = observe_with_state(&block, &receipts, state).unwrap_err();
    assert!(matches!(
        error,
        ObservationError::OutputMismatch {
            field: OutputField::TempoAdvancedHash,
            expected: MismatchValue::Hash(expected),
            actual: MismatchValue::Hash(actual),
        } if expected == imported_header().hash_slow() && actual == forged_hash
    ));
}

#[test]
fn enabled_token_outputs_match_by_order_with_typed_field_failures() {
    let block = recovered_block(
        vec![advance_transaction_with_tokens(
            ZONE_INBOX_ADDRESS,
            vec![IZoneInbox::EnabledToken {
                token: Address::repeat_byte(0x71),
                name: "Token".into(),
                symbol: "TKN".into(),
                currency: "USD".into(),
            }],
        )],
        vec![Address::ZERO],
    );
    let mut logs = required_advance_logs(None);
    logs.push(token_enabled_log("TKN"));
    let mut receipts = vec![receipt(true, logs)];

    let observation = observe_with_state(&block, &receipts, post_state(block.hash())).unwrap();
    assert_eq!(observation.inputs.advance_tempo.enabled_tokens().len(), 1);

    receipts[0].logs[2] = token_enabled_log("BAD");
    assert!(matches!(
        observe_with_state(&block, &receipts, post_state(block.hash())),
        Err(ObservationError::OutputMismatch {
            field: OutputField::TokenEnabledSymbol { index: 0 },
            expected: MismatchValue::Text(expected),
            actual: MismatchValue::Text(actual),
        }) if expected == "TKN" && actual == "BAD"
    ));
}

#[test]
fn cardinality_errors_are_structural() {
    let (block, receipts, state) = basic_fixture();
    let error = observe_with_state(&block, &Vec::<TempoReceipt<Log>>::new(), state).unwrap_err();
    assert!(matches!(
        error,
        ObservationError::InvalidEnvelope {
            rule: EnvelopeRule::TransactionReceiptCardinality,
            ..
        }
    ));

    let without_sender = recovered_block(vec![advance_transaction(ZONE_INBOX_ADDRESS)], vec![]);
    let error = observe_with_state(
        &without_sender,
        &receipts,
        post_state(without_sender.hash()),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ObservationError::InvalidEnvelope {
            rule: EnvelopeRule::TransactionSenderCardinality,
            ..
        }
    ));
}

#[test]
fn opening_envelope_requires_system_identity_destination_and_success() {
    let receipts = vec![receipt(true, required_advance_logs(None))];
    let wrong_sender = recovered_block(
        vec![advance_transaction(ZONE_INBOX_ADDRESS)],
        vec![Address::repeat_byte(1)],
    );
    assert!(matches!(
        observe_with_state(&wrong_sender, &receipts, post_state(wrong_sender.hash())),
        Err(ObservationError::InvalidEnvelope {
            rule: EnvelopeRule::AdvanceSystemCaller,
            ..
        })
    ));

    let wrong_destination = recovered_block(
        vec![advance_transaction(Address::repeat_byte(2))],
        vec![Address::ZERO],
    );
    assert!(matches!(
        observe_with_state(
            &wrong_destination,
            &receipts,
            post_state(wrong_destination.hash())
        ),
        Err(ObservationError::InvalidEnvelope {
            rule: EnvelopeRule::AdvanceDestination,
            ..
        })
    ));

    let (block, _, state) = basic_fixture();
    assert!(matches!(
        observe_with_state(&block, &[receipt(false, vec![])], state),
        Err(ObservationError::InvalidEnvelope {
            rule: EnvelopeRule::AdvanceSuccess,
            ..
        })
    ));
}

#[test]
fn deterministic_envelope_validation_precedes_exact_state_acquisition() {
    let block = recovered_block(
        vec![advance_transaction(Address::repeat_byte(2))],
        vec![Address::ZERO],
    );
    let receipts = vec![receipt(true, required_advance_logs(None))];
    let mut state_requested = false;

    let error = super::observe_l2_block(&block, &receipts, |_| {
        state_requested = true;
        Err(AcquisitionError::missing(AcquisitionSource::ExactZoneState, block.hash()).into())
    })
    .unwrap_err();

    assert!(matches!(
        error,
        ObservationError::InvalidEnvelope {
            rule: EnvelopeRule::AdvanceDestination,
            ..
        }
    ));
    assert!(!state_requested);
}

#[test]
fn deterministic_event_validation_precedes_exact_state_acquisition() {
    let (block, mut receipts, _) = basic_fixture();
    let forged_hash = B256::repeat_byte(0xee);
    receipts[0].logs = required_advance_logs(Some(forged_hash));
    let mut state_requested = false;

    let error = super::observe_l2_block(&block, &receipts, |_| {
        state_requested = true;
        Err(AcquisitionError::missing(AcquisitionSource::ExactZoneState, block.hash()).into())
    })
    .unwrap_err();

    assert!(matches!(
        error,
        ObservationError::OutputMismatch {
            field: OutputField::TempoAdvancedHash,
            expected: MismatchValue::Hash(expected),
            actual: MismatchValue::Hash(actual),
        } if expected == imported_header().hash_slow() && actual == forged_hash
    ));
    assert!(!state_requested);
}

#[test]
fn outcomes_preserve_config_before_operation_order() {
    let (user_hash, observation) = observe_user_logs(vec![
        tempo_gas_rate_updated_log(),
        withdrawal_requested_log(),
    ]);
    let user_events = &observation.outcomes().events()[2..];
    assert_eq!(
        user_events[0].position(),
        L2EventPosition {
            transaction_index: 1,
            receipt_log_index: 0,
            block_log_index: 2,
            transaction_hash: user_hash,
        }
    );
    assert_eq!(
        user_events[1].position(),
        L2EventPosition {
            transaction_index: 1,
            receipt_log_index: 1,
            block_log_index: 3,
            transaction_hash: user_hash,
        }
    );
    assert!(matches!(
        user_events[0].event(),
        L2ProtocolEvent::Outbox(Outbox::OutboxEvents::TempoGasRateUpdated(_))
    ));
    assert!(matches!(
        user_events[1].event(),
        L2ProtocolEvent::Outbox(Outbox::OutboxEvents::WithdrawalRequested(_))
    ));
}

#[test]
fn outcomes_preserve_operation_before_config_order() {
    let (user_hash, observation) = observe_user_logs(vec![
        withdrawal_requested_log(),
        tempo_gas_rate_updated_log(),
    ]);
    let user_events = &observation.outcomes().events()[2..];
    assert_eq!(
        user_events[0].position(),
        L2EventPosition {
            transaction_index: 1,
            receipt_log_index: 0,
            block_log_index: 2,
            transaction_hash: user_hash,
        }
    );
    assert_eq!(
        user_events[1].position(),
        L2EventPosition {
            transaction_index: 1,
            receipt_log_index: 1,
            block_log_index: 3,
            transaction_hash: user_hash,
        }
    );
    assert!(matches!(
        user_events[0].event(),
        L2ProtocolEvent::Outbox(Outbox::OutboxEvents::WithdrawalRequested(_))
    ));
    assert!(matches!(
        user_events[1].event(),
        L2ProtocolEvent::Outbox(Outbox::OutboxEvents::TempoGasRateUpdated(_))
    ));
}

#[test]
fn protocol_event_surface_fails_closed_and_external_logs_are_ignored() {
    let (block, mut receipts, state) = basic_fixture();
    receipts[0].logs.insert(
        0,
        Log {
            address: Address::repeat_byte(0x99),
            data: LogData::new_unchecked(vec![B256::repeat_byte(0xaa)], Bytes::new()),
        },
    );
    let observation = observe_with_state(&block, &receipts, state).unwrap();
    assert_eq!(observation.outcomes.events.len(), 2);

    let (block, mut receipts, state) = basic_fixture();
    let transaction_hash = *block.body().transactions[0].tx_hash();
    receipts[0].logs.insert(
        0,
        Log {
            address: ZONE_INBOX_ADDRESS,
            data: LogData::new_unchecked(vec![B256::repeat_byte(0xff)], Bytes::new()),
        },
    );
    assert!(matches!(
        observe_with_state(&block, &receipts, state),
        Err(ObservationError::ProtocolEvent {
            chain: ProtocolChain::ZoneL2,
            transaction_index: 0,
            receipt_log_index: 0,
            block_log_index: 0,
            transaction_hash: actual_hash,
            error,
        }) if actual_hash == transaction_hash
            && matches!(error.as_ref(), crate::model::events::ProtocolEventError::UnsupportedProtocolEvent { .. })
    ));

    let (block, mut receipts, state) = basic_fixture();
    receipts[0].logs.insert(
        0,
        Log {
            address: ZONE_INBOX_ADDRESS,
            data: LogData::new_unchecked(
                vec![IZoneInbox::TempoAdvanced::SIGNATURE_HASH],
                Bytes::from(vec![0xde, 0xad]),
            ),
        },
    );
    assert!(matches!(
        observe_with_state(&block, &receipts, state),
        Err(ObservationError::ProtocolEvent { error, .. })
            if matches!(error.as_ref(), crate::model::events::ProtocolEventError::MalformedProtocolEvent { .. })
    ));
}

#[test]
fn deposit_rejected_is_unsupported_not_a_failed_deposit() {
    let (block, mut receipts, state) = basic_fixture();
    receipts[0].logs.insert(
        0,
        Log {
            address: ZONE_INBOX_ADDRESS,
            data: LogData::new_unchecked(
                vec![b256!(
                    "4620415fad9c416306a56ca0ee640b3418628a5f2e45ddde3ddf7452a7a654fb"
                )],
                Bytes::new(),
            ),
        },
    );
    assert!(matches!(
        observe_with_state(&block, &receipts, state),
        Err(ObservationError::ProtocolEvent { error, .. })
            if matches!(error.as_ref(), crate::model::events::ProtocolEventError::UnsupportedProtocolEvent { .. })
    ));
}

#[test]
fn finalization_is_unique_final_and_retains_independent_event_and_state_outputs() {
    let advance = advance_transaction(ZONE_INBOX_ADDRESS);
    let finalize = finalization_transaction(ZONE_NUMBER);
    let finalize_hash = *finalize.tx_hash();
    let block = recovered_block(vec![advance, finalize], vec![Address::ZERO, Address::ZERO]);
    let state = post_state(block.hash());
    let receipts = vec![
        receipt(true, required_advance_logs(None)),
        receipt(
            true,
            vec![Log {
                address: ZONE_OUTBOX_ADDRESS,
                data: IZoneOutbox::BatchFinalized {
                    withdrawalQueueHash: B256::repeat_byte(0xa1),
                    withdrawalBatchIndex: 91,
                }
                .encode_log_data(),
            }],
        ),
    ];
    let observation = observe_with_state(&block, &receipts, state).unwrap();
    assert_eq!(
        observation.inputs.finalization.unwrap().transaction_hash,
        finalize_hash
    );
    assert_eq!(
        observation.outcomes.post_state.withdrawal_cursor_for_test(),
        (B256::repeat_byte(0x51), 3)
    );
    assert!(matches!(
        &observation.outcomes.events.last().unwrap().event,
        L2ProtocolEvent::Outbox(Outbox::OutboxEvents::BatchFinalized(event))
            if event.withdrawalQueueHash == B256::repeat_byte(0xa1)
                && event.withdrawalBatchIndex == 91
    ));
}

#[test]
fn finalization_rejects_wrong_block_number_and_position() {
    let wrong_number = recovered_block(
        vec![
            advance_transaction(ZONE_INBOX_ADDRESS),
            finalization_transaction(ZONE_NUMBER + 1),
        ],
        vec![Address::ZERO, Address::ZERO],
    );
    let receipts = vec![
        receipt(true, required_advance_logs(None)),
        receipt(true, vec![]),
    ];
    assert!(matches!(
        observe_with_state(&wrong_number, &receipts, post_state(wrong_number.hash())),
        Err(ObservationError::InvalidEnvelope {
            rule: EnvelopeRule::FinalizationBlockNumber,
            ..
        })
    ));

    let misplaced = recovered_block(
        vec![
            advance_transaction(ZONE_INBOX_ADDRESS),
            finalization_transaction(ZONE_NUMBER),
            user_transaction(0x71),
        ],
        vec![Address::ZERO, Address::ZERO, Address::repeat_byte(3)],
    );
    let receipts = vec![
        receipt(true, required_advance_logs(None)),
        receipt(true, vec![]),
        receipt(true, vec![]),
    ];
    assert!(matches!(
        observe_with_state(&misplaced, &receipts, post_state(misplaced.hash())),
        Err(ObservationError::InvalidEnvelope {
            rule: EnvelopeRule::FinalizationPosition,
            ..
        })
    ));
}

#[test]
fn finalization_requires_system_identity_destination_and_success() {
    let finalization_receipts = || {
        vec![
            receipt(true, required_advance_logs(None)),
            receipt(true, vec![]),
        ]
    };

    let wrong_sender = recovered_block(
        vec![
            advance_transaction(ZONE_INBOX_ADDRESS),
            finalization_transaction(ZONE_NUMBER),
        ],
        vec![Address::ZERO, Address::repeat_byte(1)],
    );
    assert!(matches!(
        observe_with_state(
            &wrong_sender,
            &finalization_receipts(),
            post_state(wrong_sender.hash())
        ),
        Err(ObservationError::InvalidEnvelope {
            rule: EnvelopeRule::SystemIdentity,
            ..
        })
    ));

    let wrong_destination = recovered_block(
        vec![
            advance_transaction(ZONE_INBOX_ADDRESS),
            system_transaction(
                Address::repeat_byte(2),
                finalization_transaction(ZONE_NUMBER).input().clone(),
            ),
        ],
        vec![Address::ZERO, Address::ZERO],
    );
    assert!(matches!(
        observe_with_state(
            &wrong_destination,
            &finalization_receipts(),
            post_state(wrong_destination.hash())
        ),
        Err(ObservationError::InvalidEnvelope {
            rule: EnvelopeRule::FinalizationDestination,
            ..
        })
    ));

    let failed = recovered_block(
        vec![
            advance_transaction(ZONE_INBOX_ADDRESS),
            finalization_transaction(ZONE_NUMBER),
        ],
        vec![Address::ZERO, Address::ZERO],
    );
    let failed_receipts = vec![
        receipt(true, required_advance_logs(None)),
        receipt(false, vec![]),
    ];
    assert!(matches!(
        observe_with_state(&failed, &failed_receipts, post_state(failed.hash())),
        Err(ObservationError::InvalidEnvelope {
            rule: EnvelopeRule::FinalizationSuccess,
            ..
        })
    ));
}

#[test]
fn exact_state_is_bound_to_the_observed_zone_hash() {
    let (block, receipts, state) = basic_fixture();
    let state = state.with_block_hash_for_test(B256::repeat_byte(0xdd));
    assert!(matches!(
        observe_with_state(&block, &receipts, state),
        Err(ObservationError::OutputMismatch {
            field: OutputField::StateBlockBinding,
            ..
        })
    ));
}

#[test]
fn malformed_finalization_calldata_has_its_own_error_class() {
    let block = recovered_block(
        vec![
            advance_transaction(ZONE_INBOX_ADDRESS),
            system_transaction(ZONE_OUTBOX_ADDRESS, Bytes::from_static(b"bad")),
        ],
        vec![Address::ZERO, Address::ZERO],
    );
    let receipts = vec![
        receipt(true, required_advance_logs(None)),
        receipt(true, vec![]),
    ];
    assert!(matches!(
        observe_with_state(&block, &receipts, post_state(block.hash())),
        Err(ObservationError::MalformedAuthenticatedData {
            kind: DataSource::FinalizationCalldata,
            ..
        })
    ));
}
