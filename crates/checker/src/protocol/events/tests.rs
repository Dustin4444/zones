use std::mem::size_of;

use alloy_primitives::{Address, B256, Bytes, FixedBytes, Log, LogData, U256, address};
use alloy_sol_types::{SolEvent, SolEventInterface};

use super::{
    factory as factory_model, inbox as inbox_model, outbox as outbox_model,
    portal::{self as portal_model, Portal},
    tempo_state as tempo_state_model, *,
};
use crate::protocol::constants::{
    COMPRESSED_PUBLIC_KEY_SIZE, ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE, MAX_CALLBACK_DATA_SIZE,
    MAX_SEQUENCERS, MAX_TOKEN_CURRENCY_BYTES, MAX_TOKEN_NAME_BYTES, MAX_TOKEN_SYMBOL_BYTES,
    TEMPO_STATE_ADDRESS, ZONE_FACTORY_ADDRESS, ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS,
};

const PORTAL_ADDRESS: Address = address!("0x5ad0000000000000000000000000000000000042");
const EXTERNAL_ADDRESS: Address = address!("0x1111111111111111111111111111111111111111");
const ACCOUNT: Address = address!("0x2222222222222222222222222222222222222222");
const TOKEN: Address = address!("0x3333333333333333333333333333333333333333");
const RECIPIENT: Address = address!("0x4444444444444444444444444444444444444444");

fn event_log<E: SolEvent>(address: Address, event: E) -> Log {
    Log {
        address,
        data: event.encode_log_data(),
    }
}

fn raw_log(address: Address, topics: Vec<B256>, data: Vec<u8>) -> Log {
    Log {
        address,
        data: LogData::new_unchecked(topics, Bytes::from(data)),
    }
}

fn assert_signature<E: SolEvent>(literal: B256, signature: &'static str) {
    assert_eq!(E::SIGNATURE, signature);
    assert_eq!(E::SIGNATURE_HASH, literal);
}

macro_rules! assert_portal_model_vector {
    ($event:expr, $event_ty:ty, $topic:expr, $signature:literal, $variant:pat) => {{
        assert_signature::<$event_ty>($topic, $signature);
        let log = event_log(PORTAL_ADDRESS, $event);
        assert_eq!(log.topics().first(), Some(&$topic));
        let classified = classify_l1_protocol_event(PORTAL_ADDRESS, &log)
            .unwrap()
            .unwrap();
        assert!(matches!(classified, L1ProtocolEvent::Portal($variant)));
    }};
}

macro_rules! assert_portal_known_vector {
    ($event:expr, $event_ty:ty, $topic:expr, $signature:literal) => {{
        assert_signature::<$event_ty>($topic, $signature);
        let log = event_log(PORTAL_ADDRESS, $event);
        assert_eq!(log.topics().first(), Some(&$topic));
        assert_eq!(
            classify_l1_protocol_event(PORTAL_ADDRESS, &log).unwrap(),
            Some(L1ProtocolEvent::KnownNonModel)
        );
    }};
}

macro_rules! assert_factory_vector {
    ($event:expr, $event_ty:ty, $topic:expr, $signature:literal, $pattern:pat) => {{
        assert_signature::<$event_ty>($topic, $signature);
        let log = event_log(ZONE_FACTORY_ADDRESS, $event);
        assert_eq!(log.topics().first(), Some(&$topic));
        let classified = classify_l1_protocol_event(PORTAL_ADDRESS, &log)
            .unwrap()
            .unwrap();
        assert!(matches!(classified, $pattern));
    }};
}

macro_rules! assert_inbox_vector {
    ($event:expr, $event_ty:ty, $topic:expr, $signature:literal, $variant:pat) => {{
        assert_signature::<$event_ty>($topic, $signature);
        let log = event_log(ZONE_INBOX_ADDRESS, $event);
        assert_eq!(log.topics().first(), Some(&$topic));
        let classified = classify_l2_protocol_event(&log).unwrap().unwrap();
        assert!(matches!(classified, L2ProtocolEvent::Inbox($variant)));
    }};
}

