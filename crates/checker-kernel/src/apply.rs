use std::collections::BTreeMap;
use std::num::NonZeroU64;

use alloy_primitives::{Address, U256};

use crate::{
    commitments::{ordinary_deposit_hash, portal_address},
    effects::{ExpectedEffect, ExpectedState},
    facts::{
        DepositOutcome, ImportedFacts, ImportedOperation, OrdinaryDeposit, TokenEnable, ZoneFacts,
    },
    state::{
        Cursor, DepositId, DepositOwner, Overlay, PortalState, State, StateDelta, StateKey,
        StateValue, TokenAccounting, TokenPhase, TokenState, WithdrawalId, WithdrawalOwner,
        ZoneState,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedCandidate {
    state: State,
    effects: Vec<ExpectedEffect>,
    delta: StateDelta,
    token_enables: Vec<TokenEnable>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub delta: StateDelta,
    pub expected_effects: Vec<ExpectedEffect>,
    pub expected_state: ExpectedState,
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
                    })),
                );
                enable_token(&mut overlay, initial_token)?;
                token_enables.push(initial_token.clone());
            }
            ImportedOperation::UpdateBouncebackGas(gas) => {
                let PortalState::Created {
                    identity, deposit, ..
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
        effects.push(ExpectedEffect::TokenEnabled {
            token: enable.token,
            name: enable.name.clone(),
            symbol: enable.symbol.clone(),
            currency: enable.currency.clone(),
        });
    }
    for (deposit, outcome) in facts.deposits.iter().zip(&facts.outcomes) {
        consume_deposit(&mut overlay, deposit, *outcome, &mut effects)?;
    }
    let expected_state = expected_state(&overlay)?;
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
    })
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
    effects: &mut Vec<ExpectedEffect>,
) -> Result<(), ModelError> {
    if input.tempo_refund_recipient.is_zero() {
        return Err(ModelError::ZeroRefundRecipient);
    }
    let PortalState::Created {
        identity,
        bounceback_gas,
        deposit,
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
        })),
    );
    effects.push(ExpectedEffect::DepositAppended {
        id,
        queue_hash: hash,
    });
    Ok(())
}

fn consume_deposit(
    overlay: &mut Overlay<'_>,
    supplied: &OrdinaryDeposit,
    outcome: DepositOutcome,
    effects: &mut Vec<ExpectedEffect>,
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
    let Some(StateValue::Deposit(DepositOwner::Ordinary(expected))) =
        overlay.get(&StateKey::Deposit(id))
    else {
        return Err(ModelError::DepositPrefixMismatch);
    };
    let expected = expected.clone();
    if &expected != supplied {
        return Err(ModelError::DepositPrefixMismatch);
    }
    let hash = ordinary_deposit_hash(&expected, zone.processed_deposit.hash);
    let mut token = token(overlay, expected.token)?;
    if token.phase != TokenPhase::ZoneEnabled {
        return Err(ModelError::TokenNotZoneEnabled(expected.token));
    }
    match outcome {
        DepositOutcome::Minted => {
            token.accounting.supply = token
                .accounting
                .supply
                .checked_add(U256::from(expected.amount))
                .ok_or(ModelError::Overflow)?;
            token.accounting.deposits = token
                .accounting
                .deposits
                .checked_sub(U256::from(expected.amount))
                .ok_or(ModelError::Underflow)?;
            effects.push(ExpectedEffect::DepositProcessed {
                deposit_hash: hash,
                sender: expected.sender,
                token: expected.token,
                amount: expected.amount,
            });
        }
        DepositOutcome::Failed => {
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
                Some(StateValue::Withdrawal(WithdrawalOwner::FailedDeposit {
                    deposit: id,
                    token: expected.token,
                    recipient: expected.tempo_refund_recipient,
                    amount: expected.amount,
                })),
            );
            effects.push(ExpectedEffect::DepositFailed {
                deposit_hash: hash,
                sender: expected.sender,
                token: expected.token,
                amount: expected.amount,
                withdrawal,
                recipient: expected.tempo_refund_recipient,
            });
        }
    }
    zone.processed_deposit = Cursor { hash, number };
    overlay.set(
        StateKey::Token(expected.token),
        Some(StateValue::Token(token)),
    );
    overlay.set(StateKey::Deposit(id), None);
    overlay.set(StateKey::Zone, Some(StateValue::Zone(zone)));
    Ok(())
}

fn expected_state(overlay: &Overlay<'_>) -> Result<ExpectedState, ModelError> {
    let zone = zone(overlay)?;
    let collateral_requirement = overlay
        .range(
            std::ops::Bound::Included(StateKey::Token(Address::ZERO)),
            std::ops::Bound::Included(StateKey::Token(Address::repeat_byte(0xff))),
        )
        .try_fold(U256::ZERO, |total, (_, value)| match value {
            StateValue::Token(token) => total
                .checked_add(token.accounting.collateral().ok_or(ModelError::Overflow)?)
                .ok_or(ModelError::Overflow),
            _ => Err(ModelError::CorruptState),
        })?;
    Ok(ExpectedState {
        processed_deposit_hash: zone.processed_deposit.hash,
        processed_deposit_number: zone.processed_deposit.number,
        withdrawal_queue_hash: zone.withdrawal_queue_hash,
        withdrawal_batch_index: zone.withdrawal_batch_index,
        collateral_requirement,
    })
}
