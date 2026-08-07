use std::num::NonZeroU64;

use crate::kernel::{
    BatchSubmission, BounceBackDeposit, Cursor, Deposit, DepositOutcome, ExpectedState,
    Finalization, ImportedFacts, ImportedOperation, OrdinaryDeposit, PortalIdentity, RefundClaim,
    TokenEnable, UserWithdrawal, Withdrawal, WithdrawalOutcome, WithdrawalProcessing, ZoneFacts,
    ZoneOperation,
};
use alloy_consensus::BlockHeader as _;
use alloy_primitives::{Address, B256, FixedBytes, U256};

use crate::{
    observe::{L1BlockObservation, L2BlockObservation, ZonePostStateOutputs},
    persistence::{BlockNumHash, CoverageGapReason},
    protocol::events::{
        Factory, Inbox, L1ProtocolEvent, L2ProtocolEvent, Outbox, PortalEvent, TempoState,
    },
    runtime::{AuthenticatedBlock, AuthenticatedOutputs, Failure, FailureClass},
};

use crate::kernel::Effect;

pub(crate) struct AuthenticatedObservation {
    pub l2: L2BlockObservation,
    pub l1: Vec<L1BlockObservation>,
    pub state: ZonePostStateOutputs,
    pub portal_creation_block_hash: B256,
    pub zone_id: u32,
}

#[derive(Debug, Clone, Copy)]
#[repr(u16)]
pub(crate) enum AdapterFindingCode {
    HeaderSequence = 100,
    Grammar = 200,
}

fn failure(code: AdapterFindingCode, message: impl Into<String>) -> Failure {
    let code = code as u16;
    Failure {
        class: FailureClass::AuthenticatedDivergence,
        gap_reason: CoverageGapReason::NotCheckedAncestorDivergence,
        message: message.into(),
        finding: Some(Box::new(crate::kernel::Finding {
            category: crate::kernel::FindingCategory::Observation,
            code,
            location: Some(crate::kernel::FindingLocation::Block),
            expected: None,
            actual: Some(crate::kernel::Datum::Code(code)),
        })),
    }
}

pub(crate) fn adapt(o: &AuthenticatedObservation) -> Result<AuthenticatedBlock, Failure> {
    let headers = o.l2.inputs().advance_tempo().imported_headers();
    if o.l1.len() != headers.len() {
        return Err(failure(
            AdapterFindingCode::HeaderSequence,
            "Tempo observation count does not match advanceTempo headers",
        ));
    }
    let mut imported_facts = ImportedFacts {
        block_hash: headers.last().expect("headers are nonempty").hash(),
        block_number: headers.last().expect("headers are nonempty").number(),
        operations: Vec::new(),
    };
    let mut imported_effects = Vec::new();
    for (observation, header) in o.l1.iter().zip(headers) {
        if (observation.block_hash(), observation.block_number())
            != (header.hash(), header.number())
        {
            return Err(failure(
                AdapterFindingCode::HeaderSequence,
                "Tempo observation does not match advanceTempo header",
            ));
        }
        let (facts, effects) =
            adapt_imported(observation, header, o.portal_creation_block_hash, o.zone_id)?;
        imported_facts.operations.extend(facts.operations);
        imported_effects.extend(effects);
    }
    let (zone_facts, mut zone_effects) = zone_facts(o)?;
    imported_effects.append(&mut zone_effects);
    let state = ExpectedState {
        tempo_block_hash: o.state.tempo_block_hash(),
        tempo_block_number: o.state.tempo_block_number(),
        processed_deposit_hash: o.state.processed_deposit_queue_hash(),
        processed_deposit_number: o.state.processed_deposit_number(),
        withdrawal_queue_hash: o.state.withdrawal_queue_hash(),
        withdrawal_batch_index: o.state.withdrawal_batch_index(),
    };
    let first = headers.first().expect("headers are nonempty");
    let final_observation = o.l1.last().expect("matched nonempty headers");
    Ok(AuthenticatedBlock {
        zone: BlockNumHash {
            number: o.l2.block_number(),
            hash: o.l2.block_hash(),
        },
        parent: BlockNumHash {
            number: o.l2.block_number() - 1,
            hash: o.l2.parent_hash(),
        },
        tempo: BlockNumHash {
            number: final_observation.block_number(),
            hash: final_observation.block_hash(),
        },
        tempo_parent: BlockNumHash {
            number: first.number().checked_sub(1).ok_or_else(|| {
                failure(
                    AdapterFindingCode::HeaderSequence,
                    "imported genesis has no parent",
                )
            })?,
            hash: first.header().parent_hash(),
        },
        imported: imported_facts,
        zone_facts,
        outputs: AuthenticatedOutputs {
            effects: imported_effects,
            state,
            supplies: o.state.token_supplies().clone(),
        },
    })
}

