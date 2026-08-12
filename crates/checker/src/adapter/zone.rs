//! Parses Zone inputs and validates their authenticated event grammar.

use std::num::NonZeroU64;

use alloy_consensus::BlockHeader as _;
use alloy_primitives::B256;

use crate::{
    failure::Failure,
    kernel::{
        BounceBackDeposit, Deposit, DepositOutcome, Effect, Finalization, RefundClaim, TokenEnable,
        UserWithdrawal, ZoneFacts, ZoneOperation,
    },
    observe::{
        OrderedL2Outcome,
        events::{Inbox, L2ProtocolEvent, Outbox, TempoState},
    },
};

use super::{
    AdapterFindingCode, AuthenticatedObservation, ZoneAdaptation, deposits::ordinary_deposit,
};

/// Kernel outputs derived from authenticated Zone events.
struct ZoneOutputs {
    outcomes: Vec<DepositOutcome>,
    operations: Vec<ZoneOperation>,
    effects: Vec<Effect>,
    finalization: Option<Finalization>,
}

/// Parse Zone calldata and its validated event grammar into kernel facts and effects.
pub(super) fn facts(o: &AuthenticatedObservation) -> Result<ZoneAdaptation, Failure> {
    let advance_hash = o.l2.inputs().advance_transaction_hash();
    validate_zone_event_grammar(o)?;
    let (enabled_tokens, deposits) = adapt_deposits(o)?;
    let ZoneOutputs {
        outcomes,
        operations,
        effects,
        finalization,
    } = adapt_outcomes(o, advance_hash)?;
    Ok(ZoneAdaptation {
        facts: ZoneFacts {
            block_hash: o.l2.block_hash(),
            block_number: o.l2.block_number(),
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
fn validate_zone_event_grammar(o: &AuthenticatedObservation) -> Result<(), Failure> {
    let events = o.l2.outcomes().events();
    let advance = o.l2.inputs().advance_tempo();
    let advance_hash = o.l2.inputs().advance_transaction_hash();
    let mut cursor = 0usize;
    let imported = advance.imported_header();
    match events.first().map(|outcome| outcome.event()) {
        Some(L2ProtocolEvent::TempoState(TempoState::TempoStateEvents::TempoBlockFinalized(
            event,
        ))) if event.blockHash == imported.hash()
            && event.blockNumber == imported.number()
            && event.stateRoot == imported.header().state_root() => {}
        _ => {
            return Err(AdapterFindingCode::Grammar
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
                AdapterFindingCode::Grammar.failure(format!("deposit {index} missing outcome"))
            })?;
            if outcome.position().transaction_index() != 0
                || outcome.position().transaction_hash() != advance_hash
            {
                return Err(AdapterFindingCode::Grammar.failure(format!(
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
                    return Err(AdapterFindingCode::Grammar
                        .failure(format!("deposit {index} has invalid outcome grammar")));
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
            return Err(AdapterFindingCode::Grammar
                .failure(format!("deposit {index} has unsupported kind")));
        }
    }
    match events.get(cursor).map(|outcome| outcome.event()) {
        Some(L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::TempoAdvanced(event)))
            if event.tempoBlockHash == o.state.tempo_block_hash
                && event.tempoBlockNumber == o.state.tempo_block_number
                && event.newProcessedDepositQueueHash == o.state.processed_deposit_queue_hash
                && event.lastProcessedDepositNumber == o.state.processed_deposit_number
                && event.depositsProcessed
                    == u64::try_from(advance.deposits().len()).unwrap_or(u64::MAX) => {}
        _ => {
            return Err(AdapterFindingCode::Grammar
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
        return Err(AdapterFindingCode::Grammar.failure("extra event in advance transaction"));
    }

    let final_hash = o.l2.inputs().finalization().map(|f| f.transaction_hash());
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
                AdapterFindingCode::Grammar.failure("unexpected post-advance protocol event")
            );
        }
        cursor += 1;
    }
    match final_hash {
        Some(hash) => {
            let outcome = events.get(cursor).ok_or_else(|| {
                AdapterFindingCode::Grammar.failure("finalization missing BatchFinalized")
            })?;
            if outcome.position().transaction_hash() != hash
                || !matches!(
                    outcome.event(),
                    L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::BatchFinalized(_))
                )
            {
                return Err(
                    AdapterFindingCode::Grammar.failure("finalization does not own BatchFinalized")
                );
            }
            cursor += 1;
        }
        None if events.get(cursor).is_some_and(|o| {
            matches!(
                o.event(),
                L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::BatchFinalized(_))
            )
        }) =>
        {
            return Err(
                AdapterFindingCode::Grammar.failure("BatchFinalized has no finalization envelope")
            );
        }
        None => {}
    }
    if cursor != events.len() {
        return Err(AdapterFindingCode::Grammar.failure("extra finalization or protocol events"));
    }
    Ok(())
}

/// Consume one protocol event owned by the `advanceTempo` transaction.
fn expect_advance(
    events: &[OrderedL2Outcome],
    cursor: &mut usize,
    advance_hash: B256,
    expected: impl FnOnce(&L2ProtocolEvent) -> bool,
    label: &str,
) -> Result<(), Failure> {
    let outcome = events
        .get(*cursor)
        .ok_or_else(|| AdapterFindingCode::Grammar.failure(format!("advance missing {label}")))?;
    if outcome.position().transaction_index() != 0
        || outcome.position().transaction_hash() != advance_hash
        || !expected(outcome.event())
    {
        return Err(AdapterFindingCode::Grammar
            .failure(format!("advance expected {label} at cursor {cursor}")));
    }
    *cursor += 1;
    Ok(())
}
/// Adapt the deposits carried by an authenticated `advanceTempo` input.
fn adapt_deposits(
    o: &AuthenticatedObservation,
) -> Result<(Vec<TokenEnable>, Vec<Deposit>), Failure> {
    let advance = o.l2.inputs().advance_tempo();
    let enabled_tokens = advance
        .enabled_tokens()
        .iter()
        .map(|e| TokenEnable {
            token: e.token,
            name: e.name.clone(),
            symbol: e.symbol.clone(),
            currency: e.currency.clone(),
        })
        .collect();
    let mut deposits = Vec::new();
    for d in advance.deposits() {
        if let Some(d) = d.as_ordinary() {
            deposits.push(Deposit::Ordinary(ordinary_deposit(d)?));
        } else if let Some(d) = d.as_withdrawal_bounce_back() {
            let bytes = d.to.as_slice();
            if bytes[..12].iter().any(|byte| *byte != 0) {
                return Err(AdapterFindingCode::Grammar
                    .failure("bounceback recipient has non-canonical high bytes"));
            }
            if d.amount == 0 {
                return Err(AdapterFindingCode::Grammar.failure("zero bounceback amount"));
            }
            let mut nonce_bytes = [0; 8];
            nonce_bytes.copy_from_slice(&bytes[12..]);
            let nonce = NonZeroU64::new(u64::from_be_bytes(nonce_bytes))
                .ok_or_else(|| AdapterFindingCode::Grammar.failure("zero bounceback nonce"))?;
            deposits.push(Deposit::BounceBack(BounceBackDeposit {
                token: d.token,
                fallback_nonce: nonce,
                amount: d.amount,
            }));
        } else {
            return Err(AdapterFindingCode::Grammar.failure("unsupported deposit kind"));
        }
    }
    Ok((enabled_tokens, deposits))
}

/// Adapt authenticated Zone event outputs into kernel facts and effects.
fn adapt_outcomes(
    o: &AuthenticatedObservation,
    advance_hash: B256,
) -> Result<ZoneOutputs, Failure> {
    let mut outcomes = Vec::new();
    let mut operations = Vec::new();
    let mut effects = Vec::new();
    for outcome in o.l2.outcomes().events() {
        match outcome.event() {
            L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::TokenEnabled(e)) => {
                effects.push(Effect::TokenEnabled {
                    token: e.token,
                    name: e.name.clone(),
                    symbol: e.symbol.clone(),
                    currency: e.currency.clone(),
                })
            }
            L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::DepositProcessed(e)) => {
                outcomes.push(DepositOutcome::Minted);
                effects.push(Effect::DepositProcessed {
                    deposit_hash: e.depositHash,
                    sender: e.sender,
                    token: e.token,
                    amount: e.amount,
                });
            }
            L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::DepositFailed(e)) => {
                outcomes.push(DepositOutcome::Failed);
                effects.push(Effect::DepositFailed {
                    deposit_hash: e.depositHash,
                    sender: e.sender,
                    token: e.token,
                    amount: e.amount,
                });
            }
            L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::WithdrawalBounceBackProcessed(e)) => {
                outcomes.push(DepositOutcome::BounceBackMinted {
                    recipient: e.zoneFallbackRecipient,
                });
                effects.push(Effect::BounceBackMinted {
                    token: e.token,
                    amount: e.amount,
                });
            }
            L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::WithdrawalBounceBackPending(e)) => {
                outcomes.push(DepositOutcome::BounceBackPending {
                    recipient: e.zoneFallbackRecipient,
                });
                effects.push(Effect::BounceBackPending {
                    token: e.token,
                    amount: e.amount,
                });
            }
            L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::TempoGasRateUpdated(e)) => {
                operations.push(ZoneOperation::UpdateTempoGasRate(e.tempoGasRate))
            }
            L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::MaxWithdrawalsPerBlockUpdated(
                e,
            )) => operations.push(ZoneOperation::UpdateMaxWithdrawals(
                e.maxWithdrawalsPerBlock,
            )),
            L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::WithdrawalRequested(e)) => {
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
            L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::RefundClaimed(e)) => {
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
            L2ProtocolEvent::Outbox(Outbox::IZoneOutboxEvents::BatchFinalized(e)) => {
                effects.push(Effect::BatchFinalized {
                    id: crate::kernel::BatchId::new(o.zone_id, e.withdrawalBatchIndex).ok_or_else(
                        || AdapterFindingCode::Grammar.failure("zero finalized batch index"),
                    )?,
                    queue_hash: e.withdrawalQueueHash,
                })
            }
            L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::TempoAdvanced(_))
            | L2ProtocolEvent::TempoState(_) => {}
            L2ProtocolEvent::Inbox(Inbox::IZoneInboxEvents::DepositRejected(_)) => {
                return Err(AdapterFindingCode::Grammar
                    .failure("unsupported DepositRejected event passed classification"));
            }
        }
    }
    let finalization = o.l2.inputs().finalization().map(|f| Finalization {
        block_number: f.input().block_number(),
        declared_count: f.input().count(),
        encrypted_senders: f.input().encrypted_senders().to_vec(),
    });
    Ok(ZoneOutputs {
        outcomes,
        operations,
        effects,
        finalization,
    })
}
