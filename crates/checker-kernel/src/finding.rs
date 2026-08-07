use alloy_primitives::{Address, B256, U256};
use serde::{Deserialize, Serialize};

use crate::state::StateKey;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Datum {
    U64(u64),
    U128(u128),
    U256(U256),
    Address(Address),
    Hash(B256),
    Bool(bool),
    Bytes { length: u64, digest: B256 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FindingCategory {
    Authentication,
    EffectMismatch,
    StateMismatch,
    Invariant,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FindingLocation {
    Operation(u32),
    State(StateKey),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub category: FindingCategory,
    pub code: u16,
    pub location: Option<FindingLocation>,
    pub expected: Option<Datum>,
    pub actual: Option<Datum>,
}