pub(crate) fn adapt_imported(
    observation: &L1BlockObservation,
    header: &crate::observe::ImportedTempoHeader,
    portal_creation_block_hash: B256,
    zone_id: u32,
) -> Result<(ImportedFacts, Vec<Effect>), Failure> {
    if (observation.block_hash(), observation.block_number()) != (header.hash(), header.number()) {
        return Err(failure(
            AdapterFindingCode::Grammar,
            "Tempo observation does not match imported header",
        ));
    }
    imported_facts(observation, header, portal_creation_block_hash, zone_id)
}

fn token(token: Address, name: &str, symbol: &str, currency: &str) -> TokenEnable {
    TokenEnable {
        token,
        name: name.into(),
        symbol: symbol.into(),
        currency: currency.into(),
    }
}

fn ordinary(d: &tempo_zone_contracts::ZonePortal::Deposit) -> Result<OrdinaryDeposit, Failure> {
    let ciphertext: [u8; 64] = d.encrypted.ciphertext.as_ref().try_into().map_err(|_| {
        failure(
            AdapterFindingCode::Grammar,
            "deposit ciphertext is not 64 bytes",
        )
    })?;
    Ok(OrdinaryDeposit {
        token: d.token,
        sender: d.sender,
        amount: d.amount,
        tempo_refund_recipient: d.tempoRefundRecipient,
        key_index: d.keyIndex,
        encrypted: crate::kernel::DepositPayload {
            ephemeral_pubkey_x: d.encrypted.ephemeralPubkeyX,
            ephemeral_pubkey_y_parity: d.encrypted.ephemeralPubkeyYParity,
            ciphertext: FixedBytes::from(ciphertext),
            nonce: d.encrypted.nonce,
            tag: d.encrypted.tag,
        },
    })
}

