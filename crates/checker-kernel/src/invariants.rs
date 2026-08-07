use alloy_primitives::U256;
use serde::{Deserialize, Serialize};

use crate::{
    finding::Datum,
    state::{PortalState, State, StateKey, StateValue, TokenPhase},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvariantCode {
    MissingPortal,
    MissingZone,
    FamilyMismatch,
    PreCreationRows,
    DepositCursor,
    DepositToken,
    AccountingOverflow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvariantViolation {
    pub code: InvariantCode,
    pub location: Option<Box<StateKey>>,
    pub expected: Option<Datum>,
    pub actual: Option<Datum>,
}

pub fn validate(state: &State) -> Result<(), InvariantViolation> {
    let Some(StateValue::Portal(portal)) = state.rows().get(&StateKey::Portal) else {
        return Err(violation(
            InvariantCode::MissingPortal,
            Some(StateKey::Portal),
        ));
    };
    if !matches!(state.rows().get(&StateKey::Zone), Some(StateValue::Zone(_))) {
        return Err(violation(InvariantCode::MissingZone, Some(StateKey::Zone)));
    }
    for (key, value) in state.rows() {
        if !value.matches_key(key) {
            return Err(violation(InvariantCode::FamilyMismatch, Some(*key)));
        }
    }
    if matches!(portal, PortalState::AwaitingCreation(_)) && state.rows().len() != 2 {
        return Err(violation(InvariantCode::PreCreationRows, None));
    }
    for (key, value) in state.rows() {
        match (key, value) {
            (StateKey::Deposit(id), StateValue::Deposit(owner)) => {
                let crate::state::DepositOwner::Ordinary(deposit) = owner;
                if !matches!(
                    state.rows().get(&StateKey::Token(deposit.token)),
                    Some(StateValue::Token(_))
                ) {
                    return Err(violation(
                        InvariantCode::DepositToken,
                        Some(StateKey::Deposit(*id)),
                    ));
                }
            }
            (StateKey::Token(_), StateValue::Token(token)) => {
                if token.accounting.collateral().is_none() {
                    return Err(violation(InvariantCode::AccountingOverflow, Some(*key)));
                }
                if token.phase == TokenPhase::PendingZoneEnable
                    && token.accounting.supply != U256::ZERO
                {
                    return Err(violation(InvariantCode::PreCreationRows, Some(*key)));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn violation(code: InvariantCode, location: Option<StateKey>) -> InvariantViolation {
    InvariantViolation {
        code,
        location: location.map(Box::new),
        expected: None,
        actual: None,
    }
}