macro_rules! assert_outbox_vector {
    ($event:expr, $event_ty:ty, $topic:expr, $signature:literal, $variant:pat) => {{
        assert_signature::<$event_ty>($topic, $signature);
        let log = event_log(ZONE_OUTBOX_ADDRESS, $event);
        assert_eq!(log.topics().first(), Some(&$topic));
        let classified = classify_l2_protocol_event(&log).unwrap().unwrap();
        assert!(matches!(classified, L2ProtocolEvent::Outbox($variant)));
    }};
}

#[test]
fn model_event_portal_vectors_classify_exactly() {
    assert_eq!(Portal::PortalEvents::COUNT, 21);

    assert_portal_model_vector!(
        Portal::DepositMade {
            newCurrentDepositQueueHash: B256::repeat_byte(1),
            sender: ACCOUNT,
            token: TOKEN,
            netAmount: 4,
            fee: 5,
            keyIndex: U256::from(6),
            ephemeralPubkeyX: B256::repeat_byte(7),
            ephemeralPubkeyYParity: 2,
            ciphertext: Bytes::from(vec![8; ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE]),
            nonce: FixedBytes::<12>::repeat_byte(9),
            tag: FixedBytes::<16>::repeat_byte(10),
            tempoRefundRecipient: RECIPIENT,
            depositNumber: 11,
        },
        Portal::DepositMade,
        portal_model::DEPOSIT_MADE_TOPIC,
        "DepositMade(bytes32,address,address,uint128,uint128,uint256,bytes32,uint8,bytes,bytes12,bytes16,address,uint64)",
        PortalModelEvent::DepositMade(_)
    );
    assert_portal_model_vector!(
        Portal::TokenEnabled {
            token: TOKEN,
            name: "Token".into(),
            symbol: "TOK".into(),
            currency: "USD".into(),
        },
        Portal::TokenEnabled,
        portal_model::TOKEN_ENABLED_TOPIC,
        "TokenEnabled(address,string,string,string)",
        PortalModelEvent::TokenEnabled(_)
    );
    assert_portal_model_vector!(
        Portal::BatchSubmitted {
            withdrawalBatchIndex: 1,
            withdrawalQueueIndex: U256::from(2),
            nextProcessedDepositQueueHash: B256::repeat_byte(3),
            nextBlockHash: B256::repeat_byte(4),
            withdrawalQueueHash: B256::repeat_byte(5),
            lastProcessedDepositNumber: 6,
        },
        Portal::BatchSubmitted,
        portal_model::BATCH_SUBMITTED_TOPIC,
        "BatchSubmitted(uint64,uint256,bytes32,bytes32,bytes32,uint64)",
        PortalModelEvent::BatchSubmitted(_)
    );
    assert_portal_model_vector!(
        Portal::WithdrawalProcessed {
            to: RECIPIENT,
            senderTag: B256::repeat_byte(1),
            token: TOKEN,
            amount: 2,
            callbackSuccess: true,
        },
        Portal::WithdrawalProcessed,
        portal_model::WITHDRAWAL_PROCESSED_TOPIC,
        "WithdrawalProcessed(address,bytes32,address,uint128,bool)",
        PortalModelEvent::WithdrawalProcessed(_)
    );
    assert_portal_model_vector!(
        Portal::WithdrawalBounceBack {
            newCurrentDepositQueueHash: B256::repeat_byte(1),
            fallbackNonce: 2,
            token: TOKEN,
            amount: 3,
            depositNumber: 4,
        },
        Portal::WithdrawalBounceBack,
        portal_model::WITHDRAWAL_BOUNCE_BACK_TOPIC,
        "WithdrawalBounceBack(bytes32,uint64,address,uint128,uint64)",
        PortalModelEvent::WithdrawalBounceBack(_)
    );
    assert_portal_model_vector!(
        Portal::DepositBounceBack {
            tempoRefundRecipient: RECIPIENT,
            token: TOKEN,
            amount: 3,
            bouncebackFee: 4,
        },
        Portal::DepositBounceBack,
        portal_model::DEPOSIT_BOUNCE_BACK_TOPIC,
        "DepositBounceBack(address,address,uint128,uint128)",
        PortalModelEvent::DepositBounceBack(_)
    );
    assert_portal_model_vector!(
        Portal::DepositBounceBackPending {
            tempoRefundRecipient: RECIPIENT,
            token: TOKEN,
            amount: 3,
            bouncebackFee: 4,
        },
        Portal::DepositBounceBackPending,
        portal_model::DEPOSIT_BOUNCE_BACK_PENDING_TOPIC,
        "DepositBounceBackPending(address,address,uint128,uint128)",
        PortalModelEvent::DepositBounceBackPending(_)
    );
    assert_portal_model_vector!(
        Portal::RefundClaimed {
            recipient: RECIPIENT,
            token: TOKEN,
            amount: 3
        },
        Portal::RefundClaimed,
        portal_model::REFUND_CLAIMED_TOPIC,
        "RefundClaimed(address,address,uint128)",
        PortalModelEvent::RefundClaimed(_)
    );
    assert_portal_model_vector!(
        Portal::BouncebackGasUpdated { bouncebackGas: 7 },
        Portal::BouncebackGasUpdated,
        portal_model::BOUNCEBACK_GAS_UPDATED_TOPIC,
        "BouncebackGasUpdated(uint64)",
        PortalModelEvent::BouncebackGasUpdated(_)
    );

    assert_portal_known_vector!(
        Portal::SequencerEncryptionKeyUpdated {
            x: B256::repeat_byte(1),
            yParity: 2,
            keyIndex: U256::from(3),
            activationBlock: 4,
        },
        Portal::SequencerEncryptionKeyUpdated,
        portal_model::SEQUENCER_ENCRYPTION_KEY_UPDATED_TOPIC,
        "SequencerEncryptionKeyUpdated(bytes32,uint8,uint256,uint64)"
    );
    assert_portal_known_vector!(
        Portal::ZoneGasRateUpdated { zoneGasRate: 1 },
        Portal::ZoneGasRateUpdated,
        portal_model::ZONE_GAS_RATE_UPDATED_TOPIC,
        "ZoneGasRateUpdated(uint128)"
    );
    assert_portal_known_vector!(
        Portal::MaxTempoGasRateUpdated { maxTempoGasRate: 1 },
        Portal::MaxTempoGasRateUpdated,
        portal_model::MAX_TEMPO_GAS_RATE_UPDATED_TOPIC,
        "MaxTempoGasRateUpdated(uint128)"
    );
    assert_portal_known_vector!(
        Portal::AdminTransferStarted {
            currentAdmin: ACCOUNT,
            pendingAdmin: RECIPIENT
        },
        Portal::AdminTransferStarted,
        portal_model::ADMIN_TRANSFER_STARTED_TOPIC,
        "AdminTransferStarted(address,address)"
    );
    assert_portal_known_vector!(
        Portal::AdminTransferred {
            previousAdmin: ACCOUNT,
            newAdmin: RECIPIENT
        },
        Portal::AdminTransferred,
        portal_model::ADMIN_TRANSFERRED_TOPIC,
        "AdminTransferred(address,address)"
    );
    assert_portal_known_vector!(
        Portal::RoleUpdated {
            account: ACCOUNT,
            prev: Portal::Role::None,
            next: Portal::Role::Account,
        },
        Portal::RoleUpdated,
        portal_model::ROLE_UPDATED_TOPIC,
        "RoleUpdated(address,uint8,uint8)"
    );
    assert_portal_known_vector!(
        Portal::EnforcementModesUpdated {
            accessMode: true,
            gatewayMode: false
        },
        Portal::EnforcementModesUpdated,
        portal_model::ENFORCEMENT_MODES_UPDATED_TOPIC,
        "EnforcementModesUpdated(bool,bool)"
    );
    assert_portal_known_vector!(
        Portal::SequencerSetUpdated {
            nonce: 1,
            threshold: 1,
            sequencers: vec![ACCOUNT]
        },
        Portal::SequencerSetUpdated,
        portal_model::SEQUENCER_SET_UPDATED_TOPIC,
        "SequencerSetUpdated(uint64,uint8,address[])"
    );
    assert_portal_known_vector!(
        Portal::LeaderUpdated {
            previousLeader: ACCOUNT,
            newLeader: RECIPIENT,
            epoch: 2,
            activationTempoBlock: 3,
        },
        Portal::LeaderUpdated,
        portal_model::LEADER_UPDATED_TOPIC,
        "LeaderUpdated(address,address,uint64,uint64)"
    );
    assert_portal_known_vector!(
        Portal::DepositsPaused { token: TOKEN },
        Portal::DepositsPaused,
        portal_model::DEPOSITS_PAUSED_TOPIC,
        "DepositsPaused(address)"
    );
    assert_portal_known_vector!(
        Portal::DepositsResumed { token: TOKEN },
        Portal::DepositsResumed,
        portal_model::DEPOSITS_RESUMED_TOPIC,
        "DepositsResumed(address)"
    );
    assert_portal_known_vector!(
        Portal::RpcUrlUpdated {
            rpcUrl: "https://rpc.invalid".into()
        },
        Portal::RpcUrlUpdated,
        portal_model::RPC_URL_UPDATED_TOPIC,
        "RpcUrlUpdated(string)"
    );
}