fn imported_facts(
    observation: &L1BlockObservation,
    header: &crate::observe::ImportedTempoHeader,
    portal_creation_block_hash: B256,
    zone_id: u32,
) -> Result<(ImportedFacts, Vec<Effect>), Failure> {
    let mut operations = Vec::new();
    let mut effects = Vec::new();
    for tx in observation.protocol_transactions() {
        let events: Vec<_> = tx.outcomes().iter().map(|x| x.event()).collect();
        if let Some(call) = tx.direct_call() {
            if let Some(call) = call.as_submit_batch() {
                if !matches!(
                    events.as_slice(),
                    [L1ProtocolEvent::Portal(PortalEvent::BatchSubmitted(_))]
                ) {
                    return Err(failure(
                        AdapterFindingCode::Grammar,
                        "submitBatch requires exactly one BatchSubmitted event",
                    ));
                }
                operations.push(ImportedOperation::SubmitBatch(BatchSubmission {
                    tempo_block: call.tempoBlockNumber,
                    previous_block: call.blockTransition.prevBlockHash,
                    next_block: call.blockTransition.nextBlockHash,
                    previous_deposit: Cursor {
                        hash: call.depositQueueTransition.prevProcessedHash,
                        number: call.depositQueueTransition.prevDepositNumber,
                    },
                    next_deposit: Cursor {
                        hash: call.depositQueueTransition.nextProcessedHash,
                        number: call.depositQueueTransition.nextDepositNumber,
                    },
                    withdrawal_queue_hash: call.withdrawalQueueHash,
                    next_zone_height: call.nextZoneHeight,
                }));
            } else if let Some(call) = call.as_process_withdrawals() {
                let withdrawals = call
                    .withdrawals
                    .iter()
                    .map(|w| Withdrawal {
                        token: w.token,
                        sender_tag: w.senderTag,
                        to: w.to,
                        amount: w.amount,
                        memo: w.memo,
                        gas_limit: w.gasLimit,
                        fallback_nonce: w.fallbackNonce,
                        callback_data: w.callbackData.clone(),
                        encrypted_sender: w.encryptedSender.clone(),
                    })
                    .collect();
                let (outcomes, processing_effects) = parse_withdrawal_events(
                    &events,
                    call.withdrawals.len(),
                    observation.portal_address(),
                )?;
                effects.extend(processing_effects);
                operations.push(ImportedOperation::ProcessWithdrawals(
                    WithdrawalProcessing {
                        base_fee: U256::from(header.header().base_fee_per_gas().ok_or_else(
                            || {
                                failure(
                                    AdapterFindingCode::Grammar,
                                    "imported header missing base fee",
                                )
                            },
                        )?),
                        withdrawals,
                        remaining_queue: call.remainingQueue,
                        outcomes,
                    },
                ));
            }
        } else if events.iter().any(|event| {
            matches!(
                event,
                L1ProtocolEvent::Portal(
                    PortalEvent::BatchSubmitted(_)
                        | PortalEvent::WithdrawalProcessed(_)
                        | PortalEvent::WithdrawalBounceBack(_)
                        | PortalEvent::DepositBounceBack(_)
                        | PortalEvent::DepositBounceBackPending(_)
                )
            )
        }) {
            return Err(failure(
                AdapterFindingCode::Grammar,
                "direct-call event occurred outside its transaction envelope",
            ));
        }
        if events
            .iter()
            .any(|event| matches!(event, L1ProtocolEvent::FactoryZoneCreated(_)))
            && !matches!(
                events.as_slice(),
                [
                    L1ProtocolEvent::Portal(PortalEvent::TokenEnabled(_)),
                    L1ProtocolEvent::FactoryZoneCreated(_)
                ]
            )
        {
            return Err(failure(
                AdapterFindingCode::Grammar,
                "creation requires TokenEnabled followed by ZoneCreated",
            ));
        }
        if observation.block_hash() == portal_creation_block_hash
            && events
                .iter()
                .any(|event| matches!(event, L1ProtocolEvent::Portal(PortalEvent::TokenEnabled(_))))
            && !events
                .iter()
                .any(|event| matches!(event, L1ProtocolEvent::FactoryZoneCreated(_)))
        {
            return Err(failure(
                AdapterFindingCode::Grammar,
                "creation-block TokenEnabled must belong to the creation pair",
            ));
        }
        if observation.block_hash() != portal_creation_block_hash
            && events
                .iter()
                .any(|event| matches!(event, L1ProtocolEvent::FactoryZoneCreated(_)))
        {
            return Err(failure(
                AdapterFindingCode::Grammar,
                "ZoneCreated occurred outside the configured creation block",
            ));
        }
        for event in events {
            match event {
                L1ProtocolEvent::FactoryZoneCreated(Factory::ZoneCreated {
                    portal,
                    zoneId,
                    initialToken,
                    ..
                }) if observation.block_hash() == portal_creation_block_hash => {
                    let enabled = tx
                        .outcomes()
                        .iter()
                        .find_map(|x| match x.event() {
                            L1ProtocolEvent::Portal(PortalEvent::TokenEnabled(e)) => {
                                Some(token(e.token, &e.name, &e.symbol, &e.currency))
                            }
                            _ => None,
                        })
                        .ok_or_else(|| {
                            failure(AdapterFindingCode::Grammar, "creation missing TokenEnabled")
                        })?;
                    operations.push(ImportedOperation::Create {
                        identity: PortalIdentity {
                            portal: *portal,
                            zone_id: *zoneId,
                            initial_token: *initialToken,
                        },
                        initial_token: enabled,
                    });
                }
                L1ProtocolEvent::Portal(PortalEvent::TokenEnabled(e))
                    if observation.block_hash() != portal_creation_block_hash =>
                {
                    operations.push(ImportedOperation::EnableToken(token(
                        e.token,
                        &e.name,
                        &e.symbol,
                        &e.currency,
                    )))
                }
                L1ProtocolEvent::Portal(PortalEvent::BouncebackGasUpdated(e)) => {
                    operations.push(ImportedOperation::UpdateBouncebackGas(e.bouncebackGas))
                }
                L1ProtocolEvent::Portal(PortalEvent::DepositMade(e))
                    if tx
                        .direct_call()
                        .is_none_or(|call| call.as_process_withdrawals().is_none()) =>
                {
                    let ciphertext: [u8; 64] = e.ciphertext.as_ref().try_into().map_err(|_| {
                        failure(
                            AdapterFindingCode::Grammar,
                            "deposit ciphertext is not 64 bytes",
                        )
                    })?;
                    let d = OrdinaryDeposit {
                        token: e.token,
                        sender: e.sender,
                        amount: e.netAmount,
                        tempo_refund_recipient: e.tempoRefundRecipient,
                        key_index: e.keyIndex,
                        encrypted: crate::kernel::DepositPayload {
                            ephemeral_pubkey_x: e.ephemeralPubkeyX,
                            ephemeral_pubkey_y_parity: e.ephemeralPubkeyYParity,
                            ciphertext: FixedBytes::from(ciphertext),
                            nonce: e.nonce,
                            tag: e.tag,
                        },
                    };
                    operations.push(ImportedOperation::AppendDeposit(d));
                    effects.push(Effect::DepositAppended {
                        id: crate::kernel::DepositId {
                            portal: observation.portal_address(),
                            number: NonZeroU64::new(e.depositNumber).ok_or_else(|| {
                                failure(AdapterFindingCode::Grammar, "zero deposit number")
                            })?,
                        },
                        queue_hash: e.newCurrentDepositQueueHash,
                    });
                }
                L1ProtocolEvent::Portal(PortalEvent::RefundClaimed(e)) => {
                    operations.push(ImportedOperation::ClaimPortalRefund(RefundClaim {
                        token: e.token,
                        recipient: e.recipient,
                        amount: e.amount,
                    }));
                    effects.push(Effect::RefundClaimed {
                        token: e.token,
                        recipient: e.recipient,
                        amount: e.amount,
                    });
                }
                L1ProtocolEvent::Portal(PortalEvent::BatchSubmitted(e)) => {
                    effects.push(Effect::BatchSubmitted {
                        id: crate::kernel::BatchId {
                            zone_id,
                            index: NonZeroU64::new(e.withdrawalBatchIndex).ok_or_else(|| {
                                failure(AdapterFindingCode::Grammar, "zero batch index")
                            })?,
                        },
                        queue_index: e.withdrawalQueueIndex,
                        processed_deposit_hash: e.nextProcessedDepositQueueHash,
                        final_block_hash: e.nextBlockHash,
                        queue_hash: e.withdrawalQueueHash,
                        processed_deposit_number: e.lastProcessedDepositNumber,
                    })
                }
                L1ProtocolEvent::Portal(
                    PortalEvent::WithdrawalProcessed(_)
                    | PortalEvent::WithdrawalBounceBack(_)
                    | PortalEvent::DepositBounceBack(_)
                    | PortalEvent::DepositBounceBackPending(_),
                ) if tx
                    .direct_call()
                    .is_some_and(|call| call.as_process_withdrawals().is_some()) => {}
                L1ProtocolEvent::Portal(PortalEvent::DepositMade(_))
                    if tx
                        .direct_call()
                        .is_some_and(|call| call.as_process_withdrawals().is_some()) => {}
                L1ProtocolEvent::Portal(PortalEvent::TokenEnabled(_))
                    if observation.block_hash() == portal_creation_block_hash => {}
                L1ProtocolEvent::FactoryZoneCreated(_) => {}
                L1ProtocolEvent::KnownIgnored | L1ProtocolEvent::Portal(_) => {
                    return Err(failure(
                        AdapterFindingCode::Grammar,
                        "protocol event does not match the expected grammar",
                    ));
                }
            }
        }
    }
    Ok((
        ImportedFacts {
            block_hash: observation.block_hash(),
            block_number: observation.block_number(),
            operations,
        },
        effects,
    ))
}

