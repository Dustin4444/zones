//! Canonical Zone block observation from one in-process Reth notification.

use alloy_consensus::{
    BlockHeader as _, Transaction as _, TxReceipt as _, transaction::TxHashRef as _,
};
use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256, Bloom};
use reth_primitives_traits::RecoveredBlock;
use tempo_primitives::{Block, TempoReceipt, TempoTxEnvelope};

use tempo_zone_contracts::{ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS};

use crate::observe::events::{L2ProtocolEvent, classify_l2_protocol_event};

use super::{
    abi::{DecodedAdvanceTempo, DecodedFinalization, decode_advance_tempo, decode_finalization},
    error::{
        AcquisitionError, AcquisitionSource, AuthenticatedTransaction, EnvelopeRule,
        ObservationError, ProtocolChain, ensure_acquisition_equal,
    },
};

/// Canonical coordinates retained for every supported protocol log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct L2EventPosition {
    transaction_index: usize,
    receipt_log_index: usize,
    block_log_index: usize,
    transaction_hash: B256,
    transaction_sender: Address,
}

impl L2EventPosition {
    pub(crate) fn transaction_index(&self) -> usize {
        self.transaction_index
    }

    pub(crate) fn transaction_hash(&self) -> B256 {
        self.transaction_hash
    }

    pub(crate) fn transaction_sender(&self) -> Address {
        self.transaction_sender
    }
}

/// One strictly decoded implementation outcome in canonical block order.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct OrderedL2Outcome {
    position: L2EventPosition,
    event: L2ProtocolEvent,
}

impl OrderedL2Outcome {
    pub(crate) fn position(&self) -> L2EventPosition {
        self.position
    }

    pub(crate) fn event(&self) -> &L2ProtocolEvent {
        &self.event
    }
}

/// Inputs authenticated by the canonical transaction envelopes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct L2AuthenticatedInputs {
    advance_transaction_hash: B256,
    advance_tempo: DecodedAdvanceTempo,
    finalization: Option<FinalizationEnvelope>,
}

impl L2AuthenticatedInputs {
    pub(crate) fn advance_transaction_hash(&self) -> B256 {
        self.advance_transaction_hash
    }

    pub(crate) fn advance_tempo(&self) -> &DecodedAdvanceTempo {
        &self.advance_tempo
    }

    pub(crate) fn finalization(&self) -> Option<&FinalizationEnvelope> {
        self.finalization.as_ref()
    }
}

/// Final system-call input and its containing transaction identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FinalizationEnvelope {
    transaction_hash: B256,
    input: DecodedFinalization,
}

impl FinalizationEnvelope {
    pub(crate) fn transaction_hash(&self) -> B256 {
        self.transaction_hash
    }

    pub(crate) fn input(&self) -> &DecodedFinalization {
        &self.input
    }
}

/// Outputs authenticated by successful receipts.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct L2AuthenticatedOutcomes {
    events: Vec<OrderedL2Outcome>,
}

impl L2AuthenticatedOutcomes {
    pub(crate) fn events(&self) -> &[OrderedL2Outcome] {
        &self.events
    }
}

/// Complete ephemeral observation of one non-genesis Zone block.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct L2BlockObservation {
    block_number: u64,
    block_hash: B256,
    parent_hash: B256,
    inputs: L2AuthenticatedInputs,
    outcomes: L2AuthenticatedOutcomes,
}

impl L2BlockObservation {
    pub(crate) fn block_number(&self) -> u64 {
        self.block_number
    }

    pub(crate) fn block_hash(&self) -> B256 {
        self.block_hash
    }

    pub(crate) fn parent_hash(&self) -> B256 {
        self.parent_hash
    }

    pub(crate) fn inputs(&self) -> &L2AuthenticatedInputs {
        &self.inputs
    }

    pub(crate) fn outcomes(&self) -> &L2AuthenticatedOutcomes {
        &self.outcomes
    }
}