#[test]
fn model_event_factory_vectors_classify_exactly() {
    assert_eq!(Factory::FactoryEvents::COUNT, 2);
    assert_factory_vector!(
        Factory::ZoneCreated {
            zoneId: 1,
            portal: PORTAL_ADDRESS,
            initialToken: TOKEN,
            accessMode: true,
            gatewayMode: false,
            admin: ACCOUNT,
            sequencers: vec![ACCOUNT],
            threshold: 1,
            verifier: RECIPIENT,
        },
        Factory::ZoneCreated,
        factory_model::ZONE_CREATED_TOPIC,
        "ZoneCreated(uint32,address,address,bool,bool,address,address[],uint8,address)",
        L1ProtocolEvent::FactoryZoneCreated(_)
    );
    assert_factory_vector!(
        Factory::OwnershipTransferred {
            previousOwner: ACCOUNT,
            newOwner: RECIPIENT
        },
        Factory::OwnershipTransferred,
        factory_model::OWNERSHIP_TRANSFERRED_TOPIC,
        "OwnershipTransferred(address,address)",
        L1ProtocolEvent::KnownNonModel
    );

    let other_portal = Address::repeat_byte(0x55);
    let unrelated_zone = event_log(
        ZONE_FACTORY_ADDRESS,
        Factory::ZoneCreated {
            zoneId: 2,
            portal: other_portal,
            initialToken: TOKEN,
            accessMode: true,
            gatewayMode: false,
            admin: ACCOUNT,
            sequencers: vec![ACCOUNT],
            threshold: 1,
            verifier: RECIPIENT,
        },
    );
    assert_eq!(
        classify_l1_protocol_event(PORTAL_ADDRESS, &unrelated_zone),
        Ok(Some(L1ProtocolEvent::KnownNonModel))
    );
}