fn parse_withdrawal_events(
    events: &[&L1ProtocolEvent],
    member_count: usize,
    portal: Address,
) -> Result<(Vec<WithdrawalOutcome>, Vec<Effect>), Failure> {
    let mut cursor = 0;
    let mut outcomes = Vec::with_capacity(member_count);
    let mut effects = Vec::new();
    for _ in 0..member_count {
        let event = events.get(cursor).ok_or_else(|| {
            failure(
                AdapterFindingCode::Grammar,
                "processWithdrawals missing member outcome",
            )
        })?;
        cursor += 1;
        match event {
            L1ProtocolEvent::Portal(PortalEvent::DepositBounceBack(e)) => {
                outcomes.push(WithdrawalOutcome::FailedDepositPaid {
                    collected_fee: e.bouncebackFee,
                });
                effects.push(Effect::FailedDepositRefunded {
                    recipient: e.tempoRefundRecipient,
                    token: e.token,
                    amount: e.amount,
                    fee: e.bouncebackFee,
                    pending: false,
                });
            }
            L1ProtocolEvent::Portal(PortalEvent::DepositBounceBackPending(e)) => {
                outcomes.push(WithdrawalOutcome::FailedDepositPending {
                    collected_fee: e.bouncebackFee,
                });
                effects.push(Effect::FailedDepositRefunded {
                    recipient: e.tempoRefundRecipient,
                    token: e.token,
                    amount: e.amount,
                    fee: e.bouncebackFee,
                    pending: true,
                });
            }
            L1ProtocolEvent::Portal(PortalEvent::WithdrawalBounceBack(e)) => {
                effects.push(Effect::BounceBackAppended {
                    fallback_nonce: e.fallbackNonce,
                    token: e.token,
                    amount: e.amount,
                    id: crate::kernel::DepositId {
                        portal,
                        number: NonZeroU64::new(e.depositNumber).ok_or_else(|| {
                            failure(
                                AdapterFindingCode::Grammar,
                                "zero bounceback deposit number",
                            )
                        })?,
                    },
                    queue_hash: e.newCurrentDepositQueueHash,
                });
                let Some(L1ProtocolEvent::Portal(PortalEvent::WithdrawalProcessed(processed))) =
                    events.get(cursor).copied()
                else {
                    return Err(failure(
                        AdapterFindingCode::Grammar,
                        "WithdrawalBounceBack must be followed by WithdrawalProcessed",
                    ));
                };
                cursor += 1;
                if processed.callbackSuccess {
                    return Err(failure(
                        AdapterFindingCode::Grammar,
                        "bounce WithdrawalProcessed callbackSuccess must be false",
                    ));
                }
                effects.push(Effect::UserWithdrawalProcessed {
                    to: processed.to,
                    sender_tag: processed.senderTag,
                    token: processed.token,
                    amount: processed.amount,
                    callback_success: false,
                });
                outcomes.push(WithdrawalOutcome::UserBounced);
            }
            L1ProtocolEvent::Portal(PortalEvent::DepositMade(first)) => {
                let mut callback_deposits = Vec::new();
                let mut next = Some(first.clone());
                while let Some(deposit) = next.take() {
                    let ciphertext: [u8; 64] =
                        deposit.ciphertext.as_ref().try_into().map_err(|_| {
                            failure(
                                AdapterFindingCode::Grammar,
                                "callback ciphertext is not 64 bytes",
                            )
                        })?;
                    callback_deposits.push(OrdinaryDeposit {
                        token: deposit.token,
                        sender: deposit.sender,
                        amount: deposit.netAmount,
                        tempo_refund_recipient: deposit.tempoRefundRecipient,
                        key_index: deposit.keyIndex,
                        encrypted: crate::kernel::DepositPayload {
                            ephemeral_pubkey_x: deposit.ephemeralPubkeyX,
                            ephemeral_pubkey_y_parity: deposit.ephemeralPubkeyYParity,
                            ciphertext: FixedBytes::from(ciphertext),
                            nonce: deposit.nonce,
                            tag: deposit.tag,
                        },
                    });
                    effects.push(Effect::DepositAppended {
                        id: crate::kernel::DepositId {
                            portal,
                            number: NonZeroU64::new(deposit.depositNumber).ok_or_else(|| {
                                failure(AdapterFindingCode::Grammar, "zero callback deposit number")
                            })?,
                        },
                        queue_hash: deposit.newCurrentDepositQueueHash,
                    });
                    if let Some(L1ProtocolEvent::Portal(PortalEvent::DepositMade(d))) =
                        events.get(cursor).copied()
                    {
                        cursor += 1;
                        next = Some(d.clone());
                    }
                }
                let Some(L1ProtocolEvent::Portal(PortalEvent::WithdrawalProcessed(processed))) =
                    events.get(cursor).copied()
                else {
                    return Err(failure(
                        AdapterFindingCode::Grammar,
                        "callback deposits must be followed by WithdrawalProcessed",
                    ));
                };
                cursor += 1;
                if !processed.callbackSuccess {
                    return Err(failure(
                        AdapterFindingCode::Grammar,
                        "callback WithdrawalProcessed callbackSuccess must be true",
                    ));
                }
                effects.push(Effect::UserWithdrawalProcessed {
                    to: processed.to,
                    sender_tag: processed.senderTag,
                    token: processed.token,
                    amount: processed.amount,
                    callback_success: true,
                });
                outcomes.push(WithdrawalOutcome::UserDelivered { callback_deposits });
            }
            L1ProtocolEvent::Portal(PortalEvent::WithdrawalProcessed(processed)) => {
                if !processed.callbackSuccess {
                    return Err(failure(
                        AdapterFindingCode::Grammar,
                        "delivered WithdrawalProcessed callbackSuccess must be true",
                    ));
                }
                effects.push(Effect::UserWithdrawalProcessed {
                    to: processed.to,
                    sender_tag: processed.senderTag,
                    token: processed.token,
                    amount: processed.amount,
                    callback_success: true,
                });
                outcomes.push(WithdrawalOutcome::UserDelivered {
                    callback_deposits: vec![],
                });
            }
            _ => {
                return Err(failure(
                    AdapterFindingCode::Grammar,
                    "unexpected processWithdrawals member event",
                ));
            }
        }
    }
    if cursor != events.len() {
        return Err(failure(
            AdapterFindingCode::Grammar,
            "processWithdrawals has extra or out-of-order events",
        ));
    }
    Ok((outcomes, effects))
}

