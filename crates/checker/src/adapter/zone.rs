//! Parses Zone inputs and validates their authenticated event sequence.

use alloy_consensus::BlockHeader as _;
use alloy_primitives::B256;

use crate::{
    failure::Failure,
    kernel::{
        Deposit, DepositOutcome, Effect, Finalization, RefundClaim, TokenEnable, UserWithdrawal,
        ZoneFacts, ZoneOperation,
    },
    observe::{
        OrderedL2Outcome,
        events::{Inbox, L2ProtocolEvent, Outbox, TempoState},
    },
};

use super::{AdapterFindingCode, AuthenticatedObservation, ZoneFactsAndEffects};

/// Kernel outputs derived from authenticated Zone events.
struct ZoneOutputs {
    outcomes: Vec<DepositOutcome>,
    operations: Vec<ZoneOperation>,
    effects: Vec<Effect>,
    finalization: Option<Finalization>,
}

impl ZoneOutputs {
    /// Record one deposit outcome alongside its predicted effect.
    fn push(&mut self, outcome: DepositOutcome, effect: Effect) {
        self.outcomes.push(outcome);
        self.effects.push(effect);
    }
}

/// Parse Zone calldata and its validated event sequence into kernel facts and effects.
pub(super) fn adapt(
    observation: &AuthenticatedObservation,
) -> Result<ZoneFactsAndEffects, Failure> {
    let advance_hash = observation.l2.inputs().advance_transaction_hash();
    validate_zone_event_sequence(observation)?;
    let (enabled_tokens, deposits) = adapt_deposits(observation)?;
    let ZoneOutputs {
        outcomes,
        operations,
        effects,
        finalization,
    } = adapt_outcomes(observation, advance_hash)?;
    Ok(ZoneFactsAndEffects {
        facts: ZoneFacts {
            block_hash: observation.l2.block_hash(),
            block_number: observation.l2.block_number(),
            enabled_tokens,
            deposits,
            outcomes,
            operations,
            finalization,
        },
        effects,
    })
}

