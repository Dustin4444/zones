//! Parses authenticated Tempo transaction envelopes into checker facts.

use alloy_consensus::BlockHeader as _;
use alloy_primitives::{Address, B256, U256};

use crate::{
    failure::Failure,
    kernel::{
        BatchSubmission, Effect, ImportedFacts, ImportedOperation, PortalCallbackOperation,
        PortalIdentity, RefundClaim, TokenEnable, Withdrawal, WithdrawalOutcome,
        WithdrawalProcessing,
    },
    observe::{
        L1BlockObservation, L1TransactionObservation,
        events::{Factory, L1ProtocolEvent, Portal},
    },
};

use super::{AdapterFindingCode, ImportedFactsAndEffects};

/// The Zone-creation event's position and preceding `TokenEnabled`, if this
/// transaction's events contain exactly one `FactoryZoneCreated`.
struct CreationContext {
    event_index: usize,
    initial_token: TokenEnable,
}

/// Parsed effects and outcomes for one `processWithdrawals` call.
struct WithdrawalOutcomesAndEffects {
    outcomes: Vec<WithdrawalOutcome>,
    effects: Vec<Effect>,
}

impl WithdrawalOutcomesAndEffects {
    /// Record one withdrawal outcome alongside its predicted effect.
    fn push(&mut self, outcome: WithdrawalOutcome, effect: Effect) {
        self.outcomes.push(outcome);
        self.effects.push(effect);
    }
}

/// Parse imported transaction envelopes into ordered kernel facts and effects.
pub(super) fn adapt(
    observation: &L1BlockObservation,
    header: &crate::observe::ImportedTempoHeader,
    portal_creation_block_hash: B256,
    zone_id: u32,
) -> Result<ImportedFactsAndEffects, Failure> {
    let mut operations = Vec::new();
    let mut effects = Vec::new();
    for tx in observation.protocol_transactions() {
        let all_events: Vec<_> = tx.outcomes().iter().map(|x| x.event()).collect();
        let event_cursor = dispatch_direct_calls(
            tx,
            &all_events,
            observation,
            header,
            zone_id,
            &mut operations,
            &mut effects,
        )?;
        let is_creation_block = observation.block_hash() == portal_creation_block_hash;
        dispatch_remaining_events(
            &all_events[event_cursor..],
            is_creation_block,
            tx.direct_calls().is_empty(),
            observation,
            &mut operations,
            &mut effects,
        )?;
    }
    Ok(ImportedFactsAndEffects {
        facts: ImportedFacts {
            block_hash: observation.block_hash(),
            block_number: observation.block_number(),
            operations,
        },
        effects,
    })
}