/// Observation failure plus the imported Tempo coordinate once `advanceTempo`
/// has been authenticated. Later Zone-envelope and event failures must retain
/// that coordinate for durable diagnostics.
#[derive(Debug)]
pub(crate) struct L2ObservationFailure {
    error: Box<ObservationError>,
    imported_tempo: Option<BlockNumHash>,
}

impl L2ObservationFailure {
    pub(crate) fn into_parts(self) -> (ObservationError, Option<BlockNumHash>) {
        (*self.error, self.imported_tempo)
    }

    fn with_imported_tempo(error: ObservationError, imported_tempo: BlockNumHash) -> Self {
        Self {
            error: Box::new(error),
            imported_tempo: Some(imported_tempo),
        }
    }
}

impl From<ObservationError> for L2ObservationFailure {
    fn from(error: ObservationError) -> Self {
        Self {
            error: Box::new(error),
            imported_tempo: None,
        }
    }
}

/// Observe one canonical non-genesis Zone block.
///
/// Transactions, recovered senders, and receipts all come from the same Reth
/// notification or backfill result. This layer authenticates the complete
/// receipt set against the block header, authenticates transaction envelopes,
/// and strictly decodes protocol logs; the evaluator compares those independent
/// inputs and outputs against expected transitions.
#[cfg(test)]
pub(crate) fn observe_l2_block(
    block: &RecoveredBlock<Block>,
    receipts: &[TempoReceipt],
) -> Result<L2BlockObservation, ObservationError> {
    observe_l2_block_with_context(block, receipts).map_err(|failure| failure.into_parts().0)
}

/// Observe one Zone block while retaining how far authenticated envelope
/// decoding progressed if it fails.
pub(crate) fn observe_l2_block_with_context(
    block: &RecoveredBlock<Block>,
    receipts: &[TempoReceipt],
) -> Result<L2BlockObservation, L2ObservationFailure> {
    let block_number = block.header().number();
    let block_hash = block.hash();
    let parent_hash = block.header().parent_hash();
    if block_number == 0 {
        return Err(ObservationError::invalid_block_envelope(EnvelopeRule::NonGenesis).into());
    }

    let transactions = &block.body().transactions;
    let senders = block.senders();
    if transactions.len() != receipts.len() {
        return Err(ObservationError::from(AcquisitionError::inconsistent(
            AcquisitionSource::ZoneNotificationReceipts,
            transactions.len(),
            receipts.len(),
        ))
        .into());
    }
    if transactions.len() != senders.len() {
        return Err(ObservationError::from(AcquisitionError::inconsistent(
            AcquisitionSource::ZoneNotificationBlock,
            transactions.len(),
            senders.len(),
        ))
        .into());
    }
    authenticate_receipt_commitments(block, receipts)?;

    let first = transactions
        .first()
        .ok_or_else(|| ObservationError::invalid_block_envelope(EnvelopeRule::AdvancePresent))?;
    if !first.is_system_tx() || senders[0] != Address::ZERO {
        return Err(
            ObservationError::invalid_envelope(0, EnvelopeRule::AdvanceSystemCaller).into(),
        );
    }
    if first.to() != Some(ZONE_INBOX_ADDRESS) {
        return Err(ObservationError::invalid_envelope(0, EnvelopeRule::AdvanceDestination).into());
    }
    if !receipts[0].status() {
        return Err(ObservationError::invalid_envelope(0, EnvelopeRule::AdvanceSuccess).into());
    }
    let advance_coordinate =
        AuthenticatedTransaction::new(ProtocolChain::ZoneL2, 0, *first.tx_hash());
    let advance_tempo = decode_advance_tempo(first.input(), advance_coordinate)?;
    let imported_header = advance_tempo.final_imported_header();
    let imported_tempo = BlockNumHash::new(imported_header.number(), imported_header.hash());

    let finish = || -> Result<L2BlockObservation, ObservationError> {
        let mut finalization = None;
        for (index, ((transaction, sender), receipt)) in transactions
            .iter()
            .zip(senders)
            .zip(receipts)
            .enumerate()
            .skip(1)
        {
            if !transaction.is_system_tx() && *sender != Address::ZERO {
                continue;
            }
            if !transaction.is_system_tx() || *sender != Address::ZERO {
                return Err(ObservationError::invalid_envelope(
                    index,
                    EnvelopeRule::SystemIdentity,
                ));
            }
            if index + 1 != transactions.len() {
                return Err(ObservationError::invalid_envelope(
                    index,
                    EnvelopeRule::FinalizationPosition,
                ));
            }
            if transaction.to() != Some(ZONE_OUTBOX_ADDRESS) {
                return Err(ObservationError::invalid_envelope(
                    index,
                    EnvelopeRule::FinalizationDestination,
                ));
            }
            if !receipt.status() {
                return Err(ObservationError::invalid_envelope(
                    index,
                    EnvelopeRule::FinalizationSuccess,
                ));
            }
            let coordinate =
                AuthenticatedTransaction::new(ProtocolChain::ZoneL2, index, *transaction.tx_hash());
            let input = decode_finalization(transaction.input(), coordinate)?;
            if input.block_number() != block_number {
                return Err(ObservationError::invalid_envelope(
                    index,
                    EnvelopeRule::FinalizationBlockNumber,
                ));
            }
            finalization = Some(FinalizationEnvelope {
                transaction_hash: *transaction.tx_hash(),
                input,
            });
        }

        let events = ordered_l2_events(transactions, senders, receipts)?;

        Ok(L2BlockObservation {
            block_number,
            block_hash,
            parent_hash,
            inputs: L2AuthenticatedInputs {
                advance_transaction_hash: *first.tx_hash(),
                advance_tempo,
                finalization,
            },
            outcomes: L2AuthenticatedOutcomes { events },
        })
    };
    finish().map_err(|error| L2ObservationFailure::with_imported_tempo(error, imported_tempo))
}

