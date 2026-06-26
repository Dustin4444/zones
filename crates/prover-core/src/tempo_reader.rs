use alloc::vec::Vec;

use alloy_primitives::{Address, Bytes};
use alloy_sol_types::{SolCall, SolError};
use tempo_zone_contracts::TempoStateReader;
use zone_primitives::constants::TEMPO_STATE_ADDRESS;

use crate::{ProverError, TempoWitnessProvider};

pub const TEMPO_STATE_READER_BASE_GAS: u64 = 200;
pub const TEMPO_STATE_READER_PER_SLOT_GAS: u64 = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TempoStateReaderCallResult {
    Returned { gas_used: u64, output: Bytes },
    Reverted { output: Bytes },
}

#[derive(Debug, Clone, Copy)]
pub struct WitnessTempoStateReader<'a> {
    provider: &'a TempoWitnessProvider,
    zone_block_index: u64,
}

impl<'a> WitnessTempoStateReader<'a> {
    pub const fn new(provider: &'a TempoWitnessProvider, zone_block_index: u64) -> Self {
        Self {
            provider,
            zone_block_index,
        }
    }

    pub fn call(
        &self,
        caller: Address,
        is_direct_call: bool,
        data: &[u8],
    ) -> Result<TempoStateReaderCallResult, ProverError> {
        if !is_direct_call {
            return Ok(revert(
                TempoStateReader::DelegateCallNotAllowed {}.abi_encode(),
            ));
        }

        if caller != TEMPO_STATE_ADDRESS {
            return Ok(revert(TempoStateReader::Unauthorized {}.abi_encode()));
        }

        let Some(selector) = selector(data) else {
            return Ok(revert(Bytes::new()));
        };

        if selector == TempoStateReader::readStorageAtCall::SELECTOR {
            self.read_storage_at(data)
        } else if selector == TempoStateReader::readStorageBatchAtCall::SELECTOR {
            self.read_storage_batch_at(data)
        } else {
            Ok(revert(Bytes::new()))
        }
    }

    fn read_storage_at(&self, data: &[u8]) -> Result<TempoStateReaderCallResult, ProverError> {
        let call = match TempoStateReader::readStorageAtCall::abi_decode(data) {
            Ok(call) => call,
            Err(_) => return Ok(revert(Bytes::new())),
        };

        let value = self.provider.read_storage_at(
            self.zone_block_index,
            call.blockNumber,
            call.account,
            call.slot,
        )?;
        Ok(TempoStateReaderCallResult::Returned {
            gas_used: tempo_state_reader_gas(1)?,
            output: TempoStateReader::readStorageAtCall::abi_encode_returns(&value).into(),
        })
    }

    fn read_storage_batch_at(
        &self,
        data: &[u8],
    ) -> Result<TempoStateReaderCallResult, ProverError> {
        let call = match TempoStateReader::readStorageBatchAtCall::abi_decode(data) {
            Ok(call) => call,
            Err(_) => return Ok(revert(Bytes::new())),
        };

        let mut values = Vec::with_capacity(call.slots.len());
        for slot in &call.slots {
            values.push(self.provider.read_storage_at(
                self.zone_block_index,
                call.blockNumber,
                call.account,
                *slot,
            )?);
        }

        Ok(TempoStateReaderCallResult::Returned {
            gas_used: tempo_state_reader_gas(call.slots.len())?,
            output: TempoStateReader::readStorageBatchAtCall::abi_encode_returns(&values).into(),
        })
    }
}

pub fn tempo_state_reader_gas(slot_count: usize) -> Result<u64, ProverError> {
    let slot_count =
        u64::try_from(slot_count).map_err(|_| ProverError::TempoStateReaderGasOverflow)?;
    let slot_gas = TEMPO_STATE_READER_PER_SLOT_GAS
        .checked_mul(slot_count)
        .ok_or(ProverError::TempoStateReaderGasOverflow)?;
    TEMPO_STATE_READER_BASE_GAS
        .checked_add(slot_gas)
        .ok_or(ProverError::TempoStateReaderGasOverflow)
}

fn selector(data: &[u8]) -> Option<[u8; 4]> {
    data.get(..4)?.try_into().ok()
}

fn revert(output: impl Into<Bytes>) -> TempoStateReaderCallResult {
    TempoStateReaderCallResult::Reverted {
        output: output.into(),
    }
}

#[cfg(test)]
mod tests {
    use alloc::{collections::BTreeMap, vec};

    use alloy_primitives::{B256, U256, address, b256};

    use super::*;
    use crate::{TempoStateReadKey, TempoWitnessProvider};