#[test]
fn model_event_inbox_vectors_classify_exactly() {
    assert_eq!(Inbox::InboxEvents::COUNT, 7);
    assert_inbox_vector!(
        Inbox::TempoAdvanced {
            tempoBlockHash: B256::repeat_byte(1),
            tempoBlockNumber: 2,
            depositsProcessed: U256::from(3),
            newProcessedDepositQueueHash: B256::repeat_byte(4),
            lastProcessedDepositNumber: 5,
        },
        Inbox::TempoAdvanced,
        inbox_model::TEMPO_ADVANCED_TOPIC,
        "TempoAdvanced(bytes32,uint64,uint256,bytes32,uint64)",
        Inbox::InboxEvents::TempoAdvanced(_)
    );
    assert_inbox_vector!(
        Inbox::DepositProcessed {
            depositHash: B256::repeat_byte(1),
            sender: ACCOUNT,
            to: RECIPIENT,
            token: TOKEN,
            amount: 2,
            memo: B256::repeat_byte(3),
        },
        Inbox::DepositProcessed,
        inbox_model::DEPOSIT_PROCESSED_TOPIC,
        "DepositProcessed(bytes32,address,address,address,uint128,bytes32)",
        Inbox::InboxEvents::DepositProcessed(_)
    );
    assert_inbox_vector!(
        Inbox::DepositFailed {
            depositHash: B256::repeat_byte(1),
            sender: ACCOUNT,
            token: TOKEN,
            amount: 2,
        },
        Inbox::DepositFailed,
        inbox_model::DEPOSIT_FAILED_TOPIC,
        "DepositFailed(bytes32,address,address,uint128)",
        Inbox::InboxEvents::DepositFailed(_)
    );
    assert_inbox_vector!(
        Inbox::WithdrawalBounceBackProcessed {
            zoneFallbackRecipient: RECIPIENT,
            token: TOKEN,
            amount: 2,
        },
        Inbox::WithdrawalBounceBackProcessed,
        inbox_model::WITHDRAWAL_BOUNCE_BACK_PROCESSED_TOPIC,
        "WithdrawalBounceBackProcessed(address,address,uint128)",
        Inbox::InboxEvents::WithdrawalBounceBackProcessed(_)
    );
    assert_inbox_vector!(
        Inbox::WithdrawalBounceBackPending {
            zoneFallbackRecipient: RECIPIENT,
            token: TOKEN,
            amount: 2,
        },
        Inbox::WithdrawalBounceBackPending,
        inbox_model::WITHDRAWAL_BOUNCE_BACK_PENDING_TOPIC,
        "WithdrawalBounceBackPending(address,address,uint128)",
        Inbox::InboxEvents::WithdrawalBounceBackPending(_)
    );
    assert_inbox_vector!(
        Inbox::RefundClaimed {
            recipient: RECIPIENT,
            token: TOKEN,
            amount: 2
        },
        Inbox::RefundClaimed,
        inbox_model::REFUND_CLAIMED_TOPIC,
        "RefundClaimed(address,address,uint128)",
        Inbox::InboxEvents::RefundClaimed(_)
    );
    assert_inbox_vector!(
        Inbox::TokenEnabled {
            token: TOKEN,
            name: "Token".into(),
            symbol: "TOK".into(),
            currency: "USD".into(),
        },
        Inbox::TokenEnabled,
        inbox_model::TOKEN_ENABLED_TOPIC,
        "TokenEnabled(address,string,string,string)",
        Inbox::InboxEvents::TokenEnabled(_)
    );

    assert_signature::<inbox_model::DepositRejected>(
        inbox_model::DEPOSIT_REJECTED_TOPIC,
        "DepositRejected(bytes32,address,uint8,address,uint128,address)",
    );
    assert_eq!(
        portal_model::TOKEN_ENABLED_TOPIC,
        inbox_model::TOKEN_ENABLED_TOPIC
    );
    assert_eq!(
        portal_model::REFUND_CLAIMED_TOPIC,
        inbox_model::REFUND_CLAIMED_TOPIC
    );
}

