//! Exact post-block Zone state acquisition.

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::{Address, B256, U256};
use reth_storage_api::{
    StateProvider, StateProviderBox, StateProviderFactory, errors::provider::ProviderResult,
};

use crate::{
    model::state_layout::{
        ExactStateAccess, INBOX_PROCESSED_DEPOSIT_HASH_ACCESS,
        INBOX_PROCESSED_DEPOSIT_NUMBER_ACCESS, OUTBOX_LAST_BATCH_INDEX_ACCESS,
        OUTBOX_LAST_BATCH_QUEUE_HASH_ACCESS, TEMPO_BLOCK_HASH_ACCESS, TEMPO_BLOCK_NUMBER_ACCESS,
        decode_full_word_hash, decode_low_u64, tip20_total_supply_access,
    },
    observe::error::{AcquisitionError, AcquisitionSource, ObservationError},
};

/// Protocol commitments read from state after one exact Zone block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ZonePostStateOutputs {
    block_hash: B256,
    tempo_block_hash: B256,
    tempo_block_number: u64,
    processed_deposit_queue_hash: B256,
    processed_deposit_number: u64,
    withdrawal_queue_hash: B256,
    withdrawal_batch_index: u64,
    /// Exact supplies for the caller-supplied checker token set.
    token_supplies: BTreeMap<Address, U256>,
}

impl ZonePostStateOutputs {
    pub(crate) fn block_hash(&self) -> B256 {
        self.block_hash
    }

    pub(crate) fn tempo_block_hash(&self) -> B256 {
        self.tempo_block_hash
    }

    pub(crate) fn tempo_block_number(&self) -> u64 {
        self.tempo_block_number
    }

    pub(crate) fn processed_deposit_queue_hash(&self) -> B256 {
        self.processed_deposit_queue_hash
    }

    pub(crate) fn processed_deposit_number(&self) -> u64 {
        self.processed_deposit_number
    }

    pub(crate) fn withdrawal_queue_hash(&self) -> B256 {
        self.withdrawal_queue_hash
    }

    pub(crate) fn withdrawal_batch_index(&self) -> u64 {
        self.withdrawal_batch_index
    }