    fn provider_for(
        reads: impl IntoIterator<Item = (TempoStateReadKey, B256)>,
    ) -> TempoWitnessProvider {
        TempoWitnessProvider {
            reads: reads
                .into_iter()
                .map(|(key, value)| (key, U256::from_be_bytes(value.0)))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn read_key(
        zone_block_index: u64,
        tempo_block_number: u64,
        account: Address,
        slot: B256,
    ) -> TempoStateReadKey {
        TempoStateReadKey {
            zone_block_index,
            tempo_block_number,
            account,
            slot: slot.into(),
        }
    }

    #[test]
    fn reads_single_slot_from_witness_provider() {
        let account = address!("0x0000000000000000000000000000000000001000");
        let slot = b256!("0000000000000000000000000000000000000000000000000000000000000007");
        let value = B256::repeat_byte(0xaa);
        let provider = provider_for([(read_key(2, 100, account, slot), value)]);
        let reader = WitnessTempoStateReader::new(&provider, 2);
        let calldata = TempoStateReader::readStorageAtCall {
            account,
            slot,
            blockNumber: 100,
        }
        .abi_encode();

        let result = reader
            .call(TEMPO_STATE_ADDRESS, true, &calldata)
            .expect("proved read must succeed");

        assert_eq!(
            result,
            TempoStateReaderCallResult::Returned {
                gas_used: tempo_state_reader_gas(1).unwrap(),
                output: TempoStateReader::readStorageAtCall::abi_encode_returns(&value).into(),
            }
        );
    }

    #[test]
    fn reads_batch_slots_from_witness_provider_in_order() {
        let account = address!("0x0000000000000000000000000000000000001000");
        let first_slot = b256!("0000000000000000000000000000000000000000000000000000000000000001");
        let second_slot = b256!("0000000000000000000000000000000000000000000000000000000000000002");
        let first_value = B256::repeat_byte(0x11);
        let second_value = B256::repeat_byte(0x22);
        let provider = provider_for([
            (read_key(3, 101, account, first_slot), first_value),
            (read_key(3, 101, account, second_slot), second_value),
        ]);
        let reader = WitnessTempoStateReader::new(&provider, 3);
        let calldata = TempoStateReader::readStorageBatchAtCall {
            account,
            slots: vec![first_slot, second_slot],
            blockNumber: 101,
        }
        .abi_encode();

        let result = reader
            .call(TEMPO_STATE_ADDRESS, true, &calldata)
            .expect("proved reads must succeed");

        assert_eq!(
            result,
            TempoStateReaderCallResult::Returned {
                gas_used: tempo_state_reader_gas(2).unwrap(),
                output: TempoStateReader::readStorageBatchAtCall::abi_encode_returns(&vec![
                    first_value,
                    second_value,
                ])
                .into(),
            }
        );
    }

    #[test]
    fn missing_witness_read_fails_closed() {
        let account = address!("0x0000000000000000000000000000000000001000");
        let slot = b256!("0000000000000000000000000000000000000000000000000000000000000007");
        let provider = provider_for([]);
        let reader = WitnessTempoStateReader::new(&provider, 2);
        let calldata = TempoStateReader::readStorageAtCall {
            account,
            slot,
            blockNumber: 100,
        }
        .abi_encode();

        assert_eq!(
            reader
                .call(TEMPO_STATE_ADDRESS, true, &calldata)
                .unwrap_err(),
            ProverError::MissingTempoStateRead {
                zone_block_index: 2,
                tempo_block_number: 100,
                account,
                slot: slot.into(),
            }
        );
    }

    #[test]
    fn unauthorized_or_delegate_calls_revert_like_live_precompile() {
        let provider = provider_for([]);
        let reader = WitnessTempoStateReader::new(&provider, 0);
        let caller = address!("0x0000000000000000000000000000000000009999");

        assert_eq!(
            reader.call(TEMPO_STATE_ADDRESS, false, &[]).unwrap(),
            TempoStateReaderCallResult::Reverted {
                output: TempoStateReader::DelegateCallNotAllowed {}
                    .abi_encode()
                    .into(),
            }
        );
        assert_eq!(
            reader.call(caller, true, &[]).unwrap(),
            TempoStateReaderCallResult::Reverted {
                output: TempoStateReader::Unauthorized {}.abi_encode().into(),
            }
        );
        assert_eq!(
            reader.call(TEMPO_STATE_ADDRESS, true, &[0xff]).unwrap(),
            TempoStateReaderCallResult::Reverted {
                output: Bytes::new(),
            }
        );
    }
}