#[test]
fn model_event_outbox_and_tempo_vectors_classify_exactly() {
    assert_eq!(Outbox::OutboxEvents::COUNT, 4);
    assert_outbox_vector!(
        Outbox::WithdrawalRequested {
            withdrawalIndex: 1,
            sender: ACCOUNT,
            token: TOKEN,
            to: RECIPIENT,
            amount: 2,
            fee: 3,
            memo: B256::repeat_byte(4),
            gasLimit: 5,
            fallbackNonce: 6,
            data: Bytes::new(),
            revealTo: Bytes::new(),
        },
        Outbox::WithdrawalRequested,
        outbox_model::WITHDRAWAL_REQUESTED_TOPIC,
        "WithdrawalRequested(uint64,address,address,address,uint128,uint128,bytes32,uint64,uint64,bytes,bytes)",
        Outbox::OutboxEvents::WithdrawalRequested(_)
    );
    assert_outbox_vector!(
        Outbox::BatchFinalized {
            withdrawalQueueHash: B256::repeat_byte(1),
            withdrawalBatchIndex: 2
        },
        Outbox::BatchFinalized,
        outbox_model::BATCH_FINALIZED_TOPIC,
        "BatchFinalized(bytes32,uint64)",
        Outbox::OutboxEvents::BatchFinalized(_)
    );
    assert_outbox_vector!(
        Outbox::TempoGasRateUpdated { tempoGasRate: 1 },
        Outbox::TempoGasRateUpdated,
        outbox_model::TEMPO_GAS_RATE_UPDATED_TOPIC,
        "TempoGasRateUpdated(uint128)",
        Outbox::OutboxEvents::TempoGasRateUpdated(_)
    );
    assert_outbox_vector!(
        Outbox::MaxWithdrawalsPerBlockUpdated {
            maxWithdrawalsPerBlock: 1
        },
        Outbox::MaxWithdrawalsPerBlockUpdated,
        outbox_model::MAX_WITHDRAWALS_PER_BLOCK_UPDATED_TOPIC,
        "MaxWithdrawalsPerBlockUpdated(uint32)",
        Outbox::OutboxEvents::MaxWithdrawalsPerBlockUpdated(_)
    );

    assert_eq!(TempoState::TempoStateEvents::COUNT, 1);
    assert_signature::<TempoState::TempoBlockFinalized>(
        tempo_state_model::BLOCK_FINALIZED_TOPIC,
        "TempoBlockFinalized(bytes32,uint64,bytes32)",
    );
    let log = event_log(
        TEMPO_STATE_ADDRESS,
        TempoState::TempoBlockFinalized {
            blockHash: B256::repeat_byte(1),
            blockNumber: 2,
            stateRoot: B256::repeat_byte(3),
        },
    );
    assert!(matches!(
        classify_l2_protocol_event(&log),
        Ok(Some(L2ProtocolEvent::TempoState(
            TempoState::TempoStateEvents::TempoBlockFinalized(_)
        )))
    ));
}