/// Validate Zone event ownership and order with one transaction-scoped cursor.
fn validate_zone_event_sequence(observation: &AuthenticatedObservation) -> Result<(), Failure> {
    let events = observation.l2.outcomes().events();
    let advance = observation.l2.inputs().advance_tempo();
    let advance_hash = observation.l2.inputs().advance_transaction_hash();
    let mut cursor = 0usize;
    let imported = advance.imported_header();
    match events.first().map(|outcome| outcome.event()) {
        Some(L2ProtocolEvent::TempoState(TempoState::TempoStateEvents::TempoBlockFinalized(
            event,
        ))) if event.blockHash == imported.hash()
            && event.blockNumber == imported.number()
            && event.stateRoot == imported.header().state_root() => {}
        _ => {
            return Err(AdapterFindingCode::EventSequence
                .failure("TempoBlockFinalized fields do not match the imported header"));
        }
    }
    expect_advance(
        events,
        &mut cursor,
        advance_hash,
        |e: &L2ProtocolEvent| {
            matches!(
                e,
                L2ProtocolEvent::TempoState(TempoState::TempoStateEvents::TempoBlockFinalized(_))
            )
        },
        "TempoBlockFinalized",
    )?;
    for _ in advance.enabled_tokens() {
        expect_advance(
            events,
            &mut cursor,
            advance_hash,
            |e: &L2ProtocolEvent| {
                matches!(
                    e,
                    L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::TokenEnabled(_))
                )
            },
            "TokenEnabled",
        )?;
    }
    for (index, deposit) in advance.deposits().iter().enumerate() {
        if deposit.as_ordinary().is_some() {
            let outcome = events.get(cursor).ok_or_else(|| {
                AdapterFindingCode::EventSequence
                    .failure(format!("deposit {index} missing outcome"))
            })?;
            if !belongs_to_advance(outcome, advance_hash) {
                return Err(AdapterFindingCode::EventSequence.failure(format!(
                    "deposit {index} outcome belongs to wrong transaction"
                )));
            }
            cursor += 1;
            match outcome.event() {
                L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::DepositProcessed(_)) => {}
                L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::WithdrawalRequested(_)) => {
                    expect_advance(
                        events,
                        &mut cursor,
                        advance_hash,
                        |e: &L2ProtocolEvent| {
                            matches!(
                                e,
                                L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::DepositFailed(_))
                            )
                        },
                        "DepositFailed",
                    )?;
                }
                _ => {
                    return Err(AdapterFindingCode::EventSequence.failure(format!(
                        "deposit {index} has an unexpected outcome sequence"
                    )));
                }
            }
        } else if deposit.as_withdrawal_bounce_back().is_some() {
            expect_advance(
                events,
                &mut cursor,
                advance_hash,
                |e: &L2ProtocolEvent| {
                    matches!(
                        e,
                        L2ProtocolEvent::Inbox(
                            Inbox::IZoneInboxEvents::WithdrawalBounceBackProcessed(_)
                                | Inbox::IZoneInboxEvents::WithdrawalBounceBackPending(_)
                        )
                    )
                },
                "bounceback outcome",
            )?;
        } else {
            return Err(AdapterFindingCode::EventSequence
                .failure(format!("deposit {index} has unsupported kind")));
        }
    }
    match events.get(cursor).map(|outcome| outcome.event()) {
        Some(L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::TempoAdvanced(event)))
            if event.tempoBlockHash == observation.state.tempo_block_hash
                && event.tempoBlockNumber == observation.state.tempo_block_number
                && event.newProcessedDepositQueueHash
                    == observation.state.processed_deposit_queue_hash
                && event.lastProcessedDepositNumber
                    == observation.state.processed_deposit_number
                && event.depositsProcessed
                    == u64::try_from(advance.deposits().len()).unwrap_or(u64::MAX) => {}
        _ => {
            return Err(AdapterFindingCode::EventSequence
                .failure("TempoAdvanced fields do not match advanceTempo input or Zone state"));
        }
    }
    expect_advance(
        events,
        &mut cursor,
        advance_hash,
        |e: &L2ProtocolEvent| {
            matches!(
                e,
                L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::TempoAdvanced(_))
            )
        },
        "TempoAdvanced",
    )?;
    if events
        .get(cursor)
        .is_some_and(|e| e.position().transaction_index() == 0)
    {
        return Err(AdapterFindingCode::EventSequence.failure("extra event in advance transaction"));
    }

    let final_hash = observation
        .l2
        .inputs()
        .finalization()
        .map(|f| f.transaction_hash());
    while let Some(outcome) = events.get(cursor) {
        if final_hash.is_some_and(|hash| outcome.position().transaction_hash() == hash) {
            break;
        }
        if !matches!(
            outcome.event(),
            L2ProtocolEvent::Outbox(
                Outbox::IZoneOutboxEvents::TempoGasRateUpdated(_)
                    | Outbox::IZoneOutboxEvents::MaxWithdrawalsPerBlockUpdated(_)
                    | Outbox::IZoneOutboxEvents::WithdrawalRequested(_)
            ) | L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::RefundClaimed(_))
        ) {
            return Err(
                AdapterFindingCode::EventSequence.failure("unexpected post-advance protocol event")
            );
        }
        cursor += 1;
    }
    match final_hash {
        Some(hash) => {
            let outcome = events.get(cursor).ok_or_else(|| {
                AdapterFindingCode::EventSequence.failure("finalization missing BatchFinalized")
            })?;
            if outcome.position().transaction_hash() != hash
                || !matches!(
                    outcome.event(),
                    L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::BatchFinalized(_))
                )
            {
                return Err(AdapterFindingCode::EventSequence
                    .failure("finalization does not own BatchFinalized"));
            }
            cursor += 1;
        }
        None if events.get(cursor).is_some_and(|outcome| {
            matches!(
                outcome.event(),
                L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::BatchFinalized(_))
            )
        }) =>
        {
            return Err(AdapterFindingCode::EventSequence
                .failure("BatchFinalized has no finalization envelope"));
        }
        None => {}
    }
    if cursor != events.len() {
        return Err(
            AdapterFindingCode::EventSequence.failure("extra finalization or protocol events")
        );
    }
    Ok(())
}

