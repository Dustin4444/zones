use alloy_consensus::Header;
use alloy_primitives::Address;

use super::*;
use crate::observe::ProtocolChain;

fn l2_transaction(index: usize) -> AuthenticatedTransaction {
    AuthenticatedTransaction::new(ProtocolChain::ZoneL2, index, B256::repeat_byte(0xa1))
}

fn l1_transaction(index: usize) -> AuthenticatedTransaction {
    AuthenticatedTransaction::new(ProtocolChain::TempoL1, index, B256::repeat_byte(0xb1))
}

fn decode_advance(calldata: &[u8]) -> Result<DecodedAdvanceTempo, ObservationError> {
    decode_advance_tempo(calldata, l2_transaction(0))
}

fn decode_finalize(calldata: &[u8]) -> Result<DecodedFinalization, ObservationError> {
    decode_finalization(calldata, l2_transaction(1))
}

fn decode_portal(calldata: &[u8]) -> Result<DecodedPortalCall, ObservationError> {
    decode_portal_call(calldata, l1_transaction(2))
}

fn header_bytes(number: u64) -> Bytes {
    let header = TempoHeader {
        inner: Header {
            number,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut encoded = Vec::new();
    header.encode(&mut encoded);
    encoded.into()
}

fn advance_call() -> Vec<u8> {
    IZoneInbox::advanceTempoCall {
        header: header_bytes(7),
        deposits: Vec::new(),
        decryptions: Vec::new(),
        enabledTokens: Vec::new(),
    }
    .abi_encode()
}

fn ordinary_deposit() -> ZonePortal::Deposit {
    ZonePortal::Deposit {
        token: Address::repeat_byte(1),
        sender: Address::repeat_byte(2),
        amount: 3,
        tempoRefundRecipient: Address::repeat_byte(4),
        keyIndex: U256::from(5),
        encrypted: ZonePortal::DepositPayload {
            ephemeralPubkeyX: B256::repeat_byte(6),
            ephemeralPubkeyYParity: 2,
            ciphertext: Bytes::from(vec![7; ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE]),
            nonce: [8; 12].into(),
            tag: [9; 16].into(),
        },
    }
}

fn advance_with_ordinary_deposit_data(deposit_data: Vec<u8>) -> Vec<u8> {
    IZoneInbox::advanceTempoCall {
        header: header_bytes(7),
        deposits: vec![IZoneInbox::QueuedDeposit {
            depositType: IZoneInbox::DepositType::Deposit,
            depositData: deposit_data.into(),
        }],
        decryptions: vec![IZoneInbox::DecryptionData {
            sharedSecret: B256::ZERO,
            sharedSecretYParity: 0,
            cpProof: IZoneInbox::ChaumPedersenProof {
                s: B256::ZERO,
                c: B256::ZERO,
            },
        }],
        enabledTokens: Vec::new(),
    }
    .abi_encode()
}

fn submit_batch_call(signatures: Vec<Bytes>) -> Vec<u8> {
    ZonePortal::submitBatchCall {
        tempoBlockNumber: 1,
        recentTempoBlockNumber: 2,
        blockTransition: ZonePortal::BlockTransition {
            prevBlockHash: B256::repeat_byte(3),
            nextBlockHash: B256::repeat_byte(4),
        },
        depositQueueTransition: ZonePortal::DepositQueueTransition {
            prevProcessedHash: B256::repeat_byte(5),
            nextProcessedHash: B256::repeat_byte(6),
            prevDepositNumber: 7,
            nextDepositNumber: 8,
        },
        withdrawalQueueHash: B256::repeat_byte(9),
        verifierConfig: Bytes::from_static(b"config"),
        proof: Bytes::from_static(b"proof"),
        nextZoneHeight: U256::from(10),
        signatures,
    }
    .abi_encode()
}

fn usize_word(data: &[u8], offset: usize) -> usize {
    U256::from_be_slice(&data[offset..offset + WORD]).to::<usize>()
}

fn set_usize_word(data: &mut [u8], offset: usize, value: usize) {
    data[offset..offset + WORD].copy_from_slice(&U256::from(value).to_be_bytes::<WORD>());
}

fn assert_malformed<T: core::fmt::Debug>(
    result: Result<T, ObservationError>,
    expected: DataSource,
) {
    match result {
        Err(ObservationError::MalformedAuthenticatedData {
            kind, transaction, ..
        }) => {
            assert_eq!(kind, expected);
            let expected_transaction = match expected {
                DataSource::FinalizationCalldata => l2_transaction(1),
                DataSource::AdvanceTempoCalldata
                | DataSource::AdvanceHeaderRlp
                | DataSource::OrdinaryDepositData
                | DataSource::WithdrawalBounceBackData => l2_transaction(0),
                DataSource::ProcessWithdrawalsCalldata
                | DataSource::SubmitBatchCalldata
                | DataSource::PortalTransactionCalldata => l1_transaction(2),
            };
            assert_eq!(transaction, expected_transaction);
        }
        other => panic!("expected malformed {expected}, got {other:?}"),
    }
}

#[test]
fn advance_tempo_round_trips_canonical_header_and_calldata() {
    let decoded = decode_advance(&advance_call()).unwrap();
    assert_eq!(decoded.imported_header.number(), 7);
    assert!(decoded.deposits.is_empty());
}

#[test]
fn advance_tempo_round_trips_every_dynamic_input_family() {
    let ordinary = ordinary_deposit();
    let bounce_back = IZoneInbox::WithdrawalBounceBackDeposit {
        token: Address::repeat_byte(10),
        to: Address::repeat_byte(11),
        amount: 12,
    };
    let calldata = IZoneInbox::advanceTempoCall {
        header: header_bytes(7),
        deposits: vec![
            IZoneInbox::QueuedDeposit {
                depositType: IZoneInbox::DepositType::Deposit,
                depositData: ordinary.abi_encode().into(),
            },
            IZoneInbox::QueuedDeposit {
                depositType: IZoneInbox::DepositType::WithdrawalBounceBack,
                depositData: bounce_back.abi_encode().into(),
            },
        ],
        decryptions: vec![IZoneInbox::DecryptionData {
            sharedSecret: B256::repeat_byte(13),
            sharedSecretYParity: 2,
            cpProof: IZoneInbox::ChaumPedersenProof {
                s: B256::repeat_byte(14),
                c: B256::repeat_byte(15),
            },
        }],
        enabledTokens: vec![IZoneInbox::EnabledToken {
            token: Address::repeat_byte(16),
            name: "Token".into(),
            symbol: "TKN".into(),
            currency: "USD".into(),
        }],
    }
    .abi_encode();

    let decoded = decode_advance(&calldata).unwrap();
    assert!(decoded.deposits[0].as_ordinary().is_some());
    assert!(decoded.deposits[1].as_withdrawal_bounce_back().is_some());
    assert_eq!(decoded.decryptions.len(), 1);
    assert_eq!(decoded.enabled_tokens.len(), 1);
}

#[test]
fn ordinary_deposit_data_is_exactly_bounded_before_outer_decode() {
    let mut oversized = ordinary_deposit().abi_encode();
    assert_eq!(oversized.len(), ORDINARY_DEPOSIT_ENCODED_SIZE);
    oversized.extend([0; WORD]);
    let expected_evidence = AuthenticatedDataEvidence::from_bytes(&oversized);
    let calldata = advance_with_ordinary_deposit_data(oversized);

    let Err(ObservationError::MalformedAuthenticatedData {
        kind,
        transaction,
        evidence,
        ..
    }) = decode_advance(&calldata)
    else {
        panic!("expected malformed ordinary deposit data");
    };
    assert_eq!(kind, DataSource::OrdinaryDepositData);
    assert_eq!(transaction, l2_transaction(0));
    assert_eq!(evidence, expected_evidence);
    assert_ne!(
        evidence,
        AuthenticatedDataEvidence::from_bytes(&calldata),
        "nested evidence must hash depositData, not the outer advanceTempo calldata"
    );
}

#[test]
fn ordinary_deposit_nested_offsets_and_lengths_fail_in_borrowed_preflight() {
    let canonical = ordinary_deposit().abi_encode();
    assert_eq!(canonical.len(), ORDINARY_DEPOSIT_ENCODED_SIZE);
    let deposit = usize_word(&canonical, 0);
    let encrypted = deposit + usize_word(&canonical, deposit + 5 * WORD);
    let ciphertext = encrypted + usize_word(&canonical, encrypted + 2 * WORD);
    assert_eq!(
        (deposit, encrypted, ciphertext),
        (WORD, 7 * WORD, 12 * WORD)
    );

    for (offset, value) in [
        (0, WORD + 1),
        (deposit + 5 * WORD, 5 * WORD),
        (encrypted + 2 * WORD, 4 * WORD),
        (ciphertext, ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE + 1),
    ] {
        let mut malformed = canonical.clone();
        set_usize_word(&mut malformed, offset, value);
        assert_eq!(malformed.len(), ORDINARY_DEPOSIT_ENCODED_SIZE);
        assert_malformed(
            decode_advance(&advance_with_ordinary_deposit_data(malformed)),
            DataSource::OrdinaryDepositData,
        );
    }
}

#[test]
fn advance_tempo_rejects_trailing_abi_bytes() {
    let mut calldata = advance_call();
    calldata.extend([0_u8; WORD]);
    assert_malformed(decode_advance(&calldata), DataSource::AdvanceTempoCalldata);
}

#[test]
fn advance_tempo_rejects_noncanonical_header_rlp() {
    let mut header = header_bytes(7).to_vec();
    let mut cursor = header.as_slice();
    let envelope = alloy_rlp::Header::decode(&mut cursor).unwrap();
    assert!(envelope.list);
    let first_item = header.len() - cursor.len();
    assert_eq!(header[first_item], 0x80, "first Tempo header field is zero");
    header[first_item] = 0x00;
    let calldata = IZoneInbox::advanceTempoCall {
        header: header.into(),
        deposits: Vec::new(),
        decryptions: Vec::new(),
        enabledTokens: Vec::new(),
    }
    .abi_encode();
    assert_malformed(decode_advance(&calldata), DataSource::AdvanceHeaderRlp);
}

#[test]
fn advance_tempo_rejects_trailing_header_rlp() {
    let mut header = header_bytes(7).to_vec();
    header.push(0x80);
    let calldata = IZoneInbox::advanceTempoCall {
        header: header.into(),
        deposits: Vec::new(),
        decryptions: Vec::new(),
        enabledTokens: Vec::new(),
    }
    .abi_encode();
    assert_malformed(decode_advance(&calldata), DataSource::AdvanceHeaderRlp);
}

#[test]
fn dynamic_array_protocol_caps_are_checked_before_generated_decode() {
    for (head_word, maximum) in [
        (1, MAX_DEPOSITS_PER_TEMPO_BLOCK),
        (3, MAX_TOKENS_ENABLED_PER_TEMPO_BLOCK),
    ] {
        let mut calldata = advance_call();
        let payload = &mut calldata[SELECTOR..];
        let array = usize_word(payload, head_word * WORD);
        set_usize_word(payload, array, maximum + 1);
        assert_malformed(decode_advance(&calldata), DataSource::AdvanceTempoCalldata);
    }

    let mut calldata = submit_batch_call(Vec::new());
    let payload = &mut calldata[SELECTOR..];
    let signatures = usize_word(payload, 12 * WORD);
    set_usize_word(payload, signatures, MAX_SEQUENCERS + 1);
    assert_malformed(decode_portal(&calldata), DataSource::SubmitBatchCalldata);
}

#[test]
fn abi_offsets_lengths_and_element_tables_fail_closed_before_decode() {
    for bad_offset in [4 * WORD + 1, advance_call().len() - SELECTOR] {
        let mut calldata = advance_call();
        set_usize_word(&mut calldata[SELECTOR..], 0, bad_offset);
        assert_malformed(decode_advance(&calldata), DataSource::AdvanceTempoCalldata);
    }

    let mut calldata = advance_call();
    let payload = &mut calldata[SELECTOR..];
    let header = usize_word(payload, 0);
    set_usize_word(payload, header, payload.len());
    assert_malformed(decode_advance(&calldata), DataSource::AdvanceTempoCalldata);

    let mut calldata = submit_batch_call(vec![Bytes::from_static(b"signature")]);
    let payload = &mut calldata[SELECTOR..];
    let signatures = usize_word(payload, 12 * WORD);
    let signature_table = signatures + WORD;
    set_usize_word(payload, signature_table, payload.len());
    assert_malformed(decode_portal(&calldata), DataSource::SubmitBatchCalldata);
}

#[test]
fn finalization_decodes_count_block_and_structural_sender_lengths() {
    let call = IZoneOutbox::finalizeWithdrawalBatchCall {
        count: U256::from(2),
        blockNumber: 9,
        encryptedSenders: vec![
            Bytes::new(),
            Bytes::from(vec![7; AUTHENTICATED_WITHDRAWAL_SIZE]),
        ],
    }
    .abi_encode();
    let decoded = decode_finalize(&call).unwrap();
    assert_eq!(decoded.count, 2);
    assert_eq!(decoded.block_number, 9);
    assert_eq!(decoded.encrypted_senders[0].len(), 0);
    assert_eq!(
        decoded.encrypted_senders[1].len(),
        AUTHENTICATED_WITHDRAWAL_SIZE
    );
}

#[test]
fn finalization_rejects_count_and_dynamic_length_mismatches() {
    let count_mismatch = IZoneOutbox::finalizeWithdrawalBatchCall {
        count: U256::from(2),
        blockNumber: 9,
        encryptedSenders: vec![Bytes::new()],
    }
    .abi_encode();
    assert_malformed(
        decode_finalize(&count_mismatch),
        DataSource::FinalizationCalldata,
    );

    let malformed_length = IZoneOutbox::finalizeWithdrawalBatchCall {
        count: U256::from(1),
        blockNumber: 9,
        encryptedSenders: vec![Bytes::from(vec![0; 1])],
    }
    .abi_encode();
    assert_malformed(
        decode_finalize(&malformed_length),
        DataSource::FinalizationCalldata,
    );
}

#[test]
fn process_withdrawals_callback_is_bounded_before_decode() {
    let call = ZonePortal::processWithdrawalsCall {
        withdrawals: vec![ZonePortal::Withdrawal {
            token: Address::ZERO,
            senderTag: B256::ZERO,
            to: Address::ZERO,
            amount: 1,
            memo: B256::ZERO,
            gasLimit: 0,
            fallbackNonce: 0,
            callbackData: Bytes::from(vec![0; MAX_CALLBACK_DATA_SIZE + 1]),
            encryptedSender: Bytes::new(),
        }],
        remainingQueue: B256::ZERO,
    }
    .abi_encode();
    assert_malformed(decode_portal(&call), DataSource::ProcessWithdrawalsCalldata);
}

#[test]
fn nonempty_process_withdrawals_round_trips_canonically() {
    let call = ZonePortal::processWithdrawalsCall {
        withdrawals: vec![ZonePortal::Withdrawal {
            token: Address::repeat_byte(1),
            senderTag: B256::repeat_byte(2),
            to: Address::repeat_byte(3),
            amount: 4,
            memo: B256::repeat_byte(5),
            gasLimit: 6,
            fallbackNonce: 7,
            callbackData: Bytes::from_static(b"callback"),
            encryptedSender: Bytes::from(vec![8; AUTHENTICATED_WITHDRAWAL_SIZE]),
        }],
        remainingQueue: B256::repeat_byte(9),
    }
    .abi_encode();
    let decoded = decode_portal(&call).unwrap();
    assert!(decoded.is_nonempty_process_withdrawals());
}

#[test]
fn submit_batch_round_trips_every_dynamic_input_family() {
    let call = submit_batch_call(vec![Bytes::from_static(b"signature")]);

    assert!(decode_portal(&call).unwrap().as_submit_batch().is_some());
}
