use alloc::format;

use alloy_primitives::{Address, U256};
use revm::precompile::PrecompileError;
use tempo_contracts::precompiles::{ITIP403Registry::PolicyType, TIP403_REGISTRY_ADDRESS};
use tempo_precompiles::{
    storage::packing::extract_from_word,
    tip403_registry::{ALLOW_ALL_POLICY_ID, REJECT_ALL_POLICY_ID},
};
use zone_precompiles::{SequencerExt, policy::PolicyCheck};
use zone_primitives::{
    policy::AuthRole,
    tip403::{
        POLICY_ID_COUNTER_SLOT, POLICY_TYPE_BLACKLIST, POLICY_TYPE_COMPOUND, POLICY_TYPE_WHITELIST,
        Tip403PolicyData, decode_compound_policy_data, decode_policy_data, policy_record_base_slot,
        policy_record_compound_slot, policy_set_account_slot,
    },
};

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

    fn policy_not_found(policy_id: u64) -> PrecompileError {
        PrecompileError::Fatal(format!(
            "witness-backed TIP-403 policy {policy_id} does not exist"
        ))
    }

    fn invalid_policy_type(policy_id: u64, policy_type: u8) -> PrecompileError {
        PrecompileError::Fatal(format!(
            "invalid witness-backed TIP-403 policy type {policy_type} for policy {policy_id}"
        ))
    }

    fn incompatible_policy_type(policy_id: u64) -> PrecompileError {
        PrecompileError::Fatal(format!(
            "witness-backed TIP-403 policy {policy_id} has an incompatible policy type"
        ))
    }

    fn read_registry_word(&self, slot: U256) -> Result<U256, PrecompileError> {
        self.tempo_state_reader
            .read_storage_word(self.tempo_block_number, TIP403_REGISTRY_ADDRESS, slot)
            .map_err(Self::prover_error)
    }

    fn read_policy_counter(&self) -> Result<u64, PrecompileError> {
        Ok(self
            .read_registry_word(POLICY_ID_COUNTER_SLOT)?
            .to::<u64>()
            .max(2))
    }

    fn read_policy_data(&self, policy_id: u64) -> Result<Tip403PolicyData, PrecompileError> {
        let data = decode_policy_data(self.read_registry_word(policy_record_base_slot(policy_id))?);

        if data.policy_type == POLICY_TYPE_WHITELIST
            && data.admin.is_zero()
            && policy_id >= self.read_policy_counter()?
        {
            return Err(Self::policy_not_found(policy_id));
        }

        Ok(data)
    }

    fn policy_type_from_raw(
        policy_id: u64,
        policy_type: u8,
    ) -> Result<PolicyType, PrecompileError> {
        match policy_type {
            POLICY_TYPE_WHITELIST => Ok(PolicyType::WHITELIST),
            POLICY_TYPE_BLACKLIST => Ok(PolicyType::BLACKLIST),
            POLICY_TYPE_COMPOUND => Ok(PolicyType::COMPOUND),
            _ => Err(Self::invalid_policy_type(policy_id, policy_type)),
        }
    }

    fn is_authorized_simple(&self, policy_id: u64, user: Address) -> Result<bool, PrecompileError> {
        match policy_id {
            REJECT_ALL_POLICY_ID => return Ok(false),
            ALLOW_ALL_POLICY_ID => return Ok(true),
            _ => {}
        }

        let data = self.read_policy_data(policy_id)?;
        let is_in_set = !self
            .read_registry_word(policy_set_account_slot(policy_id, user))?
            .is_zero();

        match data.policy_type {
            POLICY_TYPE_WHITELIST => Ok(is_in_set),
            POLICY_TYPE_BLACKLIST => Ok(!is_in_set),
            POLICY_TYPE_COMPOUND => Err(Self::incompatible_policy_type(policy_id)),
            other => Err(Self::invalid_policy_type(policy_id, other)),
        }
    }
}