    pub(crate) fn token_supplies(&self) -> &BTreeMap<Address, U256> {
        &self.token_supplies
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        block_hash: B256,
        tempo_block_hash: B256,
        tempo_block_number: u64,
        processed_deposit_queue_hash: B256,
        processed_deposit_number: u64,
        withdrawal_queue_hash: B256,
        withdrawal_batch_index: u64,
    ) -> Self {
        Self {
            block_hash,
            tempo_block_hash,
            tempo_block_number,
            processed_deposit_queue_hash,
            processed_deposit_number,
            withdrawal_queue_hash,
            withdrawal_batch_index,
            token_supplies: BTreeMap::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_block_hash_for_test(mut self, block_hash: B256) -> Self {
        self.block_hash = block_hash;
        self
    }

    #[cfg(test)]
    pub(crate) fn processed_cursor_for_test(&self) -> (B256, u64) {
        (
            self.processed_deposit_queue_hash,
            self.processed_deposit_number,
        )
    }

    #[cfg(test)]
    pub(crate) fn withdrawal_cursor_for_test(&self) -> (B256, u64) {
        (self.withdrawal_queue_hash, self.withdrawal_batch_index)
    }
}

/// Narrow exact-hash lookup used by the observation adapter.
///
/// Keeping `latest` and number/tag lookups out of this interface makes an
/// accidental fallback impossible inside [`acquire_zone_post_state`].
pub(crate) trait ExactStateLookup {
    fn state_by_exact_block_hash(&self, block_hash: B256) -> ProviderResult<StateProviderBox>;
}

impl<P: StateProviderFactory + ?Sized> ExactStateLookup for P {
    fn state_by_exact_block_hash(&self, block_hash: B256) -> ProviderResult<StateProviderBox> {
        self.state_by_block_hash(block_hash)
    }
}

/// Acquire protocol outputs from the state selected by `block_hash` exactly.
///
/// The caller owns the token set because the checker model, rather than node
/// implementation state, defines which token supplies must be reconciled.
pub(crate) fn acquire_zone_post_state<P: ExactStateLookup + ?Sized>(
    provider: &P,
    block_hash: B256,
    tokens: &[Address],
) -> Result<ZonePostStateOutputs, ObservationError> {
    let state = provider
        .state_by_exact_block_hash(block_hash)
        .map_err(|error| AcquisitionError::unavailable(AcquisitionSource::ExactZoneState, error))?;

    require_accounts(state.as_ref(), block_hash, tokens)?;

    let token_supplies = tokens
        .iter()
        .copied()
        .map(|token| {
            read_storage(state.as_ref(), tip20_total_supply_access(token))
                .map(|total_supply| (token, total_supply))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;

    Ok(ZonePostStateOutputs {
        block_hash,
        tempo_block_hash: decode_full_word_hash(read_storage(
            state.as_ref(),
            TEMPO_BLOCK_HASH_ACCESS,
        )?),
        tempo_block_number: decode_low_u64(read_storage(
            state.as_ref(),
            TEMPO_BLOCK_NUMBER_ACCESS,
        )?),
        processed_deposit_queue_hash: decode_full_word_hash(read_storage(
            state.as_ref(),
            INBOX_PROCESSED_DEPOSIT_HASH_ACCESS,
        )?),
        processed_deposit_number: decode_low_u64(read_storage(
            state.as_ref(),
            INBOX_PROCESSED_DEPOSIT_NUMBER_ACCESS,
        )?),
        withdrawal_queue_hash: decode_full_word_hash(read_storage(
            state.as_ref(),
            OUTBOX_LAST_BATCH_QUEUE_HASH_ACCESS,
        )?),
        withdrawal_batch_index: decode_low_u64(read_storage(
            state.as_ref(),
            OUTBOX_LAST_BATCH_INDEX_ACCESS,
        )?),
        token_supplies,
    })
}

fn require_accounts(
    state: &dyn StateProvider,
    block_hash: B256,
    tokens: &[Address],
) -> Result<(), ObservationError> {
    let fixed_accounts = [
        TEMPO_BLOCK_HASH_ACCESS.address,
        INBOX_PROCESSED_DEPOSIT_HASH_ACCESS.address,
        OUTBOX_LAST_BATCH_QUEUE_HASH_ACCESS.address,
    ];
    let mut required_accounts = BTreeSet::new();
    required_accounts.extend(fixed_accounts);
    required_accounts.extend(tokens.iter().copied());

    for address in required_accounts {
        let account = state.basic_account(&address).map_err(|error| {
            AcquisitionError::unavailable(AcquisitionSource::ExactZoneState, error)
        })?;
        if account.is_none() {
            return Err(AcquisitionError::missing(
                AcquisitionSource::ExactZoneState,
                format!("{address} at {block_hash}"),
            )
            .into());
        }
    }

    Ok(())
}

fn read_storage(
    state: &dyn StateProvider,
    access: ExactStateAccess,
) -> Result<U256, ObservationError> {
    let value = state
        .storage(access.address, access.storage_key())
        .map_err(|error| AcquisitionError::unavailable(AcquisitionSource::ExactZoneState, error))?;
    Ok(canonical_evm_storage_value(value))
}

/// A storage-trie omission means the slot's canonical EVM value is zero.
///
/// Callers must first establish that the containing account exists. This is a
/// consensus interpretation of an unwritten slot, not a missing-data fallback.
fn canonical_evm_storage_value(value: Option<U256>) -> U256 {
    match value {
        Some(value) => value,
        None => U256::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use alloy_primitives::{address, b256, uint};
    use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
    use tempo_primitives::TempoPrimitives;

    use super::*;

    type TestProvider = MockEthProvider<TempoPrimitives>;

    struct RecordingExactState {
        expected_hash: B256,
        requested_hashes: Mutex<Vec<B256>>,
        state: TestProvider,
    }

    impl ExactStateLookup for RecordingExactState {
        fn state_by_exact_block_hash(&self, block_hash: B256) -> ProviderResult<StateProviderBox> {
            assert_eq!(block_hash, self.expected_hash);
            self.requested_hashes.lock().unwrap().push(block_hash);
            Ok(Box::new(self.state.clone()))
        }
    }

    struct UnavailableExactState;

    impl ExactStateLookup for UnavailableExactState {
        fn state_by_exact_block_hash(&self, block_hash: B256) -> ProviderResult<StateProviderBox> {
            Err(reth_storage_api::errors::provider::ProviderError::StateForHashNotFound(block_hash))
        }
    }

    fn account_with_storage(storage: impl IntoIterator<Item = (B256, U256)>) -> ExtendedAccount {
        ExtendedAccount::new(0, U256::ZERO).extend_storage(storage)
    }

    fn add_empty_fixed_accounts(provider: &TestProvider) {
        for address in [
            TEMPO_BLOCK_HASH_ACCESS.address,
            INBOX_PROCESSED_DEPOSIT_HASH_ACCESS.address,
            OUTBOX_LAST_BATCH_QUEUE_HASH_ACCESS.address,
        ] {
            provider.add_account(address, account_with_storage([]));
        }
    }

    #[test]
    fn acquires_all_fixed_outputs_and_caller_selected_supplies() {
        let provider = TestProvider::new();
        let block_hash = b256!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let tempo_hash_word =
            uint!(0x101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f_U256);
        let deposit_hash_word =
            uint!(0x303132333435363738393a3b3c3d3e3f404142434445464748494a4b4c4d4e4f_U256);
        let batch_hash_word =
            uint!(0x505152535455565758595a5b5c5d5e5f606162636465666768696a6b6c6d6e6f_U256);
        let token_a = address!("20c00000000000000000000000000000000000aa");
        let token_b = address!("20c00000000000000000000000000000000000bb");

        provider.add_account(
            TEMPO_BLOCK_HASH_ACCESS.address,
            account_with_storage([
                (TEMPO_BLOCK_HASH_ACCESS.storage_key(), tempo_hash_word),
                (
                    TEMPO_BLOCK_NUMBER_ACCESS.storage_key(),
                    uint!(0xdeadbeef0000000000000000000000000000000000000000000000000000002a_U256),
                ),
            ]),
        );
        provider.add_account(
            INBOX_PROCESSED_DEPOSIT_HASH_ACCESS.address,
            account_with_storage([
                (
                    INBOX_PROCESSED_DEPOSIT_HASH_ACCESS.storage_key(),
                    deposit_hash_word,
                ),
                (
                    INBOX_PROCESSED_DEPOSIT_NUMBER_ACCESS.storage_key(),
                    uint!(0xabcdef000000000000000000000000000000000000000000000000000000002b_U256),
                ),
            ]),
        );
        provider.add_account(
            OUTBOX_LAST_BATCH_QUEUE_HASH_ACCESS.address,
            account_with_storage([
                (
                    OUTBOX_LAST_BATCH_QUEUE_HASH_ACCESS.storage_key(),
                    batch_hash_word,
                ),
                (
                    OUTBOX_LAST_BATCH_INDEX_ACCESS.storage_key(),
                    uint!(0x123456780000000000000000000000000000000000000000000000000000002c_U256),
                ),
            ]),
        );
        provider.add_account(
            token_a,
            account_with_storage([(
                tip20_total_supply_access(token_a).storage_key(),
                U256::from(1_000_000_u64),
            )]),
        );
        provider.add_account(
            token_b,
            account_with_storage([(
                tip20_total_supply_access(token_b).storage_key(),
                U256::from(2_000_000_u64),
            )]),
        );

        let exact = RecordingExactState {
            expected_hash: block_hash,
            requested_hashes: Mutex::new(Vec::new()),
            state: provider,
        };
        let outputs = acquire_zone_post_state(&exact, block_hash, &[token_b, token_a]).unwrap();

        assert_eq!(*exact.requested_hashes.lock().unwrap(), vec![block_hash]);

        assert_eq!(
            outputs,
            ZonePostStateOutputs {
                block_hash,
                tempo_block_hash: b256!(
                    "101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f"
                ),
                tempo_block_number: 42,
                processed_deposit_queue_hash: b256!(
                    "303132333435363738393a3b3c3d3e3f404142434445464748494a4b4c4d4e4f"
                ),
                processed_deposit_number: 43,
                withdrawal_queue_hash: b256!(
                    "505152535455565758595a5b5c5d5e5f606162636465666768696a6b6c6d6e6f"
                ),
                withdrawal_batch_index: 44,
                token_supplies: BTreeMap::from([
                    (token_b, U256::from(2_000_000_u64)),
                    (token_a, U256::from(1_000_000_u64)),
                ]),
            }
        );
    }

    #[test]
    fn exact_state_lookup_failure_is_unavailable() {
        assert!(matches!(
            acquire_zone_post_state(&UnavailableExactState, B256::repeat_byte(0xee), &[]),
            Err(ObservationError::Acquisition(
                AcquisitionError::Unavailable {
                    kind: AcquisitionSource::ExactZoneState,
                    ..
                }
            ))
        ));
    }

    #[test]
    fn absent_protocol_or_token_account_is_retryable_acquisition_failure() {
        let block_hash = b256!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

        let missing_protocol =
            acquire_zone_post_state(&TestProvider::new(), block_hash, &[]).unwrap_err();
        assert!(matches!(
            missing_protocol,
            ObservationError::Acquisition(AcquisitionError::Missing {
                kind: AcquisitionSource::ExactZoneState,
                ..
            })
        ));

        let provider = TestProvider::new();
        add_empty_fixed_accounts(&provider);
        let missing_token = address!("20c00000000000000000000000000000000000cc");
        let error = acquire_zone_post_state(&provider, block_hash, &[missing_token]).unwrap_err();
        match error {
            ObservationError::Acquisition(error) => match error {
                AcquisitionError::Missing {
                    kind: AcquisitionSource::ExactZoneState,
                    identity,
                } => {
                    assert!(identity.contains(&missing_token.to_string()));
                    assert!(identity.contains(&block_hash.to_string()));
                }
                other => panic!("expected missing-account error, got {other:?}"),
            },
            other => panic!("expected missing-account acquisition error, got {other:?}"),
        }
    }

    #[test]
    fn unwritten_slots_of_existing_accounts_are_canonical_evm_zero() {
        let provider = TestProvider::new();
        add_empty_fixed_accounts(&provider);
        let token = address!("20c00000000000000000000000000000000000dd");
        provider.add_account(token, account_with_storage([]));

        let outputs =
            acquire_zone_post_state(&provider, B256::repeat_byte(0xcc), &[token]).unwrap();

        assert_eq!(outputs.tempo_block_hash, B256::ZERO);
        assert_eq!(outputs.tempo_block_number, 0);
        assert_eq!(outputs.processed_deposit_queue_hash, B256::ZERO);
        assert_eq!(outputs.processed_deposit_number, 0);
        assert_eq!(outputs.withdrawal_queue_hash, B256::ZERO);
        assert_eq!(outputs.withdrawal_batch_index, 0);
        assert_eq!(
            outputs.token_supplies,
            BTreeMap::from([(token, U256::ZERO)])
        );
    }
}