/// Dispatch one transaction's direct Portal calls, consuming their matching events.
fn dispatch_direct_calls(
    tx: &L1TransactionObservation,
    all_events: &[&L1ProtocolEvent],
    observation: &L1BlockObservation,
    header: &crate::observe::ImportedTempoHeader,
    zone_id: u32,
    operations: &mut Vec<ImportedOperation>,
    effects: &mut Vec<Effect>,
) -> Result<usize, Failure> {
    let mut event_cursor = 0;
    for direct_call in tx.direct_calls() {
        if let Some(call) = direct_call.as_set_bounceback_gas() {
            let event = next_event(
                all_events,
                &mut event_cursor,
                "setBouncebackGas is not followed by BouncebackGasUpdated",
            )?;
            let L1ProtocolEvent::Portal(Portal::ZonePortalEvents::BouncebackGasUpdated(event)) =
                event
            else {
                return Err(AdapterFindingCode::EventSequence
                    .failure("setBouncebackGas is not followed by BouncebackGasUpdated"));
            };
            if event.bouncebackGas != call.newBouncebackGas {
                return Err(AdapterFindingCode::EventSequence
                    .failure("setBouncebackGas result differs from calldata"));
            }
            operations.push(ImportedOperation::UpdateBouncebackGas(
                call.newBouncebackGas,
            ));
            continue;
        }
        if let Some(call) = direct_call.as_enable_token() {
            let event = next_event(
                all_events,
                &mut event_cursor,
                "enableToken is not followed by TokenEnabled",
            )?;
            let L1ProtocolEvent::Portal(Portal::ZonePortalEvents::TokenEnabled(event)) = event
            else {
                return Err(AdapterFindingCode::EventSequence
                    .failure("enableToken is not followed by TokenEnabled"));
            };
            if event.token != call.token {
                return Err(AdapterFindingCode::EventSequence
                    .failure("enableToken result differs from calldata"));
            }
            operations.push(ImportedOperation::EnableToken(TokenEnable::from(event)));
            continue;
        }
        if direct_call.is_deposit() {
            let event = next_event(
                all_events,
                &mut event_cursor,
                "deposit is not followed by DepositMade",
            )?;
            let L1ProtocolEvent::Portal(Portal::ZonePortalEvents::DepositMade(event)) = event
            else {
                return Err(AdapterFindingCode::EventSequence
                    .failure("deposit is not followed by DepositMade"));
            };
            push_deposit_appended(operations, effects, observation.portal_address(), event)?;
            continue;
        }
        if direct_call.is_claim_refund() {
            let event = next_event(
                all_events,
                &mut event_cursor,
                "claimRefund is not followed by RefundClaimed",
            )?;
            let L1ProtocolEvent::Portal(Portal::ZonePortalEvents::RefundClaimed(event)) = event
            else {
                return Err(AdapterFindingCode::EventSequence
                    .failure("claimRefund is not followed by RefundClaimed"));
            };
            push_refund_claim(operations, effects, event);
            continue;
        }
        if let Some(call) = direct_call.as_submit_batch() {
            let event = next_event(
                all_events,
                &mut event_cursor,
                "submitBatch is not followed by BatchSubmitted",
            )?;
            let L1ProtocolEvent::Portal(Portal::ZonePortalEvents::BatchSubmitted(event)) = event
            else {
                return Err(AdapterFindingCode::EventSequence
                    .failure("submitBatch is not followed by BatchSubmitted"));
            };
            operations.push(ImportedOperation::SubmitBatch(BatchSubmission::from(call)));
            effects.push(Effect::BatchSubmitted {
                id: crate::kernel::BatchId::new(zone_id, event.withdrawalBatchIndex)
                    .ok_or_else(|| AdapterFindingCode::EventSequence.failure("zero batch index"))?,
                queue_index: event.withdrawalQueueIndex,
                processed_deposit_hash: event.nextProcessedDepositQueueHash,
                final_block_hash: event.nextBlockHash,
                queue_hash: event.withdrawalQueueHash,
                processed_deposit_number: event.lastProcessedDepositNumber,
            });
            continue;
        }
        if let Some(call) = direct_call.as_process_withdrawals() {
            let withdrawals = call.withdrawals.iter().map(Withdrawal::from).collect();
            let (
                WithdrawalOutcomesAndEffects {
                    outcomes,
                    effects: processing_effects,
                },
                consumed,
            ) = parse_withdrawal_events_prefix(
                &all_events[event_cursor..],
                call.withdrawals.len(),
                observation.portal_address(),
            )?;
            event_cursor += consumed;
            effects.extend(processing_effects);
            operations.push(ImportedOperation::ProcessWithdrawals(
                WithdrawalProcessing {
                    base_fee: U256::from(header.header().base_fee_per_gas().ok_or_else(|| {
                        AdapterFindingCode::EventSequence
                            .failure("imported header missing base fee")
                    })?),
                    withdrawals,
                    remaining_queue: call.remainingQueue,
                    outcomes,
                },
            ));
        }
    }
    Ok(event_cursor)
}