/// Whether an authenticated L2 outcome belongs to the `advanceTempo` transaction.
fn belongs_to_advance(outcome: &OrderedL2Outcome, advance_hash: B256) -> bool {
    outcome.position().transaction_index() == 0
        && outcome.position().transaction_hash() == advance_hash
}

/// Consume one protocol event owned by the `advanceTempo` transaction.
fn expect_advance(
    events: &[OrderedL2Outcome],
    cursor: &mut usize,
    advance_hash: B256,
    expected: impl FnOnce(&L2ProtocolEvent) -> bool,
    label: &str,
) -> Result<(), Failure> {
    let outcome = events.get(*cursor).ok_or_else(|| {
        AdapterFindingCode::EventSequence.failure(format!("advance missing {label}"))
    })?;
    if !belongs_to_advance(outcome, advance_hash) || !expected(outcome.event()) {
        return Err(AdapterFindingCode::EventSequence
            .failure(format!("advance expected {label} at cursor {cursor}")));
    }
    *cursor += 1;
    Ok(())
}
/// Adapt the deposits carried by an authenticated `advanceTempo` input.
fn adapt_deposits(
    observation: &AuthenticatedObservation,
) -> Result<(Vec<TokenEnable>, Vec<Deposit>), Failure> {
    let advance = observation.l2.inputs().advance_tempo();
    let enabled_tokens = advance
        .enabled_tokens()
        .iter()
        .map(TokenEnable::from)
        .collect();
    let mut deposits = Vec::new();
    for d in advance.deposits() {
        if let Some(d) = d.as_ordinary() {
            deposits.push(Deposit::Ordinary(d.try_into()?));
        } else if let Some(d) = d.as_withdrawal_bounce_back() {
            deposits.push(Deposit::BounceBack(d.try_into()?));
        } else {
            return Err(AdapterFindingCode::EventSequence.failure("unsupported deposit kind"));
        }
    }
    Ok((enabled_tokens, deposits))
}

/// Adapt authenticated Zone event outputs into kernel facts and effects.
fn adapt_outcomes(
    observation: &AuthenticatedObservation,
    advance_hash: B256,
) -> Result<ZoneOutputs, Failure> {
    let mut acc = ZoneOutputs {
        outcomes: Vec::new(),
        operations: Vec::new(),
        effects: Vec::new(),
        finalization: None,
    };
    for outcome in observation.l2.outcomes().events() {
        match outcome.event() {
            L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::TokenEnabled(e)) => {
                acc.effects.push(Effect::from(e))
            }
            L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::DepositProcessed(e)) => {
                acc.push(DepositOutcome::Minted, Effect::from(e));
            }
            L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::DepositFailed(e)) => {
                acc.push(DepositOutcome::Failed, Effect::from(e));
            }
            L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::WithdrawalBounceBackProcessed(e)) => {
                acc.push(DepositOutcome::from(e), Effect::from(e));
            }
            L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::WithdrawalBounceBackPending(e)) => {
                acc.push(DepositOutcome::from(e), Effect::from(e));
            }
            L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::TempoGasRateUpdated(e)) => acc
                .operations
                .push(ZoneOperation::UpdateTempoGasRate(e.tempoGasRate)),
            L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::MaxWithdrawalsPerBlockUpdated(
                e,
            )) => acc.operations.push(ZoneOperation::UpdateMaxWithdrawals(
                e.maxWithdrawalsPerBlock,
            )),
            L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::WithdrawalRequested(e)) => {
                let (operation, effect) = adapt_withdrawal(
                    e,
                    outcome.position().transaction_hash(),
                    advance_hash,
                    observation.zone_id,
                );
                if let Some(operation) = operation {
                    acc.operations.push(operation);
                }
                acc.effects.push(effect);
            }
            L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::RefundClaimed(e)) => {
                acc.operations
                    .push(ZoneOperation::ClaimInboxRefund(RefundClaim::from(e)));
                acc.effects.push(Effect::from(e));
            }
            L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::BatchFinalized(e)) => {
                acc.effects.push(Effect::BatchFinalized {
                    id: crate::kernel::BatchId::new(observation.zone_id, e.withdrawalBatchIndex)
                        .ok_or_else(|| {
                            AdapterFindingCode::EventSequence.failure("zero finalized batch index")
                        })?,
                    queue_hash: e.withdrawalQueueHash,
                })
            }
            L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::TempoAdvanced(_))
            | L2ProtocolEvent::TempoState(_) => {}
            L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::DepositRejected(_)) => {
                return Err(AdapterFindingCode::EventSequence
                    .failure("unsupported DepositRejected event passed classification"));
            }
        }
    }
    acc.finalization = observation
        .l2
        .inputs()
        .finalization()
        .map(|f| Finalization {
            block_number: f.input().block_number(),
            declared_count: f.input().count(),
            encrypted_senders: f.input().encrypted_senders().to_vec(),
        });
    Ok(acc)
}