impl PolicyCheck for WitnessPolicyProvider {
    fn is_authorized(
        &self,
        policy_id: u64,
        user: Address,
        role: AuthRole,
    ) -> Result<bool, PrecompileError> {
        match policy_id {
            REJECT_ALL_POLICY_ID => Ok(false),
            ALLOW_ALL_POLICY_ID => Ok(true),
            _ => {
                let data = self.read_policy_data(policy_id)?;
                if data.policy_type != POLICY_TYPE_COMPOUND {
                    return self.is_authorized_simple(policy_id, user);
                }

                let compound = decode_compound_policy_data(
                    self.read_registry_word(policy_record_compound_slot(policy_id))?,
                );
                match role {
                    AuthRole::Sender => self.is_authorized_simple(compound.sender_policy_id, user),
                    AuthRole::Recipient => {
                        self.is_authorized_simple(compound.recipient_policy_id, user)
                    }
                    AuthRole::MintRecipient => {
                        self.is_authorized_simple(compound.mint_recipient_policy_id, user)
                    }
                    AuthRole::Transfer => {
                        let sender_ok =
                            self.is_authorized_simple(compound.sender_policy_id, user)?;
                        if !sender_ok {
                            return Ok(false);
                        }
                        self.is_authorized_simple(compound.recipient_policy_id, user)
                    }
                }
            }
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
            _ => {
                let data = self.read_policy_data(policy_id)?;
                Self::policy_type_from_raw(policy_id, data.policy_type)
            }
        }
    }

    fn compound_policy_data(&self, policy_id: u64) -> Result<(u64, u64, u64), PrecompileError> {
        let data = self.read_policy_data(policy_id)?;
        if data.policy_type != POLICY_TYPE_COMPOUND {
            return Err(Self::incompatible_policy_type(policy_id));
        }

        let compound = decode_compound_policy_data(
            self.read_registry_word(policy_record_compound_slot(policy_id))?,
        );
        Ok((
            compound.sender_policy_id,
            compound.recipient_policy_id,
            compound.mint_recipient_policy_id,
        ))
    }

    fn policy_exists(&self, policy_id: u64) -> Result<bool, PrecompileError> {
        match policy_id {
            REJECT_ALL_POLICY_ID | ALLOW_ALL_POLICY_ID => Ok(true),
            _ => Ok(policy_id < self.read_policy_counter()?),
        }
    }