#[test]
fn model_event_unknown_topicless_and_external_logs_follow_emitter_boundary() {
    let external = raw_log(
        EXTERNAL_ADDRESS,
        vec![portal_model::BOUNCEBACK_GAS_UPDATED_TOPIC],
        vec![],
    );
    assert_eq!(
        classify_l1_protocol_event(PORTAL_ADDRESS, &external).unwrap(),
        None
    );
    assert_eq!(classify_l2_protocol_event(&external).unwrap(), None);

    let unknown_portal = raw_log(PORTAL_ADDRESS, vec![B256::repeat_byte(0x42)], vec![]);
    assert!(matches!(
        classify_l1_protocol_event(PORTAL_ADDRESS, &unknown_portal),
        Err(ProtocolEventError::UnsupportedProtocolEvent { .. })
    ));
    let topicless_factory = raw_log(ZONE_FACTORY_ADDRESS, vec![], vec![]);
    assert!(matches!(
        classify_l1_protocol_event(PORTAL_ADDRESS, &topicless_factory),
        Err(ProtocolEventError::UnsupportedProtocolEvent { topic0: None, .. })
    ));
    let wrong_outbox_topic = raw_log(
        ZONE_OUTBOX_ADDRESS,
        vec![portal_model::BOUNCEBACK_GAS_UPDATED_TOPIC],
        vec![],
    );
    assert!(matches!(
        classify_l2_protocol_event(&wrong_outbox_topic),
        Err(ProtocolEventError::UnsupportedProtocolEvent { .. })
    ));
}

#[test]
fn model_event_deposit_rejected_is_explicitly_unsupported() {
    let log = event_log(
        ZONE_INBOX_ADDRESS,
        inbox_model::DepositRejected {
            depositHash: B256::repeat_byte(1),
            sender: ACCOUNT,
            depositType: inbox_model::DepositType::Deposit,
            token: TOKEN,
            amount: 4,
            tempoRefundRecipient: RECIPIENT,
        },
    );
    assert!(matches!(
        classify_l2_protocol_event(&log),
        Err(ProtocolEventError::UnsupportedProtocolEvent {
            emitter: ZONE_INBOX_ADDRESS,
            topic0: Some(inbox_model::DEPOSIT_REJECTED_TOPIC),
        })
    ));
}

