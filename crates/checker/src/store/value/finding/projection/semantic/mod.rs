//! Canonical semantic evidence encoding.
//!
//! Every enum is matched explicitly and every scalar has a fixed byte order.
//! Dynamic values use checked u64 length prefixes. This is intentionally
//! independent of Rust layout and human-facing formatting.

mod errors;
mod imported_output;
mod model;
mod nested;
mod zone_output;

#[cfg(test)]
mod tests;

use alloy_primitives::{Address, B256, U256, keccak256};

use crate::store::error::{StoreError, StoreResult};

use super::super::types::FindingSummary;

pub(super) use errors::{
    imported_projection, malformed_authenticated_data, malformed_event, portal_call,
    zone_projection,
};
pub(super) use imported_output::{expected_imported_output, observed_imported_output};
pub(super) use model::model;
pub(super) use zone_output::{
    expected_batch_finalized, expected_deposit_outcome, expected_token_enable,
    expected_zone_operation, observed_batch_finalized, observed_deposit_outcome,
    observed_tempo_advanced, observed_tempo_block_finalized, observed_token_enable,
    observed_zone_operation, tempo_advanced_expectation, tempo_block_finalized_expectation,
};

#[derive(Default)]
struct Canonical {
    bytes: Vec<u8>,
}

impl Canonical {
    fn tagged(tag: u8) -> Self {
        let mut encoder = Self::default();
        encoder.u8(tag);
        encoder
    }

    fn finish(self) -> StoreResult<FindingSummary> {
        let length = checked_usize(self.bytes.len())?;
        Ok(FindingSummary::new(length, keccak256(&self.bytes)))
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn usize(&mut self, value: usize) -> StoreResult<()> {
        self.u64(checked_usize(value)?);
        Ok(())
    }

    fn u128(&mut self, value: u128) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn u256(&mut self, value: U256) {
        self.bytes.extend_from_slice(&value.to_be_bytes::<32>());
    }

    fn address(&mut self, value: Address) {
        self.bytes.extend_from_slice(value.as_slice());
    }

    fn hash(&mut self, value: B256) {
        self.bytes.extend_from_slice(value.as_slice());
    }

    fn bytes(&mut self, value: &[u8]) -> StoreResult<()> {
        self.usize(value.len())?;
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    fn str(&mut self, value: &str) -> StoreResult<()> {
        self.bytes(value.as_bytes())
    }

    fn option<T>(
        &mut self,
        value: Option<T>,
        encode: impl FnOnce(&mut Self, T) -> StoreResult<()>,
    ) -> StoreResult<()> {
        match value {
            Some(value) => {
                self.u8(1);
                encode(self, value)
            }
            None => {
                self.u8(0);
                Ok(())
            }
        }
    }
}

fn checked_usize(value: usize) -> StoreResult<u64> {
    u64::try_from(value).map_err(|_| StoreError::InvalidPersistedValue("finding usize field"))
}

fn encode_position_fields(
    encoder: &mut Canonical,
    transaction_index: usize,
    transaction_hash: B256,
    receipt_log_index: usize,
    block_log_index: usize,
) -> StoreResult<()> {
    encoder.usize(transaction_index)?;
    encoder.hash(transaction_hash);
    encoder.usize(receipt_log_index)?;
    encoder.usize(block_log_index)
}

macro_rules! encode_position {
    ($encoder:expr, $position:expr) => {{
        let position = $position;
        super::encode_position_fields(
            $encoder,
            position.transaction_index(),
            position.transaction_hash(),
            position.receipt_log_index(),
            position.block_log_index(),
        )
    }};
}

pub(super) use encode_position;