fn authenticate_receipt_commitments(
    block: &RecoveredBlock<Block>,
    receipts: &[TempoReceipt],
) -> Result<(), ObservationError> {
    let computed_root = TempoReceipt::calculate_receipt_root_no_memo(receipts);
    ensure_acquisition_equal(
        AcquisitionSource::ZoneNotificationReceipts,
        "receipts root",
        block.header().receipts_root(),
        computed_root,
    )?;

    let computed_bloom = receipts
        .iter()
        .fold(Bloom::ZERO, |bloom, receipt| bloom | receipt.bloom());
    ensure_acquisition_equal(
        AcquisitionSource::ZoneNotificationReceipts,
        "logs bloom",
        block.header().logs_bloom(),
        computed_bloom,
    )
}

fn ordered_l2_events(
    transactions: &[TempoTxEnvelope],
    senders: &[Address],
    receipts: &[TempoReceipt],
) -> Result<Vec<OrderedL2Outcome>, ObservationError> {
    let mut outcomes = Vec::new();
    let mut block_log_index = 0usize;
    for (transaction_index, ((transaction, sender), receipt)) in
        transactions.iter().zip(senders).zip(receipts).enumerate()
    {
        if !receipt.status() {
            continue;
        }
        for (receipt_log_index, log) in receipt.logs().iter().enumerate() {
            let position = L2EventPosition {
                transaction_index,
                receipt_log_index,
                block_log_index,
                transaction_hash: *transaction.tx_hash(),
                transaction_sender: *sender,
            };
            block_log_index += 1;
            if let Some(event) = classify_l2_protocol_event(log).map_err(|error| {
                ObservationError::protocol_event(
                    ProtocolChain::ZoneL2,
                    transaction_index,
                    receipt_log_index,
                    position.block_log_index,
                    *transaction.tx_hash(),
                    error,
                )
            })? {
                outcomes.push(OrderedL2Outcome { position, event });
            }
        }
    }
    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use alloy_consensus::{Header, Sealable as _, SignableTransaction as _, Signed, TxLegacy};
    use alloy_primitives::{Address, B256, Bloom, Bytes, Log, LogData, Signature, U256, b256};
    use alloy_rlp::Encodable as _;
    use alloy_sol_types::{SolCall as _, SolEvent as _};
    use reth_primitives_traits::{RecoveredBlock, SealedBlock};
    use tempo_primitives::{
        Block, BlockBody, TempoHeader, TempoReceipt, TempoTxEnvelope, TempoTxType,
        transaction::envelope::TEMPO_SYSTEM_TX_SIGNATURE,
    };
    use tempo_zone_contracts::{
        IZoneInbox, IZoneOutbox, TEMPO_STATE_ADDRESS, TempoState, ZONE_INBOX_ADDRESS,
        ZONE_OUTBOX_ADDRESS,
    };

    use super::*;
    use crate::observe::{
        error::{
            AcquisitionError, AcquisitionSource, AuthenticatedDataEvidence,
            AuthenticatedTransaction, DataSource, EnvelopeRule, ObservationError, ProtocolChain,
        },
        events::{Inbox, L2ProtocolEvent, Outbox},
    };

    const ZONE_NUMBER: u64 = 9;
    const ZONE_PARENT_HASH: B256 = B256::repeat_byte(0x19);

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

    fn advance_logs(hash_override: Option<B256>) -> Vec<Log> {
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
        receipts: &[TempoReceipt],
    ) -> RecoveredBlock<Block> {
        let (receipts_root, logs_bloom) = receipt_commitments(receipts);
        let block = Block {
            header: TempoHeader {
                inner: Header {
                    number: ZONE_NUMBER,
                    parent_hash: ZONE_PARENT_HASH,
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
        RecoveredBlock::new_sealed(SealedBlock::seal_slow(block), senders)
    }

    fn reseal_with_receipts(
        block: RecoveredBlock<Block>,
        receipts: &[TempoReceipt],
    ) -> RecoveredBlock<Block> {
        let (receipts_root, logs_bloom) = receipt_commitments(receipts);
        reseal_with_commitments(block, receipts_root, logs_bloom)
    }

    fn reseal_with_commitments(
        block: RecoveredBlock<Block>,
        receipts_root: B256,
        logs_bloom: Bloom,
    ) -> RecoveredBlock<Block> {
        let senders = block.senders().to_vec();
        let mut block = block.into_block();
        block.header.inner.receipts_root = receipts_root;
        block.header.inner.logs_bloom = logs_bloom;
        RecoveredBlock::new_sealed(SealedBlock::seal_slow(block), senders)
    }

    fn receipt_commitments(receipts: &[TempoReceipt]) -> (B256, Bloom) {
        let receipts_root = TempoReceipt::calculate_receipt_root_no_memo(receipts);
        let logs_bloom = receipts
            .iter()
            .fold(Bloom::ZERO, |bloom, receipt| bloom | receipt.bloom());
        (receipts_root, logs_bloom)
    }

    fn basic_fixture() -> (RecoveredBlock<Block>, Vec<TempoReceipt<Log>>) {
        let receipts = vec![receipt(true, advance_logs(None))];
        let block = recovered_block(
            vec![advance_transaction(ZONE_INBOX_ADDRESS)],
            vec![Address::ZERO],
            &receipts,
        );
        (block, receipts)
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
                sender: Address::repeat_byte(0x45),
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
        let receipts = vec![receipt(true, advance_logs(None)), receipt(true, logs)];
        let block = recovered_block(
            vec![advance, user],
            vec![Address::ZERO, Address::repeat_byte(0x44)],
            &receipts,
        );
        let observation = observe_l2_block(&block, &receipts).unwrap();
        (user_hash, observation)
    }

    #[test]
    fn observes_decoded_header_input_and_ordered_protocol_events() {
        let (block, receipts) = basic_fixture();
        let observation = observe_l2_block(&block, &receipts).unwrap();

        assert_eq!(observation.block_hash, block.hash());
        assert_eq!(observation.parent_hash(), ZONE_PARENT_HASH);
        assert_eq!(
            observation
                .inputs
                .advance_tempo
                .final_imported_header()
                .hash(),
            imported_header().hash_slow()
        );
        assert_eq!(observation.outcomes.events.len(), 2);
    }

    #[test]
    fn authenticated_inputs_do_not_require_matching_event_outputs() {
        let (block, _) = basic_fixture();
        let receipts = [receipt(true, Vec::new())];
        let block = reseal_with_receipts(block, &receipts);
        let observation = observe_l2_block(&block, &receipts).unwrap();

        assert_eq!(
            observation
                .inputs
                .advance_tempo
                .final_imported_header()
                .hash(),
            imported_header().hash_slow()
        );
        assert!(observation.outcomes.events.is_empty());
    }

    #[test]
    fn tempo_advanced_cursor_is_retained_for_later_evaluation() {
        let (block, mut receipts) = basic_fixture();
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

        let block = reseal_with_receipts(block, &receipts);
        let observation = observe_l2_block(&block, &receipts).unwrap();
        assert!(matches!(
            &observation.outcomes.events[1].event,
            L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::TempoAdvanced(event))
                if event.newProcessedDepositQueueHash == B256::repeat_byte(0xa1)
                    && event.lastProcessedDepositNumber == 91
        ));
    }

    #[test]
    fn mismatched_tempo_advanced_is_retained_independently_from_calldata() {
        let (block, mut receipts) = basic_fixture();
        let forged_hash = B256::repeat_byte(0xee);
        receipts[0].logs = advance_logs(Some(forged_hash));

        let block = reseal_with_receipts(block, &receipts);
        let observation = observe_l2_block(&block, &receipts).unwrap();
        assert_eq!(
            observation
                .inputs
                .advance_tempo
                .final_imported_header()
                .hash(),
            imported_header().hash_slow()
        );
        assert!(matches!(
            &observation.outcomes.events[1].event,
            L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::TempoAdvanced(event))
                if event.tempoBlockHash == forged_hash
        ));
    }

    #[test]
    fn mismatched_enabled_token_output_is_retained_for_later_evaluation() {
        let transactions = vec![advance_transaction_with_tokens(
            ZONE_INBOX_ADDRESS,
            vec![IZoneInbox::EnabledToken {
                token: Address::repeat_byte(0x71),
                name: "Token".into(),
                symbol: "TKN".into(),
                currency: "USD".into(),
            }],
        )];
        let mut logs = advance_logs(None);
        logs.push(token_enabled_log("TKN"));
        let mut receipts = vec![receipt(true, logs)];
        let block = recovered_block(transactions, vec![Address::ZERO], &receipts);

        let observation = observe_l2_block(&block, &receipts).unwrap();
        assert_eq!(observation.inputs.advance_tempo.enabled_tokens().len(), 1);

        receipts[0].logs[2] = token_enabled_log("BAD");
        let block = reseal_with_receipts(block, &receipts);
        let observation = observe_l2_block(&block, &receipts).unwrap();
        assert_eq!(
            observation.inputs.advance_tempo.enabled_tokens()[0].symbol,
            "TKN"
        );
        assert!(matches!(
            &observation.outcomes.events[2].event,
            L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::TokenEnabled(event))
                if event.symbol == "BAD"
        ));
    }

    #[test]
    fn notification_cardinality_errors_are_acquisition_failures() {
        let (block, receipts) = basic_fixture();
        let error = observe_l2_block(&block, &Vec::<TempoReceipt<Log>>::new()).unwrap_err();
        assert!(matches!(
            error,
            ObservationError::Acquisition(AcquisitionError::Inconsistent {
                kind: AcquisitionSource::ZoneNotificationReceipts,
                ..
            })
        ));

        let without_sender = recovered_block(
            vec![advance_transaction(ZONE_INBOX_ADDRESS)],
            vec![],
            &receipts,
        );
        let error = observe_l2_block(&without_sender, &receipts).unwrap_err();
        assert!(matches!(
            error,
            ObservationError::Acquisition(AcquisitionError::Inconsistent {
                kind: AcquisitionSource::ZoneNotificationBlock,
                ..
            })
        ));
    }

    #[test]
    fn receipt_root_and_bloom_are_authenticated_against_the_zone_header() {
        let (block, receipts) = basic_fixture();
        let (receipts_root, logs_bloom) = receipt_commitments(&receipts);

        let wrong_root =
            reseal_with_commitments(block.clone(), B256::repeat_byte(0xa1), logs_bloom);
        assert!(matches!(
            observe_l2_block(&wrong_root, &receipts),
            Err(ObservationError::Acquisition(AcquisitionError::Inconsistent {
                kind: AcquisitionSource::ZoneNotificationReceipts,
                expected,
                ..
            })) if expected.contains("receipts root")
        ));

        let wrong_bloom = reseal_with_commitments(block, receipts_root, Bloom::repeat_byte(0xb2));
        assert!(matches!(
            observe_l2_block(&wrong_bloom, &receipts),
            Err(ObservationError::Acquisition(AcquisitionError::Inconsistent {
                kind: AcquisitionSource::ZoneNotificationReceipts,
                expected,
                ..
            })) if expected.contains("logs bloom")
        ));
    }

    #[test]
    fn opening_envelope_requires_system_identity_destination_and_success() {
        let receipts = vec![receipt(true, advance_logs(None))];
        let wrong_sender = recovered_block(
            vec![advance_transaction(ZONE_INBOX_ADDRESS)],
            vec![Address::repeat_byte(1)],
            &receipts,
        );
        assert!(matches!(
            observe_l2_block(&wrong_sender, &receipts),
            Err(ObservationError::InvalidEnvelope {
                rule: EnvelopeRule::AdvanceSystemCaller,
                ..
            })
        ));

        let wrong_destination = recovered_block(
            vec![advance_transaction(Address::repeat_byte(2))],
            vec![Address::ZERO],
            &receipts,
        );
        assert!(matches!(
            observe_l2_block(&wrong_destination, &receipts),
            Err(ObservationError::InvalidEnvelope {
                rule: EnvelopeRule::AdvanceDestination,
                ..
            })
        ));

        let (block, _) = basic_fixture();
        let failed_receipts = [receipt(false, vec![])];
        let block = reseal_with_receipts(block, &failed_receipts);
        assert!(matches!(
            observe_l2_block(&block, &failed_receipts),
            Err(ObservationError::InvalidEnvelope {
                rule: EnvelopeRule::AdvanceSuccess,
                ..
            })
        ));
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
                transaction_sender: Address::repeat_byte(0x44),
            }
        );
        assert_eq!(
            user_events[1].position(),
            L2EventPosition {
                transaction_index: 1,
                receipt_log_index: 1,
                block_log_index: 3,
                transaction_hash: user_hash,
                transaction_sender: Address::repeat_byte(0x44),
            }
        );
        assert!(matches!(
            user_events[0].event(),
            L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::TempoGasRateUpdated(_))
        ));
        assert_eq!(
            user_events[1].position().transaction_sender(),
            Address::repeat_byte(0x44)
        );
        assert!(matches!(
            user_events[1].event(),
            L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::WithdrawalRequested(event))
                if event.sender == Address::repeat_byte(0x45)
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
                transaction_sender: Address::repeat_byte(0x44),
            }
        );
        assert_eq!(
            user_events[1].position(),
            L2EventPosition {
                transaction_index: 1,
                receipt_log_index: 1,
                block_log_index: 3,
                transaction_hash: user_hash,
                transaction_sender: Address::repeat_byte(0x44),
            }
        );
        assert!(matches!(
            user_events[0].event(),
            L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::WithdrawalRequested(_))
        ));
        assert!(matches!(
            user_events[1].event(),
            L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::TempoGasRateUpdated(_))
        ));
    }

    #[test]
    fn protocol_event_surface_fails_closed_and_external_logs_are_ignored() {
        let (block, mut receipts) = basic_fixture();
        receipts[0].logs.insert(
            0,
            Log {
                address: Address::repeat_byte(0x99),
                data: LogData::new_unchecked(vec![B256::repeat_byte(0xaa)], Bytes::new()),
            },
        );
        let block = reseal_with_receipts(block, &receipts);
        let observation = observe_l2_block(&block, &receipts).unwrap();
        assert_eq!(observation.outcomes.events.len(), 2);

        let (block, mut receipts) = basic_fixture();
        let transaction_hash = *block.body().transactions[0].tx_hash();
        receipts[0].logs.insert(
            0,
            Log {
                address: ZONE_INBOX_ADDRESS,
                data: LogData::new_unchecked(vec![B256::repeat_byte(0xff)], Bytes::new()),
            },
        );
        let block = reseal_with_receipts(block, &receipts);
        let (error, imported_tempo) = observe_l2_block_with_context(&block, &receipts)
            .unwrap_err()
            .into_parts();
        assert_eq!(
            imported_tempo,
            Some(BlockNumHash::new(100, imported_header().hash_slow()))
        );
        assert!(matches!(
            error,
            ObservationError::ProtocolEvent {
                chain: ProtocolChain::ZoneL2,
                transaction_index: 0,
                receipt_log_index: 0,
                block_log_index: 0,
                transaction_hash: actual_hash,
                error,
            } if actual_hash == transaction_hash
                && matches!(error.as_ref(), crate::observe::events::ProtocolEventError::UnsupportedProtocolEvent { .. })
        ));

        let (block, mut receipts) = basic_fixture();
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
        let block = reseal_with_receipts(block, &receipts);
        assert!(matches!(
            observe_l2_block(&block, &receipts),
            Err(ObservationError::ProtocolEvent { error, .. })
                if matches!(error.as_ref(), crate::observe::events::ProtocolEventError::MalformedProtocolEvent { .. })
        ));
    }

    #[test]
    fn deposit_rejected_is_unsupported_not_a_failed_deposit() {
        let (block, mut receipts) = basic_fixture();
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
        let block = reseal_with_receipts(block, &receipts);
        assert!(matches!(
            observe_l2_block(&block, &receipts),
            Err(ObservationError::ProtocolEvent { error, .. })
                if matches!(error.as_ref(), crate::observe::events::ProtocolEventError::UnsupportedProtocolEvent { .. })
        ));
    }

    #[test]
    fn finalization_is_unique_final_and_retains_its_event_output() {
        let advance = advance_transaction(ZONE_INBOX_ADDRESS);
        let finalize = finalization_transaction(ZONE_NUMBER);
        let finalize_hash = *finalize.tx_hash();
        let receipts = vec![
            receipt(true, advance_logs(None)),
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
        let block = recovered_block(
            vec![advance, finalize],
            vec![Address::ZERO, Address::ZERO],
            &receipts,
        );
        let observation = observe_l2_block(&block, &receipts).unwrap();
        assert_eq!(
            observation.inputs.finalization.unwrap().transaction_hash,
            finalize_hash
        );
        assert!(matches!(
            &observation.outcomes.events.last().unwrap().event,
            L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::BatchFinalized(event))
                if event.withdrawalQueueHash == B256::repeat_byte(0xa1)
                    && event.withdrawalBatchIndex == 91
        ));
    }

    #[test]
    fn finalization_rejects_wrong_block_number_and_position() {
        let receipts = vec![receipt(true, advance_logs(None)), receipt(true, vec![])];
        let wrong_number = recovered_block(
            vec![
                advance_transaction(ZONE_INBOX_ADDRESS),
                finalization_transaction(ZONE_NUMBER + 1),
            ],
            vec![Address::ZERO, Address::ZERO],
            &receipts,
        );
        assert!(matches!(
            observe_l2_block(&wrong_number, &receipts),
            Err(ObservationError::InvalidEnvelope {
                rule: EnvelopeRule::FinalizationBlockNumber,
                ..
            })
        ));

        let misplaced_receipts = vec![
            receipt(true, advance_logs(None)),
            receipt(true, vec![]),
            receipt(true, vec![]),
        ];
        let misplaced = recovered_block(
            vec![
                advance_transaction(ZONE_INBOX_ADDRESS),
                finalization_transaction(ZONE_NUMBER),
                user_transaction(0x71),
            ],
            vec![Address::ZERO, Address::ZERO, Address::repeat_byte(3)],
            &misplaced_receipts,
        );
        assert!(matches!(
            observe_l2_block(&misplaced, &misplaced_receipts),
            Err(ObservationError::InvalidEnvelope {
                rule: EnvelopeRule::FinalizationPosition,
                ..
            })
        ));
    }

    #[test]
    fn finalization_requires_system_identity_destination_and_success() {
        let finalization_receipts = vec![receipt(true, advance_logs(None)), receipt(true, vec![])];

        let wrong_sender = recovered_block(
            vec![
                advance_transaction(ZONE_INBOX_ADDRESS),
                finalization_transaction(ZONE_NUMBER),
            ],
            vec![Address::ZERO, Address::repeat_byte(1)],
            &finalization_receipts,
        );
        assert!(matches!(
            observe_l2_block(&wrong_sender, &finalization_receipts),
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
            &finalization_receipts,
        );
        assert!(matches!(
            observe_l2_block(&wrong_destination, &finalization_receipts),
            Err(ObservationError::InvalidEnvelope {
                rule: EnvelopeRule::FinalizationDestination,
                ..
            })
        ));

        let failed_receipts = vec![receipt(true, advance_logs(None)), receipt(false, vec![])];
        let failed = recovered_block(
            vec![
                advance_transaction(ZONE_INBOX_ADDRESS),
                finalization_transaction(ZONE_NUMBER),
            ],
            vec![Address::ZERO, Address::ZERO],
            &failed_receipts,
        );
        assert!(matches!(
            observe_l2_block(&failed, &failed_receipts),
            Err(ObservationError::InvalidEnvelope {
                rule: EnvelopeRule::FinalizationSuccess,
                ..
            })
        ));
    }

    #[test]
    fn malformed_finalization_calldata_has_its_own_error_class() {
        let receipts = vec![receipt(true, advance_logs(None)), receipt(true, vec![])];
        let block = recovered_block(
            vec![
                advance_transaction(ZONE_INBOX_ADDRESS),
                system_transaction(ZONE_OUTBOX_ADDRESS, Bytes::from_static(b"bad")),
            ],
            vec![Address::ZERO, Address::ZERO],
            &receipts,
        );
        let transaction_hash = *block.body().transactions[1].tx_hash();
        let evidence = AuthenticatedDataEvidence::from_bytes(b"bad");
        assert!(matches!(
            observe_l2_block(&block, &receipts),
            Err(ObservationError::MalformedAuthenticatedData {
                kind: DataSource::FinalizationCalldata,
                transaction,
                evidence: actual_evidence,
                ..
            }) if transaction
                == AuthenticatedTransaction::new(ProtocolChain::ZoneL2, 1, transaction_hash)
                && actual_evidence == evidence
        ));
    }
}