#[test]
fn model_event_known_events_require_canonical_topics_and_data() {
    let event = Portal::BouncebackGasUpdated { bouncebackGas: 7 };
    let mut log = event_log(PORTAL_ADDRESS, event);
    log.data.data = [log.data.data.as_ref(), &[0]].concat().into();
    assert!(matches!(
        classify_l1_protocol_event(PORTAL_ADDRESS, &log),
        Err(ProtocolEventError::MalformedProtocolEvent { .. })
    ));

    let bad_topic_count = raw_log(
        PORTAL_ADDRESS,
        vec![portal_model::BOUNCEBACK_GAS_UPDATED_TOPIC, B256::ZERO],
        vec![0; 32],
    );
    assert!(matches!(
        classify_l1_protocol_event(PORTAL_ADDRESS, &bad_topic_count),
        Err(ProtocolEventError::MalformedProtocolEvent { .. })
    ));
}

#[test]
fn model_event_dynamic_fields_enforce_typed_protocol_bounds() {
    fn deposit(ciphertext_len: usize) -> Portal::DepositMade {
        Portal::DepositMade {
            newCurrentDepositQueueHash: B256::repeat_byte(1),
            sender: ACCOUNT,
            token: TOKEN,
            netAmount: 4,
            fee: 5,
            keyIndex: U256::from(6),
            ephemeralPubkeyX: B256::repeat_byte(7),
            ephemeralPubkeyYParity: 2,
            ciphertext: Bytes::from(vec![8; ciphertext_len]),
            nonce: FixedBytes::<12>::repeat_byte(9),
            tag: FixedBytes::<16>::repeat_byte(10),
            tempoRefundRecipient: RECIPIENT,
            depositNumber: 11,
        }
    }
    assert!(
        classify_l1_protocol_event(
            PORTAL_ADDRESS,
            &event_log(PORTAL_ADDRESS, deposit(ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE))
        )
        .is_ok()
    );
    assert!(matches!(
        classify_l1_protocol_event(
            PORTAL_ADDRESS,
            &event_log(
                PORTAL_ADDRESS,
                deposit(ENCRYPTED_DEPOSIT_CIPHERTEXT_SIZE - 1)
            )
        ),
        Err(ProtocolEventError::MalformedProtocolEvent { .. })
    ));

    let long_portal_metadata = Portal::TokenEnabled {
        token: TOKEN,
        name: "n".repeat(MAX_TOKEN_NAME_BYTES + 1),
        symbol: "s".repeat(MAX_TOKEN_SYMBOL_BYTES),
        currency: "c".repeat(MAX_TOKEN_CURRENCY_BYTES),
    };
    assert!(matches!(
        classify_l1_protocol_event(
            PORTAL_ADDRESS,
            &event_log(PORTAL_ADDRESS, long_portal_metadata)
        ),
        Err(ProtocolEventError::MalformedProtocolEvent { .. })
    ));
    let long_inbox_metadata = Inbox::TokenEnabled {
        token: TOKEN,
        name: "n".repeat(MAX_TOKEN_NAME_BYTES),
        symbol: "s".repeat(MAX_TOKEN_SYMBOL_BYTES + 1),
        currency: "c".repeat(MAX_TOKEN_CURRENCY_BYTES),
    };
    assert!(matches!(
        classify_l2_protocol_event(&event_log(ZONE_INBOX_ADDRESS, long_inbox_metadata)),
        Err(ProtocolEventError::MalformedProtocolEvent { .. })
    ));

    let too_many_portal_sequencers = Portal::SequencerSetUpdated {
        nonce: 1,
        threshold: 1,
        sequencers: vec![ACCOUNT; MAX_SEQUENCERS + 1],
    };
    assert!(matches!(
        classify_l1_protocol_event(
            PORTAL_ADDRESS,
            &event_log(PORTAL_ADDRESS, too_many_portal_sequencers)
        ),
        Err(ProtocolEventError::MalformedProtocolEvent { .. })
    ));
    let too_many_factory_sequencers = Factory::ZoneCreated {
        zoneId: 1,
        portal: PORTAL_ADDRESS,
        initialToken: TOKEN,
        accessMode: true,
        gatewayMode: false,
        admin: ACCOUNT,
        sequencers: vec![ACCOUNT; MAX_SEQUENCERS + 1],
        threshold: 1,
        verifier: RECIPIENT,
    };
    assert!(matches!(
        classify_l1_protocol_event(
            PORTAL_ADDRESS,
            &event_log(ZONE_FACTORY_ADDRESS, too_many_factory_sequencers)
        ),
        Err(ProtocolEventError::MalformedProtocolEvent { .. })
    ));

    fn withdrawal(data_len: usize, reveal_len: usize) -> Outbox::WithdrawalRequested {
        Outbox::WithdrawalRequested {
            withdrawalIndex: 1,
            sender: ACCOUNT,
            token: TOKEN,
            to: RECIPIENT,
            amount: 5,
            fee: 6,
            memo: B256::repeat_byte(7),
            gasLimit: 8,
            fallbackNonce: 9,
            data: Bytes::from(vec![10; data_len]),
            revealTo: Bytes::from(vec![2; reveal_len]),
        }
    }
    for reveal_len in [0, COMPRESSED_PUBLIC_KEY_SIZE] {
        assert!(
            classify_l2_protocol_event(&event_log(
                ZONE_OUTBOX_ADDRESS,
                withdrawal(MAX_CALLBACK_DATA_SIZE, reveal_len)
            ))
            .is_ok()
        );
    }
    assert!(matches!(
        classify_l2_protocol_event(&event_log(
            ZONE_OUTBOX_ADDRESS,
            withdrawal(MAX_CALLBACK_DATA_SIZE + 1, 0)
        )),
        Err(ProtocolEventError::MalformedProtocolEvent { .. })
    ));
    assert!(matches!(
        classify_l2_protocol_event(&event_log(
            ZONE_OUTBOX_ADDRESS,
            withdrawal(0, COMPRESSED_PUBLIC_KEY_SIZE - 1)
        )),
        Err(ProtocolEventError::MalformedProtocolEvent { .. })
    ));
}