    fn policy_id_counter(&self) -> Result<u64, PrecompileError> {
        self.read_policy_counter()
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
    use zone_primitives::tip403::{
        POLICY_TYPE_BLACKLIST, POLICY_TYPE_COMPOUND, POLICY_TYPE_WHITELIST,
        encode_compound_policy_data, encode_policy_data,
    };

    use super::*;
    use crate::{OwnedWitnessTempoStateReader, TempoStateReadKey, TempoWitnessProvider};

    const TEMPO_BLOCK_NUMBER: u64 = 100;
    const ZONE_BLOCK_INDEX: u64 = 0;

    fn token() -> Address {
        address!("0x20c00000000000000000000000000000000000aa")
    }

    fn admin() -> Address {
        address!("0x00000000000000000000000000000000000000ad")
    }

    fn user() -> Address {
        address!("0x00000000000000000000000000000000000000b0")
    }

    fn other_user() -> Address {
        address!("0x00000000000000000000000000000000000000b1")
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
        provider_with_reads(BTreeMap::from([(
            TempoStateReadKey {
                zone_block_index: ZONE_BLOCK_INDEX,
                tempo_block_number: TEMPO_BLOCK_NUMBER,
                account: token(),
                slot: TIP20_TRANSFER_POLICY_ID_SLOT,
            },
            policy_word(policy_id),
        )]))
    }

    fn provider_with_reads(reads: BTreeMap<TempoStateReadKey, U256>) -> WitnessPolicyProvider {
        WitnessPolicyProvider::new(
            OwnedWitnessTempoStateReader::new(TempoWitnessProvider { reads }, ZONE_BLOCK_INDEX),
            TEMPO_BLOCK_NUMBER,
        )
    }

    fn read_key(account: Address, slot: U256) -> TempoStateReadKey {
        TempoStateReadKey {
            zone_block_index: ZONE_BLOCK_INDEX,
            tempo_block_number: TEMPO_BLOCK_NUMBER,
            account,
            slot,
        }
    }

    fn registry_key(slot: U256) -> TempoStateReadKey {
        read_key(TIP403_REGISTRY_ADDRESS, slot)
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
    fn witness_backed_whitelist_policy_authorizes_listed_account() {
        let policy_id = 2;
        let reads = BTreeMap::from([
            (
                read_key(token(), TIP20_TRANSFER_POLICY_ID_SLOT),
                policy_word(policy_id),
            ),
            (registry_key(POLICY_ID_COUNTER_SLOT), U256::from(3)),
            (
                registry_key(policy_record_base_slot(policy_id)),
                encode_policy_data(POLICY_TYPE_WHITELIST, admin()),
            ),
            (
                registry_key(policy_set_account_slot(policy_id, user())),
                U256::ONE,
            ),
            (
                registry_key(policy_set_account_slot(policy_id, other_user())),
                U256::ZERO,
            ),
        ]);
        let provider = provider_with_reads(reads);

        assert_eq!(
            provider.resolve_transfer_policy_id(token()).unwrap(),
            policy_id
        );
        assert!(provider.policy_exists(policy_id).unwrap());
        assert_eq!(
            provider.policy_type_sync(policy_id).unwrap(),
            PolicyType::WHITELIST
        );
        assert!(
            provider
                .is_authorized(policy_id, user(), AuthRole::Transfer)
                .unwrap()
        );
        assert!(
            !provider
                .is_authorized(policy_id, other_user(), AuthRole::Transfer)
                .unwrap()
        );
    }

    #[test]
    fn witness_backed_blacklist_policy_rejects_listed_account() {
        let policy_id = 2;
        let reads = BTreeMap::from([
            (registry_key(POLICY_ID_COUNTER_SLOT), U256::from(3)),
            (
                registry_key(policy_record_base_slot(policy_id)),
                encode_policy_data(POLICY_TYPE_BLACKLIST, admin()),
            ),
            (
                registry_key(policy_set_account_slot(policy_id, user())),
                U256::ONE,
            ),
            (
                registry_key(policy_set_account_slot(policy_id, other_user())),
                U256::ZERO,
            ),
        ]);
        let provider = provider_with_reads(reads);

        assert!(
            !provider
                .is_authorized(policy_id, user(), AuthRole::Transfer)
                .unwrap()
        );
        assert!(
            provider
                .is_authorized(policy_id, other_user(), AuthRole::Transfer)
                .unwrap()
        );
    }

    #[test]
    fn witness_backed_compound_policy_uses_role_subpolicies() {
        let sender_policy = 2;
        let recipient_policy = 3;
        let mint_policy = 4;
        let compound_policy = 5;
        let reads = BTreeMap::from([
            (registry_key(POLICY_ID_COUNTER_SLOT), U256::from(6)),
            (
                registry_key(policy_record_base_slot(sender_policy)),
                encode_policy_data(POLICY_TYPE_WHITELIST, admin()),
            ),
            (
                registry_key(policy_record_base_slot(recipient_policy)),
                encode_policy_data(POLICY_TYPE_BLACKLIST, admin()),
            ),
            (
                registry_key(policy_record_base_slot(mint_policy)),
                encode_policy_data(POLICY_TYPE_WHITELIST, admin()),
            ),
            (
                registry_key(policy_record_base_slot(compound_policy)),
                encode_policy_data(POLICY_TYPE_COMPOUND, admin()),
            ),
            (
                registry_key(policy_record_compound_slot(compound_policy)),
                encode_compound_policy_data(sender_policy, recipient_policy, mint_policy),
            ),
            (
                registry_key(policy_set_account_slot(sender_policy, user())),
                U256::ONE,
            ),
            (
                registry_key(policy_set_account_slot(recipient_policy, user())),
                U256::ZERO,
            ),
            (
                registry_key(policy_set_account_slot(mint_policy, user())),
                U256::ONE,
            ),
        ]);
        let provider = provider_with_reads(reads);

        assert_eq!(
            provider.compound_policy_data(compound_policy).unwrap(),
            (sender_policy, recipient_policy, mint_policy)
        );
        assert!(
            provider
                .is_authorized(compound_policy, user(), AuthRole::Sender)
                .unwrap()
        );
        assert!(
            provider
                .is_authorized(compound_policy, user(), AuthRole::Recipient)
                .unwrap()
        );
        assert!(
            provider
                .is_authorized(compound_policy, user(), AuthRole::MintRecipient)
                .unwrap()
        );
        assert!(
            provider
                .is_authorized(compound_policy, user(), AuthRole::Transfer)
                .unwrap()
        );
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
