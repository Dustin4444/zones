use std::{collections::BTreeMap, num::NonZeroU64};

use alloy_primitives::{Address, U256};

use crate::{
    commitments::{
        NO_QUEUE_INDEX, RING_CAPACITY, WITHDRAWAL_SENTINEL, bounceback_deposit_hash,
        bounceback_fee, failed_deposit_sender_tag, ordinary_deposit_hash, portal_address,
        sender_tag, withdrawal_fee, withdrawal_hash, withdrawal_queue_hash,
    },
    effects::{Effect, ExpectedState},
    facts::{
        BatchSubmission, BounceBackDeposit, Deposit, DepositOutcome, Finalization, ImportedFacts,
        ImportedOperation, OrdinaryDeposit, RefundClaim, TokenEnable, WithdrawalOutcome,
        WithdrawalProcessing, ZoneFacts, ZoneOperation,
    },
    state::{
        BatchBoundary, BatchId, BatchState, Cursor, DepositId, DepositOwner, FallbackId,
        FallbackState, InboxRefundId, Overlay, PortalRefundId, PortalState, RefundCredit,
        Settlement, State, StateDelta, StateKey, StateValue, TokenAccounting, TokenPhase,
        TokenState, Withdrawal, WithdrawalId, WithdrawalOrigin, WithdrawalOwner, ZoneState,
    },
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    #[error("Portal has not been created")]
    PortalNotCreated,
    #[error("Portal is already created")]
    PortalAlreadyCreated,
    #[error("Portal identity mismatch")]
    PortalIdentityMismatch,
    #[error("Portal address does not match Zone ID")]
    PortalAddressMismatch,
    #[error("initial token mismatch")]
    InitialTokenMismatch,
    #[error("token {0} is already enabled")]
    TokenAlreadyEnabled(Address),
    #[error("token {0} is not Portal-enabled")]
    TokenNotEnabled(Address),
    #[error("token {0} is not Zone-enabled")]
    TokenNotZoneEnabled(Address),
    #[error("Zone token-enablement prefix does not match imported Portal operations")]
    TokenEnableMismatch,
    #[error("Tempo refund recipient is zero")]
    ZeroRefundRecipient,
    #[error("counter or accounting overflow")]
    Overflow,
    #[error("counter or accounting underflow")]
    Underflow,
    #[error("deposit owner collision")]
    DepositCollision,
    #[error("deposit prefix mismatch")]
    DepositPrefixMismatch,
    #[error("deposit outcome count mismatch")]
    DepositOutcomeCountMismatch,
    #[error("withdrawal owner collision")]
    WithdrawalCollision,
    #[error("corrupt state row family")]
    CorruptState,
    #[error("withdrawal block cap exceeded")]
    WithdrawalCap,
    #[error("batch, queue, or commitment mismatch")]
    CommitmentMismatch,
    #[error("owner is missing or has the wrong lifecycle phase")]
    OwnerMismatch,
    #[error("refund claim amount mismatch")]
    RefundMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedCandidate {
    state: State,
    effects: Vec<Effect>,
    delta: StateDelta,
    token_enables: Vec<TokenEnable>,
    block_hash: alloy_primitives::B256,
    block_number: u64,
}

impl ImportedCandidate {
    /// Exact accounting after the imported Tempo block and before any Zone
    /// inputs are applied. This is the collateral cut authenticated on Tempo.
    pub fn expected_accounting(&self) -> Result<BTreeMap<Address, TokenAccounting>, ModelError> {
        token_accounting(&Overlay::new(&self.state))
    }

    /// Effects independently predicted from the imported block alone.
    pub fn expected_effects(&self) -> &[Effect] {
        &self.effects
    }

    /// Materialize the authenticated post-import state without applying a
    /// synthetic Zone transition.
    pub fn into_state(self) -> State {
        self.state
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub delta: StateDelta,
    pub expected_effects: Vec<Effect>,
    pub expected_state: ExpectedState,
    /// Exact accounting for every token in the resulting overlay, including
    /// unchanged token rows which therefore do not occur in `delta`.
    pub expected_accounting: BTreeMap<Address, TokenAccounting>,
}

/// Promote the Portal-enabled token set proven at the Zone genesis anchor.
///
/// Genesis has no ordinary `advanceTempo` transition, so this is the only
/// transition allowed to change token phase without matching imported
/// `TokenEnabled` effects. The caller must independently authenticate zero
/// genesis supply for this exact token set before applying the delta.
pub fn apply_genesis_handoff(parent: &State) -> Result<StateDelta, ModelError> {
    if !matches!(portal(&Overlay::new(parent))?, PortalState::Created { .. }) {
        return Err(ModelError::PortalNotCreated);
    }
    let mut overlay = Overlay::new(parent);
    let tokens = overlay
        .range(
            std::ops::Bound::Included(StateKey::Token(Address::ZERO)),
            std::ops::Bound::Included(StateKey::Token(Address::repeat_byte(0xff))),
        )
        .map(|(key, value)| match (key, value) {
            (StateKey::Token(address), StateValue::Token(token)) => {
                Ok((address.to_owned(), *token))
            }
            _ => Err(ModelError::CorruptState),
        })
        .collect::<Result<Vec<_>, _>>()?;
    for (address, mut token) in tokens {
        if token.phase == TokenPhase::PendingZoneEnable {
            token.phase = TokenPhase::ZoneEnabled;
            overlay.set(StateKey::Token(address), Some(StateValue::Token(token)));
        }
    }
    Ok(overlay.finish())
}

pub fn apply_imported(
    parent: &State,
    facts: &ImportedFacts,
) -> Result<ImportedCandidate, ModelError> {
    let mut overlay = Overlay::new(parent);
    let mut effects = Vec::new();
    let mut token_enables = Vec::new();
    for operation in &facts.operations {
        match operation {
            ImportedOperation::Create {
                identity,
                initial_token,
            } => {
                let portal = portal(&overlay)?;
                let PortalState::AwaitingCreation(expected) = portal else {
                    return Err(ModelError::PortalAlreadyCreated);
                };
                if expected != *identity {
                    return Err(ModelError::PortalIdentityMismatch);
                }
                if portal_address(identity.zone_id) != identity.portal {
                    return Err(ModelError::PortalAddressMismatch);
                }
                if initial_token.token != identity.initial_token {
                    return Err(ModelError::InitialTokenMismatch);
                }
                overlay.set(
                    StateKey::Portal,
                    Some(StateValue::Portal(PortalState::Created {
                        identity: *identity,
                        bounceback_gas: 0,
                        deposit: Cursor::ZERO,
                        settlement: Settlement::ZERO,
                    })),
                );
                enable_token(&mut overlay, initial_token)?;
                token_enables.push(initial_token.clone());
            }
            ImportedOperation::UpdateBouncebackGas(gas) => {
                let PortalState::Created {
                    identity,
                    deposit,
                    settlement,
                    ..
                } = portal(&overlay)?
                else {
                    return Err(ModelError::PortalNotCreated);
                };
                overlay.set(
                    StateKey::Portal,
                    Some(StateValue::Portal(PortalState::Created {
                        identity,
                        bounceback_gas: *gas,
                        deposit,
                        settlement,
                    })),
                );
            }
            ImportedOperation::EnableToken(enable) => {
                enable_token(&mut overlay, enable)?;
                token_enables.push(enable.clone());
            }
            ImportedOperation::AppendDeposit(deposit) => {
                append_deposit(&mut overlay, deposit, &mut effects)?;
            }
            ImportedOperation::SubmitBatch(input) => {
                submit_batch(&mut overlay, input, &mut effects)?
            }
            ImportedOperation::ProcessWithdrawals(input) => {
                process_withdrawals(&mut overlay, input, input.base_fee, &mut effects)?
            }
            ImportedOperation::ClaimPortalRefund(input) => {
                claim_refund(&mut overlay, *input, true, &mut effects)?
            }
        }
    }
    let delta = overlay.finish();
    let mut state = parent.clone();
    state
        .apply(&delta)
        .expect("transition creates matching state row families");
    Ok(ImportedCandidate {
        state,
        effects,
        delta,
        token_enables,
        block_hash: facts.block_hash,
        block_number: facts.block_number,
    })
}

pub fn apply_zone(imported: ImportedCandidate, facts: &ZoneFacts) -> Result<Candidate, ModelError> {
    if imported.token_enables != facts.enabled_tokens {
        return Err(ModelError::TokenEnableMismatch);
    }
    if facts.deposits.len() != facts.outcomes.len() {
        return Err(ModelError::DepositOutcomeCountMismatch);
    }
    let mut overlay = Overlay::new(&imported.state);
    let mut effects = imported.effects;
    for enable in &facts.enabled_tokens {
        let mut token = token(&overlay, enable.token)?;
        token.phase = TokenPhase::ZoneEnabled;
        overlay.set(
            StateKey::Token(enable.token),
            Some(StateValue::Token(token)),
        );
        effects.push(Effect::TokenEnabled {
            token: enable.token,
            name: enable.name.clone(),
            symbol: enable.symbol.clone(),
            currency: enable.currency.clone(),
        });
    }
    for (deposit, outcome) in facts.deposits.iter().zip(&facts.outcomes) {
        consume_deposit(&mut overlay, deposit, *outcome, &mut effects)?;
    }
    if let PortalState::Created { deposit, .. } = portal(&overlay)?
        && zone(&overlay)?.processed_deposit != deposit
    {
        return Err(ModelError::CommitmentMismatch);
    }
    apply_zone_operations(&mut overlay, &facts.operations, &mut effects)?;
    if let Some(finalization) = &facts.finalization {
        finalize(
            &mut overlay,
            facts,
            imported.block_number,
            finalization,
            &mut effects,
        )?;
    }
    let expected_accounting = token_accounting(&overlay)?;
    let expected_state = expected_state(&overlay, imported.block_hash, imported.block_number)?;
    let zone_delta = overlay.finish();
    let mut writes = imported
        .delta
        .writes()
        .iter()
        .cloned()
        .collect::<BTreeMap<_, _>>();
    writes.extend(zone_delta.writes().iter().cloned());
    Ok(Candidate {
        delta: StateDelta::from_sorted_writes(writes.into_iter().collect()),
        expected_effects: effects,
        expected_state,
        expected_accounting,
    })
}

fn token_accounting(
    overlay: &Overlay<'_>,
) -> Result<BTreeMap<Address, TokenAccounting>, ModelError> {
    overlay
        .range(
            std::ops::Bound::Included(StateKey::Token(Address::ZERO)),
            std::ops::Bound::Included(StateKey::Token(Address::repeat_byte(0xff))),
        )
        .map(|(key, value)| match (key, value) {
            (StateKey::Token(token), StateValue::Token(state)) => {
                Ok((token.to_owned(), state.accounting))
            }
            _ => Err(ModelError::CorruptState),
        })
        .collect()
}

fn portal(overlay: &Overlay<'_>) -> Result<PortalState, ModelError> {
    match overlay.get(&StateKey::Portal) {
        Some(StateValue::Portal(portal)) => Ok(portal.clone()),
        _ => Err(ModelError::CorruptState),
    }
}

fn zone(overlay: &Overlay<'_>) -> Result<ZoneState, ModelError> {
    match overlay.get(&StateKey::Zone) {
        Some(StateValue::Zone(zone)) => Ok(zone.clone()),
        _ => Err(ModelError::CorruptState),
    }
}

fn token(overlay: &Overlay<'_>, address: Address) -> Result<TokenState, ModelError> {
    match overlay.get(&StateKey::Token(address)) {
        Some(StateValue::Token(token)) => Ok(*token),
        _ => Err(ModelError::TokenNotEnabled(address)),
    }
}

fn enable_token(overlay: &mut Overlay<'_>, enable: &TokenEnable) -> Result<(), ModelError> {
    if !matches!(portal(overlay)?, PortalState::Created { .. }) {
        return Err(ModelError::PortalNotCreated);
    }
    if overlay.get(&StateKey::Token(enable.token)).is_some() {
        return Err(ModelError::TokenAlreadyEnabled(enable.token));
    }
    overlay.set(
        StateKey::Token(enable.token),
        Some(StateValue::Token(TokenState {
            phase: TokenPhase::PendingZoneEnable,
            accounting: TokenAccounting::default(),
        })),
    );
    Ok(())
}

fn append_deposit(
    overlay: &mut Overlay<'_>,
    input: &OrdinaryDeposit,
    effects: &mut Vec<Effect>,
) -> Result<(), ModelError> {
    if input.tempo_refund_recipient.is_zero() {
        return Err(ModelError::ZeroRefundRecipient);
    }
    let PortalState::Created {
        identity,
        bounceback_gas,
        deposit,
        settlement,
    } = portal(overlay)?
    else {
        return Err(ModelError::PortalNotCreated);
    };
    let mut token = token(overlay, input.token)?;
    let number = deposit.number.checked_add(1).ok_or(ModelError::Overflow)?;
    let hash = ordinary_deposit_hash(input, deposit.hash);
    let id = DepositId {
        portal: identity.portal,
        number: NonZeroU64::new(number).expect("increment from a cursor is nonzero"),
    };
    if overlay.get(&StateKey::Deposit(id)).is_some() {
        return Err(ModelError::DepositCollision);
    }
    token.accounting.deposits = token
        .accounting
        .deposits
        .checked_add(U256::from(input.amount))
        .ok_or(ModelError::Overflow)?;
    overlay.set(StateKey::Token(input.token), Some(StateValue::Token(token)));
    overlay.set(
        StateKey::Deposit(id),
        Some(StateValue::Deposit(DepositOwner::Ordinary(input.clone()))),
    );
    overlay.set(
        StateKey::Portal,
        Some(StateValue::Portal(PortalState::Created {
            identity,
            bounceback_gas,
            deposit: Cursor { hash, number },
            settlement,
        })),
    );
    effects.push(Effect::DepositAppended {
        id,
        queue_hash: hash,
    });
    Ok(())
}

fn consume_deposit(
    overlay: &mut Overlay<'_>,
    supplied: &Deposit,
    outcome: DepositOutcome,
    effects: &mut Vec<Effect>,
) -> Result<(), ModelError> {
    let mut zone = zone(overlay)?;
    let number = zone
        .processed_deposit
        .number
        .checked_add(1)
        .ok_or(ModelError::Overflow)?;
    let identity = portal(overlay)?.identity();
    let id = DepositId {
        portal: identity.portal,
        number: NonZeroU64::new(number).expect("increment from a cursor is nonzero"),
    };
    let Some(StateValue::Deposit(owner)) = overlay.get(&StateKey::Deposit(id)) else {
        return Err(ModelError::DepositPrefixMismatch);
    };
    let (token_address, amount, hash) = match owner {
        DepositOwner::Ordinary(expected) => {
            if supplied != &Deposit::Ordinary(expected.clone()) {
                return Err(ModelError::DepositPrefixMismatch);
            }
            (
                expected.token,
                expected.amount,
                ordinary_deposit_hash(expected, zone.processed_deposit.hash),
            )
        }
        DepositOwner::BounceBack {
            token,
            fallback_nonce,
            amount,
            ..
        } => {
            let expected = BounceBackDeposit {
                token: *token,
                fallback_nonce: *fallback_nonce,
                amount: *amount,
            };
            if supplied != &Deposit::BounceBack(expected) {
                return Err(ModelError::DepositPrefixMismatch);
            }
            (
                *token,
                *amount,
                bounceback_deposit_hash(expected, zone.processed_deposit.hash),
            )
        }
    };
    let owner = owner.clone();
    let mut token = token(overlay, token_address)?;
    if token.phase != TokenPhase::ZoneEnabled {
        return Err(ModelError::TokenNotZoneEnabled(token_address));
    }
    match (owner, outcome) {
        (DepositOwner::Ordinary(expected), DepositOutcome::Minted) => {
            token.accounting.supply = token
                .accounting
                .supply
                .checked_add(U256::from(amount))
                .ok_or(ModelError::Overflow)?;
            token.accounting.deposits = token
                .accounting
                .deposits
                .checked_sub(U256::from(amount))
                .ok_or(ModelError::Underflow)?;
            effects.push(Effect::DepositProcessed {
                deposit_hash: hash,
                sender: expected.sender,
                token: expected.token,
                amount: expected.amount,
            });
        }
        (DepositOwner::Ordinary(expected), DepositOutcome::Failed) => {
            let withdrawal = WithdrawalId {
                zone_id: identity.zone_id,
                index: zone.next_withdrawal_index,
            };
            if overlay.get(&StateKey::Withdrawal(withdrawal)).is_some() {
                return Err(ModelError::WithdrawalCollision);
            }
            zone.next_withdrawal_index = zone
                .next_withdrawal_index
                .checked_add(1)
                .ok_or(ModelError::Overflow)?;
            overlay.set(
                StateKey::Withdrawal(withdrawal),
                Some(StateValue::Withdrawal(
                    WithdrawalOwner::PendingFailedDeposit {
                        deposit: id,
                        token: expected.token,
                        recipient: expected.tempo_refund_recipient,
                        amount: expected.amount,
                    },
                )),
            );
            effects.push(Effect::WithdrawalRequested {
                id: withdrawal,
                sender: Address::ZERO,
                token: expected.token,
                to: expected.tempo_refund_recipient,
                amount: expected.amount,
                fee: 0,
                memo: alloy_primitives::B256::ZERO,
                gas_limit: 0,
                fallback_nonce: 0,
                callback_data: Default::default(),
                reveal_to: Default::default(),
            });
            effects.push(Effect::DepositFailed {
                deposit_hash: hash,
                sender: expected.sender,
                token: expected.token,
                amount: expected.amount,
            });
        }
        (
            DepositOwner::BounceBack {
                withdrawal,
                token: owner_token,
                fallback_nonce,
                amount,
            },
            DepositOutcome::BounceBackMinted { recipient },
        ) => {
            let fallback = require_bounceback_owner(
                overlay,
                withdrawal,
                owner_token,
                fallback_nonce,
                amount,
                id,
            )?;
            if recipient.is_zero() {
                return Err(ModelError::OwnerMismatch);
            }
            token.accounting.supply = token
                .accounting
                .supply
                .checked_add(U256::from(amount))
                .ok_or(ModelError::Overflow)?;
            token.accounting.withdrawals = token
                .accounting
                .withdrawals
                .checked_sub(U256::from(amount))
                .ok_or(ModelError::Underflow)?;
            overlay.set(StateKey::Fallback(fallback), None);
            effects.push(Effect::BounceBackMinted {
                token: owner_token,
                amount,
            });
        }
        (
            DepositOwner::BounceBack {
                withdrawal,
                token: owner_token,
                fallback_nonce,
                amount,
            },
            DepositOutcome::BounceBackPending { recipient },
        ) => {
            let fallback = require_bounceback_owner(
                overlay,
                withdrawal,
                owner_token,
                fallback_nonce,
                amount,
                id,
            )?;
            if recipient.is_zero() || amount == 0 {
                return Err(ModelError::OwnerMismatch);
            }
            let refund = InboxRefundId {
                token: owner_token,
                recipient,
                withdrawal,
            };
            if overlay.get(&StateKey::InboxRefund(refund)).is_some() {
                return Err(ModelError::OwnerMismatch);
            }
            overlay.set(
                StateKey::InboxRefund(refund),
                Some(StateValue::InboxRefund(RefundCredit { amount })),
            );
            overlay.set(StateKey::Fallback(fallback), None);
            effects.push(Effect::BounceBackPending {
                token: owner_token,
                amount,
            });
        }
        _ => return Err(ModelError::DepositPrefixMismatch),
    }
    zone.processed_deposit = Cursor { hash, number };
    overlay.set(
        StateKey::Token(token_address),
        Some(StateValue::Token(token)),
    );
    overlay.set(StateKey::Deposit(id), None);
    overlay.set(StateKey::Zone, Some(StateValue::Zone(zone)));
    Ok(())
}

fn require_bounceback_owner(
    overlay: &Overlay<'_>,
    withdrawal: WithdrawalId,
    token: Address,
    fallback_nonce: NonZeroU64,
    amount: u128,
    deposit: DepositId,
) -> Result<FallbackId, ModelError> {
    let key = FallbackId {
        zone_id: withdrawal.zone_id,
        nonce: fallback_nonce,
    };
    match overlay.get(&StateKey::Fallback(key)) {
        Some(StateValue::Fallback(FallbackState::Queued {
            withdrawal: actual_withdrawal,
            token: actual_token,
            amount: actual_amount,
            deposit: actual_deposit,
        })) if *actual_withdrawal == withdrawal
            && *actual_token == token
            && *actual_amount == amount
            && *actual_deposit == deposit =>
        {
            Ok(key)
        }
        _ => Err(ModelError::OwnerMismatch),
    }
}

fn apply_zone_operations(
    overlay: &mut Overlay<'_>,
    operations: &[ZoneOperation],
    effects: &mut Vec<Effect>,
) -> Result<(), ModelError> {
    let mut accepted = 0u32;
    for operation in operations {
        match operation {
            ZoneOperation::UpdateTempoGasRate(rate) => {
                let mut z = zone(overlay)?;
                z.tempo_gas_rate = *rate;
                overlay.set(StateKey::Zone, Some(StateValue::Zone(z)));
            }
            ZoneOperation::UpdateMaxWithdrawals(cap) => {
                let mut z = zone(overlay)?;
                z.max_withdrawals_per_block = *cap;
                overlay.set(StateKey::Zone, Some(StateValue::Zone(z)));
            }
            ZoneOperation::AcceptWithdrawal(input) => {
                let mut z = zone(overlay)?;
                let limited = z.max_withdrawals_per_block != 0;
                if limited && accepted >= z.max_withdrawals_per_block {
                    return Err(ModelError::WithdrawalCap);
                }
                let mut t = token(overlay, input.token)?;
                if t.phase != TokenPhase::ZoneEnabled {
                    return Err(ModelError::TokenNotZoneEnabled(input.token));
                }
                if input.amount == 0
                    || input.transaction_hash.is_zero()
                    || input.gas_limit > 10_000_000
                    || input.callback_data.len() > 1_024
                    || (!input.reveal_to.is_empty()
                        && (input.reveal_to.len() != 33
                            || !matches!(input.reveal_to[0], 0x02 | 0x03)))
                {
                    return Err(ModelError::CommitmentMismatch);
                }
                let fee = withdrawal_fee(input.gas_limit, z.tempo_gas_rate)
                    .ok_or(ModelError::Overflow)?;
                let burn = U256::from(input.amount)
                    .checked_add(U256::from(fee))
                    .ok_or(ModelError::Overflow)?;
                t.accounting.supply = t
                    .accounting
                    .supply
                    .checked_sub(burn)
                    .ok_or(ModelError::Underflow)?;
                t.accounting.withdrawals = t
                    .accounting
                    .withdrawals
                    .checked_add(U256::from(input.amount))
                    .ok_or(ModelError::Overflow)?;
                let fallback_nonce = z
                    .last_fallback_nonce
                    .checked_add(1)
                    .ok_or(ModelError::Overflow)?;
                let fallback = FallbackId {
                    zone_id: portal(overlay)?.identity().zone_id,
                    nonce: NonZeroU64::new(fallback_nonce).expect("increment is nonzero"),
                };
                let id = WithdrawalId {
                    zone_id: fallback.zone_id,
                    index: z.next_withdrawal_index,
                };
                if overlay.get(&StateKey::Withdrawal(id)).is_some()
                    || overlay.get(&StateKey::Fallback(fallback)).is_some()
                {
                    return Err(ModelError::WithdrawalCollision);
                }
                let tag = sender_tag(input.sender, input.transaction_hash, fallback_nonce);
                let data = Withdrawal {
                    token: input.token,
                    sender_tag: tag,
                    to: input.to,
                    amount: input.amount,
                    memo: input.memo,
                    gas_limit: input.gas_limit,
                    fallback_nonce,
                    callback_data: input.callback_data.clone(),
                    encrypted_sender: input.reveal_to.clone(),
                };
                overlay.set(StateKey::Token(input.token), Some(StateValue::Token(t)));
                overlay.set(
                    StateKey::Withdrawal(id),
                    Some(StateValue::Withdrawal(WithdrawalOwner::PendingUser {
                        data,
                        fallback,
                    })),
                );
                overlay.set(
                    StateKey::Fallback(fallback),
                    Some(StateValue::Fallback(FallbackState::Held {
                        withdrawal: id,
                        token: input.token,
                        amount: input.amount,
                    })),
                );
                z.next_withdrawal_index = z
                    .next_withdrawal_index
                    .checked_add(1)
                    .ok_or(ModelError::Overflow)?;
                z.last_fallback_nonce = fallback_nonce;
                overlay.set(StateKey::Zone, Some(StateValue::Zone(z)));
                if limited {
                    accepted = accepted.checked_add(1).ok_or(ModelError::Overflow)?;
                }
                effects.push(Effect::WithdrawalRequested {
                    id,
                    sender: input.sender,
                    token: input.token,
                    to: input.to,
                    amount: input.amount,
                    fee,
                    memo: input.memo,
                    gas_limit: input.gas_limit,
                    fallback_nonce,
                    callback_data: input.callback_data.clone(),
                    reveal_to: input.reveal_to.clone(),
                });
            }
            ZoneOperation::ClaimInboxRefund(claim) => {
                claim_refund(overlay, *claim, false, effects)?
            }
        }
    }
    Ok(())
}

fn finalize(
    overlay: &mut Overlay<'_>,
    facts: &ZoneFacts,
    imported_block_number: u64,
    input: &Finalization,
    effects: &mut Vec<Effect>,
) -> Result<(), ModelError> {
    let mut z = zone(overlay)?;
    let count = z
        .next_withdrawal_index
        .checked_sub(z.batch_start.withdrawal_index)
        .ok_or(ModelError::Underflow)?;
    if input.block_number != facts.block_number
        || input.declared_count != input.encrypted_senders.len()
        || usize::try_from(count).ok() != Some(input.declared_count)
    {
        return Err(ModelError::CommitmentMismatch);
    }
    let zone_id = portal(overlay)?.identity().zone_id;
    let mut values = Vec::with_capacity(input.declared_count);
    for (offset, encrypted) in input.encrypted_senders.iter().enumerate() {
        let index = z
            .batch_start
            .withdrawal_index
            .checked_add(u64::try_from(offset).map_err(|_| ModelError::Overflow)?)
            .ok_or(ModelError::Overflow)?;
        let id = WithdrawalId { zone_id, index };
        let owner = overlay
            .get(&StateKey::Withdrawal(id))
            .cloned()
            .ok_or(ModelError::OwnerMismatch)?;
        let (mut data, origin) = match owner {
            StateValue::Withdrawal(WithdrawalOwner::PendingUser { data, fallback }) => {
                let expected_len = if data.encrypted_sender.is_empty() {
                    0
                } else {
                    113
                };
                if encrypted.len() != expected_len {
                    return Err(ModelError::CommitmentMismatch);
                }
                (data, WithdrawalOrigin::User { fallback })
            }
            StateValue::Withdrawal(WithdrawalOwner::PendingFailedDeposit {
                deposit,
                token,
                recipient,
                amount,
            }) => (
                Withdrawal {
                    token,
                    sender_tag: failed_deposit_sender_tag(),
                    to: recipient,
                    amount,
                    memo: alloy_primitives::B256::ZERO,
                    gas_limit: 0,
                    fallback_nonce: 0,
                    callback_data: Default::default(),
                    encrypted_sender: Default::default(),
                },
                WithdrawalOrigin::FailedDeposit { deposit },
            ),
            _ => return Err(ModelError::OwnerMismatch),
        };
        if matches!(origin, WithdrawalOrigin::FailedDeposit { .. }) && !encrypted.is_empty() {
            return Err(ModelError::CommitmentMismatch);
        }
        data.encrypted_sender = encrypted.clone();
        values.push(data.clone());
        overlay.set(
            StateKey::Withdrawal(id),
            Some(StateValue::Withdrawal(WithdrawalOwner::Finalized {
                data,
                origin,
            })),
        );
    }
    let index = z
        .withdrawal_batch_index
        .checked_add(1)
        .ok_or(ModelError::Overflow)?;
    let id = BatchId {
        zone_id,
        index: NonZeroU64::new(index).expect("increment is nonzero"),
    };
    if overlay.get(&StateKey::Batch(id)).is_some() {
        return Err(ModelError::OwnerMismatch);
    }
    let queue_hash = withdrawal_queue_hash(&values);
    let boundary = BatchBoundary {
        first_parent: z.batch_start.parent_hash,
        final_block: facts.block_hash,
        first_deposit: z.batch_start.deposit,
        final_deposit: z.processed_deposit,
        tempo_block: imported_block_number,
        zone_height: facts.block_number,
    };
    overlay.set(
        StateKey::Batch(id),
        Some(StateValue::Batch(BatchState::Finalized {
            boundary,
            first_withdrawal: z.batch_start.withdrawal_index,
            count,
            queue_hash,
        })),
    );
    z.withdrawal_batch_index = index;
    z.withdrawal_queue_hash = queue_hash;
    z.batch_start = crate::state::BatchBoundaryStart {
        parent_hash: facts.block_hash,
        deposit: z.processed_deposit,
        withdrawal_index: z.next_withdrawal_index,
    };
    overlay.set(StateKey::Zone, Some(StateValue::Zone(z)));
    effects.push(Effect::BatchFinalized { id, queue_hash });
    Ok(())
}

fn submit_batch(
    overlay: &mut Overlay<'_>,
    input: &BatchSubmission,
    effects: &mut Vec<Effect>,
) -> Result<(), ModelError> {
    let PortalState::Created {
        identity,
        bounceback_gas,
        deposit,
        mut settlement,
    } = portal(overlay)?
    else {
        return Err(ModelError::PortalNotCreated);
    };
    let next = settlement
        .batch_index
        .checked_add(1)
        .ok_or(ModelError::Overflow)?;
    let id = BatchId {
        zone_id: identity.zone_id,
        index: NonZeroU64::new(next).expect("increment is nonzero"),
    };
    let StateValue::Batch(BatchState::Finalized {
        boundary,
        first_withdrawal,
        count,
        queue_hash,
    }) = overlay
        .get(&StateKey::Batch(id))
        .cloned()
        .ok_or(ModelError::OwnerMismatch)?
    else {
        return Err(ModelError::OwnerMismatch);
    };
    if input.tempo_block != boundary.tempo_block
        || input.previous_block != boundary.first_parent
        || input.next_block != boundary.final_block
        || input.previous_deposit != boundary.first_deposit
        || input.next_deposit != boundary.final_deposit
        || input.withdrawal_queue_hash != queue_hash
        || input.next_zone_height != U256::from(boundary.zone_height)
        || settlement.block_hash != boundary.first_parent
        || settlement.submitted_deposit != boundary.first_deposit
        || input.next_zone_height <= settlement.zone_height
        || boundary.final_deposit.number > deposit.number
        || (count != 0 && queue_hash == WITHDRAWAL_SENTINEL)
    {
        return Err(ModelError::CommitmentMismatch);
    }
    let queue_len = checked_ring_len(settlement.queue_head, settlement.queue_tail)?;
    let queue_index = if count == 0 {
        overlay.set(StateKey::Batch(id), None);
        NO_QUEUE_INDEX
    } else {
        if queue_len == U256::from(RING_CAPACITY) {
            return Err(ModelError::WithdrawalCap);
        }
        let index = settlement.queue_tail;
        settlement.queue_tail = index.checked_add(U256::ONE).ok_or(ModelError::Overflow)?;
        overlay.set(
            StateKey::Batch(id),
            Some(StateValue::Batch(BatchState::Submitted {
                boundary,
                first_withdrawal,
                count,
                queue_hash,
                next_ordinal: 0,
                logical_queue_index: index,
            })),
        );
        index
    };
    settlement.batch_index = next;
    settlement.block_hash = boundary.final_block;
    settlement.tempo_block = boundary.tempo_block;
    settlement.submitted_deposit = boundary.final_deposit;
    settlement.zone_height = U256::from(boundary.zone_height);
    overlay.set(
        StateKey::Portal,
        Some(StateValue::Portal(PortalState::Created {
            identity,
            bounceback_gas,
            deposit,
            settlement,
        })),
    );
    effects.push(Effect::BatchSubmitted {
        id,
        queue_index,
        processed_deposit_hash: boundary.final_deposit.hash,
        final_block_hash: boundary.final_block,
        queue_hash,
        processed_deposit_number: boundary.final_deposit.number,
    });
    Ok(())
}

fn process_withdrawals(
    overlay: &mut Overlay<'_>,
    input: &WithdrawalProcessing,
    base_fee: U256,
    effects: &mut Vec<Effect>,
) -> Result<(), ModelError> {
    if input.withdrawals.len() != input.outcomes.len() {
        return Err(ModelError::DepositOutcomeCountMismatch);
    }
    if input.withdrawals.is_empty() {
        return Ok(());
    }
    let PortalState::Created {
        identity,
        bounceback_gas,
        deposit: _,
        mut settlement,
    } = portal(overlay)?
    else {
        return Err(ModelError::PortalNotCreated);
    };
    let queue_len = checked_ring_len(settlement.queue_head, settlement.queue_tail)?;
    if queue_len.is_zero() {
        return Err(ModelError::OwnerMismatch);
    }
    let (id, batch) = overlay
        .range(
            std::ops::Bound::Included(StateKey::Batch(BatchId {
                zone_id: identity.zone_id,
                index: NonZeroU64::MIN,
            })),
            std::ops::Bound::Included(StateKey::Batch(BatchId {
                zone_id: identity.zone_id,
                index: NonZeroU64::new(u64::MAX).unwrap(),
            })),
        )
        .find_map(|(key, value)| match (key, value) {
            (StateKey::Batch(id), StateValue::Batch(batch)) => Some((id, batch.clone())),
            _ => None,
        })
        .ok_or(ModelError::OwnerMismatch)?;
    let BatchState::Submitted {
        boundary,
        first_withdrawal,
        count,
        queue_hash,
        next_ordinal,
        logical_queue_index,
    } = batch
    else {
        return Err(ModelError::OwnerMismatch);
    };
    if logical_queue_index != settlement.queue_head {
        return Err(ModelError::OwnerMismatch);
    }
    let supplied = u64::try_from(input.withdrawals.len()).map_err(|_| ModelError::Overflow)?;
    let next = next_ordinal
        .checked_add(supplied)
        .ok_or(ModelError::Overflow)?;
    if next > count {
        return Err(ModelError::CommitmentMismatch);
    }
    if queue_hash == WITHDRAWAL_SENTINEL || input.remaining_queue == WITHDRAWAL_SENTINEL {
        return Err(ModelError::CommitmentMismatch);
    }
    let tail = if input.remaining_queue.is_zero() {
        WITHDRAWAL_SENTINEL
    } else {
        input.remaining_queue
    };
    let folded = input
        .withdrawals
        .iter()
        .rev()
        .fold(tail, |hash, value| withdrawal_hash(value, hash));
    if folded != queue_hash || (next == count) != input.remaining_queue.is_zero() {
        return Err(ModelError::CommitmentMismatch);
    }
    for (offset, (supplied_value, outcome)) in
        input.withdrawals.iter().zip(&input.outcomes).enumerate()
    {
        let index = first_withdrawal
            .checked_add(next_ordinal)
            .and_then(|v| v.checked_add(u64::try_from(offset).ok()?))
            .ok_or(ModelError::Overflow)?;
        let wid = WithdrawalId {
            zone_id: identity.zone_id,
            index,
        };
        let StateValue::Withdrawal(WithdrawalOwner::Finalized { data, origin }) = overlay
            .get(&StateKey::Withdrawal(wid))
            .cloned()
            .ok_or(ModelError::OwnerMismatch)?
        else {
            return Err(ModelError::OwnerMismatch);
        };
        if &data != supplied_value {
            return Err(ModelError::CommitmentMismatch);
        }
        let mut t = token(overlay, data.token)?;
        let terminal_effect = match (origin, outcome) {
            (
                WithdrawalOrigin::User { fallback },
                WithdrawalOutcome::UserDelivered { callback_deposits },
            ) => {
                require_held_fallback(overlay, fallback, wid, data.token, data.amount)?;
                if data.gas_limit == 0 && !callback_deposits.is_empty() {
                    return Err(ModelError::OwnerMismatch);
                }
                t.accounting.withdrawals = t
                    .accounting
                    .withdrawals
                    .checked_sub(U256::from(data.amount))
                    .ok_or(ModelError::Underflow)?;
                for callback in callback_deposits {
                    append_deposit(overlay, callback, effects)?;
                }
                overlay.set(StateKey::Fallback(fallback), None);
                Effect::UserWithdrawalProcessed {
                    to: data.to,
                    sender_tag: data.sender_tag,
                    token: data.token,
                    amount: data.amount,
                    callback_success: true,
                }
            }
            (WithdrawalOrigin::User { fallback }, WithdrawalOutcome::UserBounced) => {
                require_held_fallback(overlay, fallback, wid, data.token, data.amount)?;
                let nonce = fallback.nonce;
                let member = DepositOwner::BounceBack {
                    withdrawal: wid,
                    token: data.token,
                    fallback_nonce: nonce,
                    amount: data.amount,
                };
                let PortalState::Created {
                    identity: pi,
                    bounceback_gas: bg,
                    deposit: pc,
                    settlement: ps,
                } = portal(overlay)?
                else {
                    unreachable!()
                };
                let number = pc.number.checked_add(1).ok_or(ModelError::Overflow)?;
                let bounce = BounceBackDeposit {
                    token: data.token,
                    fallback_nonce: nonce,
                    amount: data.amount,
                };
                let hash = bounceback_deposit_hash(bounce, pc.hash);
                let did = DepositId {
                    portal: pi.portal,
                    number: NonZeroU64::new(number).expect("increment"),
                };
                if overlay.get(&StateKey::Deposit(did)).is_some() {
                    return Err(ModelError::DepositCollision);
                }
                overlay.set(StateKey::Deposit(did), Some(StateValue::Deposit(member)));
                overlay.set(
                    StateKey::Fallback(fallback),
                    Some(StateValue::Fallback(FallbackState::Queued {
                        withdrawal: wid,
                        token: data.token,
                        amount: data.amount,
                        deposit: did,
                    })),
                );
                overlay.set(
                    StateKey::Portal,
                    Some(StateValue::Portal(PortalState::Created {
                        identity: pi,
                        bounceback_gas: bg,
                        deposit: Cursor { hash, number },
                        settlement: ps,
                    })),
                );
                effects.push(Effect::BounceBackAppended {
                    fallback_nonce: nonce.get(),
                    token: data.token,
                    amount: data.amount,
                    id: did,
                    queue_hash: hash,
                });
                Effect::UserWithdrawalProcessed {
                    to: data.to,
                    sender_tag: data.sender_tag,
                    token: data.token,
                    amount: data.amount,
                    callback_success: false,
                }
            }
            (
                WithdrawalOrigin::FailedDeposit { deposit: _ },
                WithdrawalOutcome::FailedDepositPaid { collected_fee },
            ) => {
                let max_fee = bounceback_fee(bounceback_gas, base_fee, data.amount)
                    .ok_or(ModelError::Overflow)?;
                if *collected_fee != 0 && *collected_fee != max_fee {
                    return Err(ModelError::CommitmentMismatch);
                }
                t.accounting.deposits = t
                    .accounting
                    .deposits
                    .checked_sub(U256::from(data.amount))
                    .ok_or(ModelError::Underflow)?;
                Effect::FailedDepositRefunded {
                    recipient: data.to,
                    token: data.token,
                    amount: data.amount - *collected_fee,
                    fee: *collected_fee,
                    pending: false,
                }
            }
            (
                WithdrawalOrigin::FailedDeposit { deposit: failed },
                WithdrawalOutcome::FailedDepositPending { collected_fee },
            ) => {
                let max_fee = bounceback_fee(bounceback_gas, base_fee, data.amount)
                    .ok_or(ModelError::Overflow)?;
                if *collected_fee != 0 && *collected_fee != max_fee {
                    return Err(ModelError::CommitmentMismatch);
                }
                t.accounting.deposits = t
                    .accounting
                    .deposits
                    .checked_sub(U256::from(*collected_fee))
                    .ok_or(ModelError::Underflow)?;
                let refund = PortalRefundId {
                    token: data.token,
                    recipient: data.to,
                    deposit: failed,
                };
                if overlay.get(&StateKey::PortalRefund(refund)).is_some() {
                    return Err(ModelError::OwnerMismatch);
                }
                overlay.set(
                    StateKey::PortalRefund(refund),
                    Some(StateValue::PortalRefund(RefundCredit {
                        amount: data.amount - *collected_fee,
                    })),
                );
                Effect::FailedDepositRefunded {
                    recipient: data.to,
                    token: data.token,
                    amount: data.amount - *collected_fee,
                    fee: *collected_fee,
                    pending: true,
                }
            }
            _ => return Err(ModelError::OwnerMismatch),
        };
        overlay.set(StateKey::Token(data.token), Some(StateValue::Token(t)));
        overlay.set(StateKey::Withdrawal(wid), None);
        effects.push(terminal_effect);
    }
    if next == count {
        overlay.set(StateKey::Batch(id), None);
        settlement.queue_head = settlement
            .queue_head
            .checked_add(U256::ONE)
            .ok_or(ModelError::Overflow)?;
    } else {
        overlay.set(
            StateKey::Batch(id),
            Some(StateValue::Batch(BatchState::Submitted {
                boundary,
                first_withdrawal,
                count,
                queue_hash: input.remaining_queue,
                next_ordinal: next,
                logical_queue_index,
            })),
        );
    }
    let current = portal(overlay)?;
    let PortalState::Created {
        deposit: latest, ..
    } = current
    else {
        unreachable!()
    };
    overlay.set(
        StateKey::Portal,
        Some(StateValue::Portal(PortalState::Created {
            identity,
            bounceback_gas,
            deposit: latest,
            settlement,
        })),
    );
    Ok(())
}

fn checked_ring_len(head: U256, tail: U256) -> Result<U256, ModelError> {
    let len = tail.checked_sub(head).ok_or(ModelError::Underflow)?;
    if len > U256::from(RING_CAPACITY) {
        return Err(ModelError::OwnerMismatch);
    }
    Ok(len)
}

fn require_held_fallback(
    overlay: &Overlay<'_>,
    fallback: FallbackId,
    withdrawal: WithdrawalId,
    token: Address,
    amount: u128,
) -> Result<(), ModelError> {
    match overlay.get(&StateKey::Fallback(fallback)) {
        Some(StateValue::Fallback(FallbackState::Held {
            withdrawal: actual_withdrawal,
            token: actual_token,
            amount: actual_amount,
        })) if *actual_withdrawal == withdrawal
            && *actual_token == token
            && *actual_amount == amount =>
        {
            Ok(())
        }
        _ => Err(ModelError::OwnerMismatch),
    }
}

fn claim_refund(
    overlay: &mut Overlay<'_>,
    claim: RefundClaim,
    portal_side: bool,
    effects: &mut Vec<Effect>,
) -> Result<(), ModelError> {
    let PortalState::Created { identity, .. } = portal(overlay)? else {
        return Err(ModelError::PortalNotCreated);
    };
    let start = if portal_side {
        StateKey::PortalRefund(PortalRefundId {
            token: Address::ZERO,
            recipient: Address::ZERO,
            deposit: DepositId {
                portal: Address::ZERO,
                number: NonZeroU64::MIN,
            },
        })
    } else {
        StateKey::InboxRefund(InboxRefundId {
            token: Address::ZERO,
            recipient: Address::ZERO,
            withdrawal: WithdrawalId {
                zone_id: 0,
                index: 0,
            },
        })
    };
    let end = if portal_side {
        StateKey::PortalRefund(PortalRefundId {
            token: Address::repeat_byte(0xff),
            recipient: Address::repeat_byte(0xff),
            deposit: DepositId {
                portal: Address::repeat_byte(0xff),
                number: NonZeroU64::new(u64::MAX).expect("nonzero"),
            },
        })
    } else {
        StateKey::InboxRefund(InboxRefundId {
            token: Address::repeat_byte(0xff),
            recipient: Address::repeat_byte(0xff),
            withdrawal: WithdrawalId {
                zone_id: u32::MAX,
                index: u64::MAX,
            },
        })
    };
    let credits: Vec<_> = overlay
        .range(
            std::ops::Bound::Included(start),
            std::ops::Bound::Included(end),
        )
        .filter_map(|(key, value)| match (key, value) {
            (StateKey::PortalRefund(id), StateValue::PortalRefund(c))
                if portal_side
                    && id.deposit.portal == identity.portal
                    && id.token == claim.token
                    && id.recipient == claim.recipient =>
            {
                Some((key, c.amount))
            }
            (StateKey::InboxRefund(id), StateValue::InboxRefund(c))
                if !portal_side
                    && id.withdrawal.zone_id == identity.zone_id
                    && id.token == claim.token
                    && id.recipient == claim.recipient =>
            {
                Some((key, c.amount))
            }
            _ => None,
        })
        .collect();
    let total = credits
        .iter()
        .try_fold(0u128, |sum, (_, amount)| sum.checked_add(*amount))
        .ok_or(ModelError::Overflow)?;
    if total != claim.amount {
        return Err(ModelError::RefundMismatch);
    }
    if total != 0 {
        let mut t = token(overlay, claim.token)?;
        if portal_side {
            t.accounting.deposits = t
                .accounting
                .deposits
                .checked_sub(U256::from(total))
                .ok_or(ModelError::Underflow)?;
        } else {
            if t.phase != TokenPhase::ZoneEnabled {
                return Err(ModelError::TokenNotZoneEnabled(claim.token));
            }
            t.accounting.supply = t
                .accounting
                .supply
                .checked_add(U256::from(total))
                .ok_or(ModelError::Overflow)?;
            t.accounting.withdrawals = t
                .accounting
                .withdrawals
                .checked_sub(U256::from(total))
                .ok_or(ModelError::Underflow)?;
        }
        overlay.set(StateKey::Token(claim.token), Some(StateValue::Token(t)));
    }
    for (key, _) in credits {
        overlay.set(key, None);
    }
    effects.push(Effect::RefundClaimed {
        token: claim.token,
        recipient: claim.recipient,
        amount: total,
    });
    Ok(())
}

fn expected_state(
    overlay: &Overlay<'_>,
    tempo_block_hash: alloy_primitives::B256,
    tempo_block_number: u64,
) -> Result<ExpectedState, ModelError> {
    let zone = zone(overlay)?;
    Ok(ExpectedState {
        tempo_block_hash,
        tempo_block_number,
        processed_deposit_hash: zone.processed_deposit.hash,
        processed_deposit_number: zone.processed_deposit.number,
        withdrawal_queue_hash: zone.withdrawal_queue_hash,
        withdrawal_batch_index: zone.withdrawal_batch_index,
    })
}