fn zone_facts(o: &AuthenticatedObservation) -> Result<(ZoneFacts, Vec<Effect>), Failure> {
    let advance = o.l2.inputs().advance_tempo();
    let advance_hash = o.l2.inputs().advance_transaction_hash();
    validate_zone_event_grammar(o)?;
    let enabled_tokens = advance
        .enabled_tokens()
        .iter()
        .map(|e| token(e.token, &e.name, &e.symbol, &e.currency))
        .collect();
    let mut deposits = Vec::new();
    for d in advance.deposits() {
        if let Some(d) = d.as_ordinary() {
            deposits.push(Deposit::Ordinary(ordinary(d)?));
        } else if let Some(d) = d.as_withdrawal_bounce_back() {
            let bytes = d.to.as_slice();
            if bytes[..12].iter().any(|byte| *byte != 0) {
                return Err(failure(
                    AdapterFindingCode::Grammar,
                    "bounceback recipient has non-canonical high bytes",
                ));
            }
            if d.amount == 0 {
                return Err(failure(
                    AdapterFindingCode::Grammar,
                    "zero bounceback amount",
                ));
            }
            let nonce = NonZeroU64::new(u64::from_be_bytes(bytes[12..].try_into().unwrap()))
                .ok_or_else(|| failure(AdapterFindingCode::Grammar, "zero bounceback nonce"))?;
            deposits.push(Deposit::BounceBack(BounceBackDeposit {
                token: d.token,
                fallback_nonce: nonce,
                amount: d.amount,
            }));
        }
    }
    let mut outcomes = Vec::new();
    let mut operations = Vec::new();
    let mut effects = Vec::new();
    for outcome in o.l2.outcomes().events() {
        match outcome.event() {
            L2ProtocolEvent::Inbox(Inbox::InboxEvents::TokenEnabled(e)) => {
                effects.push(Effect::TokenEnabled {
                    token: e.token,
                    name: e.name.clone(),
                    symbol: e.symbol.clone(),
                    currency: e.currency.clone(),
                })
            }
            L2ProtocolEvent::Inbox(Inbox::InboxEvents::DepositProcessed(e)) => {
                outcomes.push(DepositOutcome::Minted);
                effects.push(Effect::DepositProcessed {
                    deposit_hash: e.depositHash,
                    sender: e.sender,
                    token: e.token,
                    amount: e.amount,
                });
            }
            L2ProtocolEvent::Inbox(Inbox::InboxEvents::DepositFailed(e)) => {
                outcomes.push(DepositOutcome::Failed);
                effects.push(Effect::DepositFailed {
                    deposit_hash: e.depositHash,
                    sender: e.sender,
                    token: e.token,
                    amount: e.amount,
                });
            }
            L2ProtocolEvent::Inbox(Inbox::InboxEvents::WithdrawalBounceBackProcessed(e)) => {
                outcomes.push(DepositOutcome::BounceBackMinted {
                    recipient: e.zoneFallbackRecipient,
                });
                effects.push(Effect::BounceBackMinted {
                    token: e.token,
                    amount: e.amount,
                });
            }
            L2ProtocolEvent::Inbox(Inbox::InboxEvents::WithdrawalBounceBackPending(e)) => {
                outcomes.push(DepositOutcome::BounceBackPending {
                    recipient: e.zoneFallbackRecipient,
                });
                effects.push(Effect::BounceBackPending {
                    token: e.token,
                    amount: e.amount,
                });
            }
            L2ProtocolEvent::Outbox(Outbox::OutboxEvents::TempoGasRateUpdated(e)) => {
                operations.push(ZoneOperation::UpdateTempoGasRate(e.tempoGasRate))
            }
            L2ProtocolEvent::Outbox(Outbox::OutboxEvents::MaxWithdrawalsPerBlockUpdated(e)) => {
                operations.push(ZoneOperation::UpdateMaxWithdrawals(
                    e.maxWithdrawalsPerBlock,
                ))
            }
            L2ProtocolEvent::Outbox(Outbox::OutboxEvents::WithdrawalRequested(e)) => {
                let sender = outcome.position().transaction_sender();
                if outcome.position().transaction_hash() != advance_hash {
                    operations.push(ZoneOperation::AcceptWithdrawal(UserWithdrawal {
                        sender,
                        transaction_hash: outcome.position().transaction_hash(),
                        token: e.token,
                        to: e.to,
                        amount: e.amount,
                        memo: e.memo,
                        gas_limit: e.gasLimit,
                        callback_data: e.data.clone(),
                        reveal_to: e.revealTo.clone(),
                    }));
                }
                effects.push(Effect::WithdrawalRequested {
                    id: crate::kernel::WithdrawalId {
                        zone_id: o.zone_id,
                        index: e.withdrawalIndex,
                    },
                    sender,
                    token: e.token,
                    to: e.to,
                    amount: e.amount,
                    fee: e.fee,
                    memo: e.memo,
                    gas_limit: e.gasLimit,
                    fallback_nonce: e.fallbackNonce,
                    callback_data: e.data.clone(),
                    reveal_to: e.revealTo.clone(),
                });
            }
            L2ProtocolEvent::Inbox(Inbox::InboxEvents::RefundClaimed(e)) => {
                operations.push(ZoneOperation::ClaimInboxRefund(RefundClaim {
                    token: e.token,
                    recipient: e.recipient,
                    amount: e.amount,
                }));
                effects.push(Effect::RefundClaimed {
                    token: e.token,
                    recipient: e.recipient,
                    amount: e.amount,
                });
            }
            L2ProtocolEvent::Outbox(Outbox::OutboxEvents::BatchFinalized(e)) => {
                effects.push(Effect::BatchFinalized {
                    id: crate::kernel::BatchId {
                        zone_id: o.zone_id,
                        index: NonZeroU64::new(e.withdrawalBatchIndex).ok_or_else(|| {
                            failure(AdapterFindingCode::Grammar, "zero finalized batch index")
                        })?,
                    },
                    queue_hash: e.withdrawalQueueHash,
                })
            }
            L2ProtocolEvent::Inbox(Inbox::InboxEvents::TempoAdvanced(_))
            | L2ProtocolEvent::TempoState(_) => {}
        }
    }
    let finalization = o.l2.inputs().finalization().map(|f| Finalization {
        block_number: f.input().block_number(),
        declared_count: f.input().count(),
        encrypted_senders: f.input().encrypted_senders().to_vec(),
    });
    Ok((
        ZoneFacts {
            block_hash: o.l2.block_hash(),
            block_number: o.l2.block_number(),
            enabled_tokens,
            deposits,
            outcomes,
            operations,
            finalization,
        },
        effects,
    ))
}

