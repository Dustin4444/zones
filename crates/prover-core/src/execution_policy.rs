use alloc::format;

use alloy_primitives::{Address, U256};
use revm::precompile::PrecompileError;
use tempo_contracts::precompiles::ITIP403Registry::PolicyType;
use tempo_precompiles::{
    storage::packing::extract_from_word,
    tip403_registry::{ALLOW_ALL_POLICY_ID, REJECT_ALL_POLICY_ID},
};
use zone_precompiles::{SequencerExt, policy::PolicyCheck};
use zone_primitives::policy::AuthRole;

use crate::{OwnedWitnessTempoStateReader, ProverError};

/// `TIP20Token.transfer_policy_id` in Tempo's storage layout.
///
/// The field is packed in slot 7 after `next_quote_token`:
/// `next_quote_token: address` at byte offset 0, followed by
/// `transfer_policy_id: uint64` at byte offset 20.
pub const TIP20_TRANSFER_POLICY_ID_SLOT: U256 = U256::from_limbs([7, 0, 0, 0]);
const TIP20_TRANSFER_POLICY_ID_OFFSET: usize = 20;
const TIP20_TRANSFER_POLICY_ID_BYTES: usize = 8;

#[derive(Debug, Clone)]
pub struct WitnessPolicyProvider {
    tempo_state_reader: OwnedWitnessTempoStateReader,
    tempo_block_number: u64,
}

impl WitnessPolicyProvider {
    pub const fn new(
        tempo_state_reader: OwnedWitnessTempoStateReader,
        tempo_block_number: u64,
    ) -> Self {
        Self {
            tempo_state_reader,
            tempo_block_number,
        }
    }

    fn prover_error(err: ProverError) -> PrecompileError {
        PrecompileError::Fatal(format!("{err}"))
    }

    fn missing_policy(policy_id: u64) -> PrecompileError {
        PrecompileError::Fatal(format!(
            "missing witness-backed TIP-403 policy data for policy {policy_id}"
        ))
    }
}

impl PolicyCheck for WitnessPolicyProvider {
    fn is_authorized(
        &self,
        policy_id: u64,
        _user: Address,
        _role: AuthRole,
    ) -> Result<bool, PrecompileError> {
        match policy_id {
            REJECT_ALL_POLICY_ID => Ok(false),
            ALLOW_ALL_POLICY_ID => Ok(true),
            _ => Err(Self::missing_policy(policy_id)),
        }
    }

    fn resolve_transfer_policy_id(&self, token: Address) -> Result<u64, PrecompileError> {
        let word = self
            .tempo_state_reader
            .read_storage_word(
                self.tempo_block_number,
                token,
                TIP20_TRANSFER_POLICY_ID_SLOT,
            )
            .map_err(Self::prover_error)?;

        extract_from_word::<u64>(
            word,
            TIP20_TRANSFER_POLICY_ID_OFFSET,
            TIP20_TRANSFER_POLICY_ID_BYTES,
        )
        .map_err(|err| {
            PrecompileError::Fatal(format!(
                "invalid packed TIP-20 transfer policy id for token {token}: {err}"
            ))
        })
    }

    fn policy_type_sync(&self, policy_id: u64) -> Result<PolicyType, PrecompileError> {
        match policy_id {
            REJECT_ALL_POLICY_ID => Ok(PolicyType::WHITELIST),
            ALLOW_ALL_POLICY_ID => Ok(PolicyType::BLACKLIST),
            _ => Err(Self::missing_policy(policy_id)),
        }
    }

    fn compound_policy_data(&self, policy_id: u64) -> Result<(u64, u64, u64), PrecompileError> {
        Err(Self::missing_policy(policy_id))
    }

    fn policy_exists(&self, policy_id: u64) -> Result<bool, PrecompileError> {
        match policy_id {
            REJECT_ALL_POLICY_ID | ALLOW_ALL_POLICY_ID => Ok(true),
            _ => Err(Self::missing_policy(policy_id)),
        }
    }

    fn policy_id_counter(&self) -> u64 {
        2
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WitnessSequencer {
    sequencer: Address,
}

impl WitnessSequencer {
    pub const fn new(sequencer: Address) -> Self {
        Self { sequencer }
    }
}

impl SequencerExt for WitnessSequencer {
    fn latest_sequencer(&self) -> Option<Address> {
        Some(self.sequencer)
    }
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeMap;

    use alloy_primitives::address;
    use tempo_precompiles::storage::packing::insert_into_word;

    use super::*;
    use crate::{OwnedWitnessTempoStateReader, TempoStateReadKey, TempoWitnessProvider};

    const TEMPO_BLOCK_NUMBER: u64 = 100;
    const ZONE_BLOCK_INDEX: u64 = 0;

    fn token() -> Address {
        address!("0x20c00000000000000000000000000000000000aa")
    }

    fn policy_word(policy_id: u64) -> U256 {
        insert_into_word(
            U256::ZERO,
            &policy_id,
            TIP20_TRANSFER_POLICY_ID_OFFSET,
            TIP20_TRANSFER_POLICY_ID_BYTES,
        )
        .expect("policy id should pack")
    }

    fn provider_with_policy(policy_id: u64) -> WitnessPolicyProvider {
        let reads = BTreeMap::from([(
            TempoStateReadKey {
                zone_block_index: ZONE_BLOCK_INDEX,
                tempo_block_number: TEMPO_BLOCK_NUMBER,
                account: token(),
                slot: TIP20_TRANSFER_POLICY_ID_SLOT,
            },
            policy_word(policy_id),
        )]);
        WitnessPolicyProvider::new(
            OwnedWitnessTempoStateReader::new(TempoWitnessProvider { reads }, ZONE_BLOCK_INDEX),
            TEMPO_BLOCK_NUMBER,
        )
    }

    #[test]
    fn resolves_proved_builtin_transfer_policy_id() {
        let provider = provider_with_policy(ALLOW_ALL_POLICY_ID);

        assert_eq!(
            provider.resolve_transfer_policy_id(token()).unwrap(),
            ALLOW_ALL_POLICY_ID
        );
        assert!(
            provider
                .is_authorized(ALLOW_ALL_POLICY_ID, Address::ZERO, AuthRole::Transfer)
                .unwrap()
        );
        assert!(
            !provider
                .is_authorized(REJECT_ALL_POLICY_ID, Address::ZERO, AuthRole::Transfer)
                .unwrap()
        );
    }

    #[test]
    fn missing_l1_policy_read_fails_closed() {
        let provider = WitnessPolicyProvider::new(
            OwnedWitnessTempoStateReader::new(
                TempoWitnessProvider {
                    reads: BTreeMap::new(),
                },
                ZONE_BLOCK_INDEX,
            ),
            TEMPO_BLOCK_NUMBER,
        );

        assert!(provider.resolve_transfer_policy_id(token()).is_err());
    }

    #[test]
    fn non_builtin_policy_authorization_fails_closed() {
        let provider = provider_with_policy(2);

        assert_eq!(provider.resolve_transfer_policy_id(token()).unwrap(), 2);
        assert!(
            provider
                .is_authorized(2, Address::ZERO, AuthRole::Transfer)
                .is_err()
        );
        assert!(provider.policy_exists(2).is_err());
        assert!(provider.compound_policy_data(2).is_err());
    }

    #[test]
    fn witness_sequencer_returns_public_input_sequencer() {
        let sequencer = address!("0x00000000000000000000000000000000000000a1");

        assert_eq!(
            WitnessSequencer::new(sequencer).latest_sequencer(),
            Some(sequencer)
        );
    }
}
