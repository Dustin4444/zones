use alloy_primitives::{Address, B256, FixedBytes, U256};
use serde::{Deserialize, Serialize};

use crate::state::PortalIdentity;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenEnable {
    pub token: Address,
    pub name: String,
    pub symbol: String,
    pub currency: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepositPayload {
    pub ephemeral_pubkey_x: B256,
    pub ephemeral_pubkey_y_parity: u8,
    pub ciphertext: FixedBytes<64>,
    pub nonce: FixedBytes<12>,
    pub tag: FixedBytes<16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrdinaryDeposit {
    pub token: Address,
    pub sender: Address,
    pub amount: u128,
    pub tempo_refund_recipient: Address,
    pub key_index: U256,
    pub encrypted: DepositPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportedOperation {
    Create {
        identity: PortalIdentity,
        initial_token: TokenEnable,
    },
    UpdateBouncebackGas(u64),
    EnableToken(TokenEnable),
    AppendDeposit(OrdinaryDeposit),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedFacts {
    pub operations: Vec<ImportedOperation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DepositOutcome {
    Minted,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZoneFacts {
    pub enabled_tokens: Vec<TokenEnable>,
    pub deposits: Vec<OrdinaryDeposit>,
    pub outcomes: Vec<DepositOutcome>,
}
