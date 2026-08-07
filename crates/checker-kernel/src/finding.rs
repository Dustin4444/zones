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
    Bytes {
        length: u64,
        digest: B256,
    },
    /// A stable protocol discriminator. This is deliberately not display text.
    Code(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FindingCategory {
    Authentication,
    EffectMismatch,
    StateMismatch,
    Invariant,
    Unsupported,
    Observation,
    Continuity,
    CreationAnchor,
    SupplyMismatch,
    CollateralMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FindingLocation {
    Operation(u32),
    State(StateKey),
    Block,
    ImportedOperation(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub category: FindingCategory,
    pub code: u16,
    pub location: Option<FindingLocation>,
    pub expected: Option<Datum>,
    pub actual: Option<Datum>,
}

/// Names used by the checker persistence contract.
pub type ViolationCategory = FindingCategory;
pub type FindingData = Datum;

impl Datum {
    /// Canonical, version-independent bytes used for finding evidence identity.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(41);
        match self {
            Self::U64(v) => {
                out.push(0);
                out.extend(v.to_be_bytes());
            }
            Self::U128(v) => {
                out.push(1);
                out.extend(v.to_be_bytes());
            }
            Self::U256(v) => {
                out.push(2);
                out.extend(v.to_be_bytes::<32>());
            }
            Self::Address(v) => {
                out.push(3);
                out.extend(v.as_slice());
            }
            Self::Hash(v) => {
                out.push(4);
                out.extend(v.as_slice());
            }
            Self::Bool(v) => {
                out.push(5);
                out.push(u8::from(*v));
            }
            Self::Bytes { length, digest } => {
                out.push(6);
                out.extend(length.to_be_bytes());
                out.extend(digest.as_slice());
            }
            Self::Code(v) => {
                out.push(7);
                out.extend(v.to_be_bytes());
            }
        }
        out
    }
}