/// Dispatch one transaction's remaining events into operations and effects.
fn dispatch_remaining_events(
    events: &[&L1ProtocolEvent],
    is_creation_block: bool,
    direct_calls_empty: bool,
    observation: &L1BlockObservation,
    operations: &mut Vec<ImportedOperation>,
    effects: &mut Vec<Effect>,
) -> Result<(), Failure> {
    if direct_calls_empty
        && events.iter().any(|event| {
            matches!(
                event,
                L1ProtocolEvent::Portal(
                    Portal::ZonePortalEvents::BatchSubmitted(_)
                        | Portal::ZonePortalEvents::WithdrawalProcessed(_)
                        | Portal::ZonePortalEvents::WithdrawalBounceBack(_)
                        | Portal::ZonePortalEvents::DepositBounceBack(_)
                        | Portal::ZonePortalEvents::DepositBounceBackPending(_)
                )
            )
        })
    {
        return Err(AdapterFindingCode::EventSequence
            .failure("direct-call event occurred outside its transaction envelope"));
    }
    let creation = creation_context(events)?;
    if !is_creation_block && creation.is_some() {
        return Err(AdapterFindingCode::EventSequence
            .failure("ZoneCreated occurred outside the configured creation block"));
    }
    let creation_token_index = creation.as_ref().map(|c| c.event_index);
    for (event_index, event) in events.iter().copied().enumerate() {
        match event {
            L1ProtocolEvent::FactoryZoneCreated(event @ Factory::ZoneCreated { .. })
                if is_creation_block =>
            {
                operations.push(ImportedOperation::Create {
                    identity: PortalIdentity::from(event),
                    initial_token: creation
                        .as_ref()
                        .map(|c| c.initial_token.clone())
                        .ok_or_else(|| {
                            AdapterFindingCode::EventSequence
                                .failure("creation missing TokenEnabled")
                        })?,
                });
            }
            L1ProtocolEvent::Portal(Portal::ZonePortalEvents::TokenEnabled(e))
                if creation_token_index != Some(event_index + 1) =>
            {
                operations.push(ImportedOperation::EnableToken(TokenEnable::from(e)))
            }
            L1ProtocolEvent::Portal(Portal::ZonePortalEvents::BouncebackGasUpdated(e)) => {
                operations.push(ImportedOperation::UpdateBouncebackGas(e.bouncebackGas))
            }
            L1ProtocolEvent::Portal(Portal::ZonePortalEvents::DepositMade(e)) => {
                push_deposit_appended(operations, effects, observation.portal_address(), e)?;
            }
            L1ProtocolEvent::Portal(Portal::ZonePortalEvents::RefundClaimed(e)) => {
                push_refund_claim(operations, effects, e);
            }
            L1ProtocolEvent::Portal(Portal::ZonePortalEvents::TokenEnabled(_)) => {}
            L1ProtocolEvent::FactoryZoneCreated(_) => {}
            L1ProtocolEvent::KnownIgnored | L1ProtocolEvent::Portal(_) => {
                return Err(AdapterFindingCode::EventSequence
                    .failure("protocol event does not match the expected sequence"));
            }
        }
    }
    Ok(())
}

/// Read the next event or fail with a sequence violation.
fn next_event<'a>(
    events: &[&'a L1ProtocolEvent],
    cursor: &mut usize,
    missing: &'static str,
) -> Result<&'a L1ProtocolEvent, Failure> {
    let event = events
        .get(*cursor)
        .copied()
        .ok_or_else(|| AdapterFindingCode::EventSequence.failure(missing))?;
    *cursor += 1;
    Ok(event)
}

/// Record a Portal refund claim as both an operation and an effect.
fn push_refund_claim(
    operations: &mut Vec<ImportedOperation>,
    effects: &mut Vec<Effect>,
    event: &Portal::RefundClaimed,
) {
    operations.push(ImportedOperation::ClaimPortalRefund(RefundClaim::from(
        event,
    )));
    effects.push(Effect::from(event));
}