/// Validate event ownership and order with one transaction-scoped cursor.
fn validate_zone_event_grammar(o: &AuthenticatedObservation) -> Result<(), Failure> {
    let events = o.l2.outcomes().events();
    let advance = o.l2.inputs().advance_tempo();
    let advance_hash = o.l2.inputs().advance_transaction_hash();
    let mut cursor = 0usize;
    let imported = advance.final_imported_header();
    match events.first().map(|outcome| outcome.event()) {
        Some(L2ProtocolEvent::TempoState(TempoState::TempoStateEvents::TempoBlockFinalized(
            event,
        ))) if event.blockHash == imported.hash()
            && event.blockNumber == imported.number()
            && event.stateRoot == imported.header().state_root() => {}
        _ => {
            return Err(failure(
                AdapterFindingCode::Grammar,
                "TempoBlockFinalized fields do not match the imported header",
            ));
        }
    }
    macro_rules! next_advance {
        ($expected:expr, $label:literal $(,)?) => {{
            let outcome = events.get(cursor).ok_or_else(|| {
                failure(
                    AdapterFindingCode::Grammar,
                    concat!("advance missing ", $label),
                )
            })?;
            if outcome.position().transaction_index() != 0
                || outcome.position().transaction_hash() != advance_hash
                || !$expected(outcome.event())
            {
                return Err(failure(
                    AdapterFindingCode::Grammar,
                    format!("advance expected {} at cursor {cursor}", $label),
                ));
            }
            cursor += 1;
        }};
    }
    next_advance!(
        |e: &L2ProtocolEvent| {
            matches!(
                e,
                L2ProtocolEvent::TempoState(TempoState::TempoStateEvents::TempoBlockFinalized(_))
            )
        },
        "TempoBlockFinalized",
    );
    for _ in advance.enabled_tokens() {
        next_advance!(
            |e: &L2ProtocolEvent| {
                matches!(
                    e,
                    L2ProtocolEvent::Inbox(Inbox::InboxEvents::TokenEnabled(_))
                )
            },
            "TokenEnabled",
        );
    }
    for (index, deposit) in advance.deposits().iter().enumerate() {
        if deposit.as_ordinary().is_some() {
            let outcome = events.get(cursor).ok_or_else(|| {
                failure(
                    AdapterFindingCode::Grammar,
                    format!("deposit {index} missing outcome"),
                )
            })?;
            if outcome.position().transaction_index() != 0
                || outcome.position().transaction_hash() != advance_hash
            {
                return Err(failure(
                    AdapterFindingCode::Grammar,
                    format!("deposit {index} outcome belongs to wrong transaction"),
                ));
            }
            cursor += 1;
            match outcome.event() {
                L2ProtocolEvent::Inbox(Inbox::InboxEvents::DepositProcessed(_)) => {}
                L2ProtocolEvent::Outbox(Outbox::OutboxEvents::WithdrawalRequested(_)) => {
                    next_advance!(
                        |e: &L2ProtocolEvent| {
                            matches!(
                                e,
                                L2ProtocolEvent::Inbox(Inbox::InboxEvents::DepositFailed(_))
                            )
                        },
                        "DepositFailed",
                    );
                }
                _ => {
                    return Err(failure(
                        AdapterFindingCode::Grammar,
                        format!("deposit {index} has invalid outcome grammar"),
                    ));
                }
            }
        } else if deposit.as_withdrawal_bounce_back().is_some() {
            next_advance!(
                |e: &L2ProtocolEvent| {
                    matches!(
                        e,
                        L2ProtocolEvent::Inbox(
                            Inbox::InboxEvents::WithdrawalBounceBackProcessed(_)
                                | Inbox::InboxEvents::WithdrawalBounceBackPending(_)
                        )
                    )
                },
                "bounceback outcome",
            );
        } else {
            return Err(failure(
                AdapterFindingCode::Grammar,
                format!("deposit {index} has unsupported kind"),
            ));
        }
    }
    match events.get(cursor).map(|outcome| outcome.event()) {
        Some(L2ProtocolEvent::Inbox(Inbox::InboxEvents::TempoAdvanced(event)))
            if event.tempoBlockHash == o.state.tempo_block_hash()
                && event.tempoBlockNumber == o.state.tempo_block_number()
                && event.newProcessedDepositQueueHash == o.state.processed_deposit_queue_hash()
                && event.lastProcessedDepositNumber == o.state.processed_deposit_number()
                && event.depositsProcessed
                    == u64::try_from(advance.deposits().len()).unwrap_or(u64::MAX) => {}
        _ => {
            return Err(failure(
                AdapterFindingCode::Grammar,
                "TempoAdvanced fields do not match advanceTempo input or Zone state",
            ));
        }
    }
    next_advance!(
        |e: &L2ProtocolEvent| {
            matches!(
                e,
                L2ProtocolEvent::Inbox(Inbox::InboxEvents::TempoAdvanced(_))
            )
        },
        "TempoAdvanced",
    );
    if events
        .get(cursor)
        .is_some_and(|e| e.position().transaction_index() == 0)
    {
        return Err(failure(
            AdapterFindingCode::Grammar,
            "extra event in advance transaction",
        ));
    }

    let final_hash = o.l2.inputs().finalization().map(|f| f.transaction_hash());
    while let Some(outcome) = events.get(cursor) {
        if final_hash.is_some_and(|hash| outcome.position().transaction_hash() == hash) {
            break;
        }
        if !matches!(
            outcome.event(),
            L2ProtocolEvent::Outbox(
                Outbox::OutboxEvents::TempoGasRateUpdated(_)
                    | Outbox::OutboxEvents::MaxWithdrawalsPerBlockUpdated(_)
                    | Outbox::OutboxEvents::WithdrawalRequested(_)
            ) | L2ProtocolEvent::Inbox(Inbox::InboxEvents::RefundClaimed(_))
        ) {
            return Err(failure(
                AdapterFindingCode::Grammar,
                "unexpected post-advance protocol event",
            ));
        }
        cursor += 1;
    }
    match final_hash {
        Some(hash) => {
            let outcome = events.get(cursor).ok_or_else(|| {
                failure(
                    AdapterFindingCode::Grammar,
                    "finalization missing BatchFinalized",
                )
            })?;
            if outcome.position().transaction_hash() != hash
                || !matches!(
                    outcome.event(),
                    L2ProtocolEvent::Outbox(Outbox::OutboxEvents::BatchFinalized(_))
                )
            {
                return Err(failure(
                    AdapterFindingCode::Grammar,
                    "finalization does not own BatchFinalized",
                ));
            }
            cursor += 1;
        }
        None if events.get(cursor).is_some_and(|o| {
            matches!(
                o.event(),
                L2ProtocolEvent::Outbox(Outbox::OutboxEvents::BatchFinalized(_))
            )
        }) =>
        {
            return Err(failure(
                AdapterFindingCode::Grammar,
                "BatchFinalized has no finalization envelope",
            ));
        }
        None => {}
    }
    if cursor != events.len() {
        return Err(failure(
            AdapterFindingCode::Grammar,
            "extra finalization or protocol events",
        ));
    }
    Ok(())
}