/// Adapt a withdrawal using the immediate caller authenticated by its event.
fn adapt_withdrawal(
    event: &Outbox::WithdrawalRequested,
    transaction_hash: B256,
    advance_hash: B256,
    zone_id: u32,
) -> (Option<ZoneOperation>, Effect) {
    let operation = (transaction_hash != advance_hash).then(|| {
        ZoneOperation::AcceptWithdrawal(UserWithdrawal {
            sender: event.sender,
            transaction_hash,
            token: event.token,
            to: event.to,
            amount: event.amount,
            memo: event.memo,
            gas_limit: event.gasLimit,
            callback_data: event.data.clone(),
            reveal_to: event.revealTo.clone(),
        })
    });
    let effect = Effect::WithdrawalRequested {
        id: crate::kernel::WithdrawalId {
            zone_id,
            index: event.withdrawalIndex,
        },
        sender: event.sender,
        token: event.token,
        to: event.to,
        amount: event.amount,
        fee: event.fee,
        memo: event.memo,
        gas_limit: event.gasLimit,
        fallback_nonce: event.fallbackNonce,
        callback_data: event.data.clone(),
        reveal_to: event.revealTo.clone(),
    };
    (operation, effect)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, Bytes};

    use super::*;

    #[test]
    fn contract_withdrawal_uses_the_authenticated_event_sender() {
        let transaction_signer = Address::repeat_byte(0x44);
        let contract_sender = Address::repeat_byte(0x45);
        let transaction_hash = B256::repeat_byte(0x46);
        let event = Outbox::WithdrawalRequested {
            withdrawalIndex: 4,
            sender: contract_sender,
            token: Address::repeat_byte(0x55),
            to: Address::repeat_byte(0x66),
            amount: 100,
            fee: 9,
            memo: B256::repeat_byte(0x77),
            gasLimit: 8,
            fallbackNonce: 3,
            data: Bytes::from_static(b"callback"),
            revealTo: Bytes::new(),
        };

        assert_ne!(transaction_signer, event.sender);
        let (operation, effect) = adapt_withdrawal(&event, transaction_hash, B256::ZERO, 7);
        let Some(ZoneOperation::AcceptWithdrawal(withdrawal)) = operation else {
            panic!("expected accepted withdrawal")
        };
        assert_eq!(withdrawal.sender, contract_sender);
        assert_eq!(withdrawal.transaction_hash, transaction_hash);
        assert!(matches!(
            effect,
            Effect::WithdrawalRequested { sender, .. } if sender == contract_sender
        ));
    }
}