#[test]
fn model_event_array_count_is_guarded_before_generated_vec_allocation() {
    // SequencerSetUpdated body: threshold, address[] offset, then count.
    let mut data = vec![0; 96];
    data[63] = 64;
    data[64..96].copy_from_slice(&usize::MAX.to_be_bytes().repeat(32 / size_of::<usize>()));
    let log = raw_log(
        PORTAL_ADDRESS,
        vec![portal_model::SEQUENCER_SET_UPDATED_TOPIC],
        data,
    );
    assert!(matches!(
        classify_l1_protocol_event(PORTAL_ADDRESS, &log),
        Err(ProtocolEventError::MalformedProtocolEvent { .. })
    ));
}

#[test]
fn model_event_rpc_url_is_structurally_bounded_without_an_invented_cap() {
    let event = Portal::RpcUrlUpdated {
        rpcUrl: "x".repeat(4_096),
    };
    assert!(classify_l1_protocol_event(PORTAL_ADDRESS, &event_log(PORTAL_ADDRESS, event)).is_ok());

    // Offset 32, declared length usize::MAX, but no supplied payload. Alloy's
    // packed-bytes decoder bounds-checks the source slice before materializing
    // the String, so this remains allocation-safe without a semantic URL cap.
    let mut data = vec![0; 64];
    data[31] = 32;
    data[32..64].copy_from_slice(&usize::MAX.to_be_bytes().repeat(32 / size_of::<usize>()));
    let log = raw_log(
        PORTAL_ADDRESS,
        vec![portal_model::RPC_URL_UPDATED_TOPIC],
        data,
    );
    assert!(matches!(
        classify_l1_protocol_event(PORTAL_ADDRESS, &log),
        Err(ProtocolEventError::MalformedProtocolEvent { .. })
    ));
}