/// Record an ordinary deposit as both an operation and an effect.
fn push_deposit_appended(
    operations: &mut Vec<ImportedOperation>,
    effects: &mut Vec<Effect>,
    portal: Address,
    event: &Portal::DepositMade,
) -> Result<(), Failure> {
    let deposit = event.try_into()?;
    operations.push(ImportedOperation::AppendDeposit(deposit));
    effects.push(Effect::DepositAppended {
        id: crate::kernel::DepositId::new(portal, event.depositNumber)
            .ok_or_else(|| AdapterFindingCode::EventSequence.failure("zero deposit number"))?,
        queue_hash: event.newCurrentDepositQueueHash,
    });
    Ok(())
}

/// Validate zone-creation event placement within one transaction's events.
fn creation_context(events: &[&L1ProtocolEvent]) -> Result<Option<CreationContext>, Failure> {
    let Some(event_index) = events
        .iter()
        .position(|event| matches!(event, L1ProtocolEvent::FactoryZoneCreated(_)))
    else {
        return Ok(None);
    };
    if event_index != 1
        || events
            .iter()
            .filter(|event| matches!(event, L1ProtocolEvent::FactoryZoneCreated(_)))
            .count()
            != 1
    {
        return Err(AdapterFindingCode::EventSequence
            .failure("creation requires TokenEnabled followed by one ZoneCreated"));
    }
    let L1ProtocolEvent::Portal(Portal::ZonePortalEvents::TokenEnabled(event)) = events[0] else {
        return Err(AdapterFindingCode::EventSequence
            .failure("creation requires TokenEnabled followed by one ZoneCreated"));
    };
    Ok(Some(CreationContext {
        event_index,
        initial_token: TokenEnable::from(event),
    }))
}

/// Parse one `processWithdrawals` prefix and return the consumed event count.
fn parse_withdrawal_events_prefix(
    events: &[&L1ProtocolEvent],
    member_count: usize,
    portal: Address,
) -> Result<(WithdrawalOutcomesAndEffects, usize), Failure> {
    let mut cursor = 0;
    let mut acc = WithdrawalOutcomesAndEffects {
        outcomes: Vec::with_capacity(member_count),
        effects: Vec::new(),
    };
    for _ in 0..member_count {
        let mut operations = Vec::new();
        while let Some(event) = events.get(cursor).copied() {
            let Some((operation, effect)) = parse_callback_operation(event, portal)? else {
                break;
            };
            cursor += 1;
            operations.push(operation);
            if let Some(effect) = effect {
                acc.effects.push(effect);
            }
        }
        let event = next_event(
            events,
            &mut cursor,
            "processWithdrawals missing member outcome",
        )?;
        if !operations.is_empty()
            && !matches!(
                event,
                L1ProtocolEvent::Portal(Portal::ZonePortalEvents::WithdrawalProcessed(_))
            )
        {
            return Err(AdapterFindingCode::EventSequence
                .failure("callback operations must be followed by WithdrawalProcessed"));
        }
        match event {
            L1ProtocolEvent::Portal(Portal::ZonePortalEvents::DepositBounceBack(e)) => {
                acc.push(WithdrawalOutcome::from(e), Effect::from(e));
            }
            L1ProtocolEvent::Portal(Portal::ZonePortalEvents::DepositBounceBackPending(e)) => {
                acc.push(WithdrawalOutcome::from(e), Effect::from(e));
            }
            L1ProtocolEvent::Portal(Portal::ZonePortalEvents::WithdrawalBounceBack(e)) => {
                acc.effects.push(Effect::BounceBackAppended {
                    fallback_nonce: e.fallbackNonce,
                    token: e.token,
                    amount: e.amount,
                    id: crate::kernel::DepositId::new(portal, e.depositNumber).ok_or_else(
                        || {
                            AdapterFindingCode::EventSequence
                                .failure("zero bounceback deposit number")
                        },
                    )?,
                    queue_hash: e.newCurrentDepositQueueHash,
                });
                let after_bounce = next_event(
                    events,
                    &mut cursor,
                    "WithdrawalBounceBack must be followed by WithdrawalProcessed",
                )?;
                let L1ProtocolEvent::Portal(Portal::ZonePortalEvents::WithdrawalProcessed(
                    processed,
                )) = after_bounce
                else {
                    return Err(AdapterFindingCode::EventSequence
                        .failure("WithdrawalBounceBack must be followed by WithdrawalProcessed"));
                };
                if processed.callbackSuccess {
                    return Err(AdapterFindingCode::EventSequence
                        .failure("bounce WithdrawalProcessed callbackSuccess must be false"));
                }
                acc.push(WithdrawalOutcome::UserBounced, Effect::from(processed));
            }
            L1ProtocolEvent::Portal(Portal::ZonePortalEvents::WithdrawalProcessed(processed)) => {
                if !processed.callbackSuccess {
                    return Err(AdapterFindingCode::EventSequence
                        .failure("delivered WithdrawalProcessed callbackSuccess must be true"));
                }
                acc.push(
                    WithdrawalOutcome::UserDelivered { operations },
                    Effect::from(processed),
                );
            }
            _ => {
                return Err(AdapterFindingCode::EventSequence
                    .failure("unexpected processWithdrawals member event"));
            }
        }
    }
    Ok((acc, cursor))
}

/// Parse one checker-relevant Portal event emitted by a withdrawal callback.
fn parse_callback_operation(
    event: &L1ProtocolEvent,
    portal: Address,
) -> Result<Option<(PortalCallbackOperation, Option<Effect>)>, Failure> {
    let operation = match event {
        L1ProtocolEvent::Portal(Portal::ZonePortalEvents::DepositMade(event)) => {
            let deposit = event.try_into()?;
            let effect = Effect::DepositAppended {
                id: crate::kernel::DepositId::new(portal, event.depositNumber).ok_or_else(
                    || AdapterFindingCode::EventSequence.failure("zero callback deposit number"),
                )?,
                queue_hash: event.newCurrentDepositQueueHash,
            };
            (
                PortalCallbackOperation::AppendDeposit(deposit),
                Some(effect),
            )
        }
        L1ProtocolEvent::Portal(Portal::ZonePortalEvents::RefundClaimed(event)) => (
            PortalCallbackOperation::ClaimRefund(RefundClaim::from(event)),
            Some(Effect::from(event)),
        ),
        L1ProtocolEvent::Portal(Portal::ZonePortalEvents::TokenEnabled(event)) => (
            PortalCallbackOperation::EnableToken(TokenEnable::from(event)),
            None,
        ),
        L1ProtocolEvent::Portal(Portal::ZonePortalEvents::BouncebackGasUpdated(event)) => (
            PortalCallbackOperation::UpdateBouncebackGas(event.bouncebackGas),
            None,
        ),
        _ => return Ok(None),
    };
    Ok(Some(operation))
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, B256, Bytes, FixedBytes, U256};

    use super::*;

    const PORTAL_ADDRESS: Address = Address::repeat_byte(0x11);

    fn refund_claimed() -> L1ProtocolEvent {
        L1ProtocolEvent::Portal(Portal::ZonePortalEvents::RefundClaimed(
            Portal::RefundClaimed {
                recipient: Address::repeat_byte(0x22),
                token: Address::repeat_byte(0x33),
                amount: 44,
            },
        ))
    }

    fn withdrawal_processed(callback_success: bool) -> L1ProtocolEvent {
        L1ProtocolEvent::Portal(Portal::ZonePortalEvents::WithdrawalProcessed(
            Portal::WithdrawalProcessed {
                to: Address::repeat_byte(0x44),
                senderTag: B256::repeat_byte(0x55),
                token: Address::repeat_byte(0x33),
                amount: 66,
                callbackSuccess: callback_success,
            },
        ))
    }

    /// Only `ciphertext` (must be 64 bytes) and `depositNumber` (must be nonzero) are
    /// load-bearing here; every assertion touching this event's operation/effect
    /// matches it as a wildcard, so the other fields are trivial placeholders.
    fn deposit_made() -> L1ProtocolEvent {
        L1ProtocolEvent::Portal(Portal::ZonePortalEvents::DepositMade(Portal::DepositMade {
            newCurrentDepositQueueHash: B256::ZERO,
            sender: Address::ZERO,
            token: Address::repeat_byte(0x33),
            netAmount: 0,
            fee: 0,
            keyIndex: U256::ZERO,
            ephemeralPubkeyX: B256::ZERO,
            ephemeralPubkeyYParity: 0,
            ciphertext: Bytes::from(vec![0; 64]),
            nonce: FixedBytes::ZERO,
            tag: FixedBytes::ZERO,
            tempoRefundRecipient: Address::ZERO,
            depositNumber: 1,
        }))
    }

    /// Only `token` is asserted; `name`/`symbol`/`currency` are unchecked.
    fn token_enabled() -> L1ProtocolEvent {
        L1ProtocolEvent::Portal(Portal::ZonePortalEvents::TokenEnabled(
            Portal::TokenEnabled {
                token: Address::repeat_byte(0xaa),
                name: String::new(),
                symbol: String::new(),
                currency: String::new(),
            },
        ))
    }

    fn bounceback_gas_updated() -> L1ProtocolEvent {
        L1ProtocolEvent::Portal(Portal::ZonePortalEvents::BouncebackGasUpdated(
            Portal::BouncebackGasUpdated { bouncebackGas: 11 },
        ))
    }

    #[test]
    fn callback_refund_claim_precedes_successful_withdrawal_completion() {
        let events = [refund_claimed(), withdrawal_processed(true)];

        let (result, consumed) =
            parse_withdrawal_events_prefix(&events.each_ref(), 1, PORTAL_ADDRESS).unwrap();
        assert_eq!(consumed, events.len());

        assert_eq!(
            result.outcomes,
            vec![WithdrawalOutcome::UserDelivered {
                operations: vec![PortalCallbackOperation::ClaimRefund(RefundClaim {
                    token: Address::repeat_byte(0x33),
                    recipient: Address::repeat_byte(0x22),
                    amount: 44,
                })],
            }]
        );
        assert_eq!(
            result.effects,
            vec![
                Effect::RefundClaimed {
                    token: Address::repeat_byte(0x33),
                    recipient: Address::repeat_byte(0x22),
                    amount: 44,
                },
                Effect::UserWithdrawalProcessed {
                    to: Address::repeat_byte(0x44),
                    sender_tag: B256::repeat_byte(0x55),
                    token: Address::repeat_byte(0x33),
                    amount: 66,
                    callback_success: true,
                },
            ]
        );
    }

    #[test]
    fn mixed_callback_operations_preserve_receipt_order() {
        let events = [
            deposit_made(),
            refund_claimed(),
            token_enabled(),
            bounceback_gas_updated(),
            withdrawal_processed(true),
        ];

        let (result, consumed) =
            parse_withdrawal_events_prefix(&events.each_ref(), 1, PORTAL_ADDRESS).unwrap();
        assert_eq!(consumed, events.len());

        let [WithdrawalOutcome::UserDelivered { operations }] = result.outcomes.as_slice() else {
            panic!("expected one delivered withdrawal")
        };
        assert!(matches!(
            operations.as_slice(),
            [
                PortalCallbackOperation::AppendDeposit(_),
                PortalCallbackOperation::ClaimRefund(_),
                PortalCallbackOperation::EnableToken(TokenEnable { token, .. }),
                PortalCallbackOperation::UpdateBouncebackGas(11),
            ] if *token == Address::repeat_byte(0xaa)
        ));
        assert!(matches!(
            result.effects.as_slice(),
            [
                Effect::DepositAppended { .. },
                Effect::RefundClaimed { .. },
                Effect::UserWithdrawalProcessed {
                    callback_success: true,
                    ..
                },
            ]
        ));
    }

    #[test]
    fn callback_operation_cannot_precede_a_failed_withdrawal_completion() {
        let events = [refund_claimed(), withdrawal_processed(false)];

        assert!(parse_withdrawal_events_prefix(&events.each_ref(), 1, PORTAL_ADDRESS).is_err());
    }
}
