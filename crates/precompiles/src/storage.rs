//! Zone precompile storage provider backed by finalized Tempo L1 state.
//!
//! Ordinary operations use the zone's local EVM state. Selected policy reads are overlaid from
//! the Tempo L1 block recorded in `TempoState`.
//!
//! # Read behavior
//!
//! - TIP-403 registry slots return the corresponding L1 value.
//! - TIP-20 transfer-policy slots replace only the L1-owned policy-ID field, preserving the
//!   remaining zone-local fields in the packed slot.
//! - All other slots return their zone-local value unchanged.
//!
//! Each mirrored read performs the local SLOAD first to preserve EVM warming, gas charging, and
//! storage-action accounting. Every L1 read during a precompile call uses the same block anchor.
//!
//! # Write behavior
//!
//! Persistent writes, increments, and decrements targeting mirrored state are rejected before
//! reaching the local EVM provider. Writes to all other slots delegate unchanged.

use alloc::format;

use crate::tempo_state::slots as tempo_state_slots;
use alloy_primitives::{Address, B256, LogData, U256};
use revm::{
    context::journaled_state::JournalCheckpoint,
    precompile::PrecompileError,
    state::{AccountInfo, Bytecode},
};
use tempo_chainspec::hardfork::TempoHardfork;
use tempo_contracts::precompiles::TIP403_REGISTRY_ADDRESS;
pub use tempo_precompiles::storage::*;
use tempo_precompiles::{
    error::{Result, TempoPrecompileError},
    storage::evm::EvmPrecompileStorageProvider,
    tip20::tip20_slots,
};
use tempo_primitives::{TempoAddressExt, TempoBlockEnv};
use zone_primitives::constants::TEMPO_STATE_ADDRESS;

/// L1 storage access needed by zone precompile storage overlays and `TempoState` reads.
pub trait L1StorageReader: Clone + Send + Sync + 'static {
    /// Read `account[slot]` at `block_number` on Tempo L1.
    fn read_l1_storage(
        &self,
        account: Address,
        slot: B256,
        block_number: u64,
    ) -> core::result::Result<B256, PrecompileError>;
}

/// Precompile storage that overlays finalized Tempo L1 policy state onto zone-local EVM state.
///
/// TIP-403 reads use L1 values, while TIP-20 policy reads replace only the policy-ID field.
/// Ordinary operations remain local, and persistent writes to mirrored state are rejected.
pub(crate) struct ZonePrecompileStorageProvider<'a, P> {
    inner: EvmPrecompileStorageProvider<'a>,
    l1_block_number: u64,
    l1: P,
}

impl<'a, P> ZonePrecompileStorageProvider<'a, P> {
    /// Wrap `inner` with an L1 reader bound to `l1_block_number` for this precompile call.
    pub(crate) fn new(
        inner: EvmPrecompileStorageProvider<'a>,
        l1: P,
        l1_block_number: u64,
    ) -> Self {
        Self {
            inner,
            l1,
            l1_block_number,
        }
    }
}

/// Read the finalized Tempo/L1 block number once before constructing the zone provider.
pub(crate) fn read_l1_anchor(inner: &mut EvmPrecompileStorageProvider<'_>) -> Result<u64> {
    let value = inner.sload(TEMPO_STATE_ADDRESS, tempo_state_slots::TEMPO_BLOCK_NUMBER)?;
    value.try_into().map_err(|_| {
        TempoPrecompileError::Fatal(format!(
            "invalid Tempo L1 block anchor (does not fit in u64): {value}"
        ))
    })
}

impl<P: L1StorageReader> ZonePrecompileStorageProvider<'_, P> {
    fn read_l1_slot(&self, address: Address, key: U256) -> Result<U256> {
        let block_number = self.l1_block_number;
        self.l1
            .read_l1_storage(address, key.into(), block_number)
            .map(|value| value.into())
            .map_err(|err| trace_err(err, address, key, block_number))
    }
}

impl<P: L1StorageReader> PrecompileStorageProvider for ZonePrecompileStorageProvider<'_, P> {
    fn chain_id(&self) -> u64 {
        self.inner.chain_id()
    }

    fn block_env(&self) -> &TempoBlockEnv {
        self.inner.block_env()
    }

    fn set_code(&mut self, address: Address, code: Bytecode) -> Result<()> {
        self.inner.set_code(address, code)
    }

    fn with_account_info(
        &mut self,
        address: Address,
        f: &mut dyn FnMut(&AccountInfo),
    ) -> Result<()> {
        self.inner.with_account_info(address, f)
    }

    fn sload(&mut self, address: Address, key: U256) -> Result<U256> {
        // Always perform the local SLOAD first so warming, gas, and storage-action accounting stay
        // identical to the normal EVM-backed provider. Mirrored slots replace only the observed
        // value returned to upstream TIP-20/TIP-403 logic.
        let local = self.inner.sload(address, key)?;
        if address == TIP403_REGISTRY_ADDRESS {
            return self.read_l1_slot(address, key);
        }

        // TODO(rusowsky): Remove once Tempo L1 stores transfer policy IDs in the TIP403 precompile.
        if is_tip20_policy_id_slot(address, key) {
            let l1 = self.read_l1_slot(address, key)?;
            return Ok(merge_transfer_policy_id(local, l1));
        }

        Ok(local)
    }

    fn tload(&mut self, address: Address, key: U256) -> Result<U256> {
        self.inner.tload(address, key)
    }

    fn sstore(&mut self, address: Address, key: U256, value: U256) -> Result<()> {
        if is_l1_slot(address, key) {
            return Err(l1_write_err(address, key));
        }
        self.inner.sstore(address, key, value)
    }

    fn sinc(&mut self, address: Address, key: U256, delta: U256) -> Result<()> {
        if is_l1_slot(address, key) {
            return Err(l1_write_err(address, key));
        }
        self.inner.sinc(address, key, delta)
    }

    fn sdec(&mut self, address: Address, key: U256, delta: U256) -> Result<()> {
        if is_l1_slot(address, key) {
            return Err(l1_write_err(address, key));
        }
        self.inner.sdec(address, key, delta)
    }

    fn tstore(&mut self, address: Address, key: U256, value: U256) -> Result<()> {
        self.inner.tstore(address, key, value)
    }

    fn emit_event(&mut self, address: Address, event: LogData) -> Result<()> {
        self.inner.emit_event(address, event)
    }

    fn deduct_gas(&mut self, gas: u64) -> Result<()> {
        self.inner.deduct_gas(gas)
    }

    fn refund_gas(&mut self, gas: i64) {
        self.inner.refund_gas(gas)
    }

    fn gas_limit(&self) -> u64 {
        self.inner.gas_limit()
    }

    fn gas_used(&self) -> u64 {
        self.inner.gas_used()
    }

    fn state_gas_used(&self) -> u64 {
        self.inner.state_gas_used()
    }

    fn gas_refunded(&self) -> i64 {
        self.inner.gas_refunded()
    }

    fn reservoir(&self) -> u64 {
        self.inner.reservoir()
    }

    fn spec(&self) -> TempoHardfork {
        self.inner.spec()
    }

    fn storage_actions(&self) -> StorageActions {
        self.inner.storage_actions()
    }

    fn amsterdam_eip8037_enabled(&self) -> bool {
        self.inner.amsterdam_eip8037_enabled()
    }

    fn is_static(&self) -> bool {
        self.inner.is_static()
    }

    fn checkpoint(&mut self) -> JournalCheckpoint {
        self.inner.checkpoint()
    }

    fn checkpoint_commit(&mut self, checkpoint: JournalCheckpoint) {
        self.inner.checkpoint_commit(checkpoint)
    }

    fn checkpoint_revert(&mut self, checkpoint: JournalCheckpoint) {
        self.inner.checkpoint_revert(checkpoint)
    }

    fn set_tip1060_storage_credits(&mut self, enabled: bool) {
        self.inner.set_tip1060_storage_credits(enabled)
    }

    fn set_tip1060_storage_credit_minting(&mut self, enabled: bool) {
        self.inner.set_tip1060_storage_credit_minting(enabled)
    }
}

// TODO(rusowsky): Remove TIP20 policy-slot detection, write protection, merge logic,
// and related tests once Tempo L1 migrates transfer policy IDs into TIP403.
fn is_l1_slot(address: Address, key: U256) -> bool {
    address == TIP403_REGISTRY_ADDRESS || is_tip20_policy_id_slot(address, key)
}

fn is_tip20_policy_id_slot(address: Address, key: U256) -> bool {
    address.is_tip20() && key == tip20_slots::TRANSFER_POLICY_ID
}

fn l1_write_err(address: Address, key: U256) -> TempoPrecompileError {
    TempoPrecompileError::Fatal(format!(
        "attempted to write mirrored Tempo L1 storage slot address={address} key={key}"
    ))
}

fn trace_err(
    err: PrecompileError,
    address: Address,
    key: U256,
    block_number: u64,
) -> TempoPrecompileError {
    let source = match err {
        PrecompileError::Fatal(msg) => msg,
        other => format!("{other:?}"),
    };
    TempoPrecompileError::Fatal(format!(
        "Tempo L1 storage read failed address={address} key={key} block={block_number}: {source}"
    ))
}

// TODO(rusowsky): Remove once Tempo L1 stores transfer policy IDs in the TIP403 precompile.
fn merge_transfer_policy_id(local_slot: U256, l1_slot: U256) -> U256 {
    let offset_bits = tip20_slots::TRANSFER_POLICY_ID_OFFSET * 8;
    let field_bits = core::mem::size_of::<u64>() * 8;
    let field_mask = ((U256::ONE << field_bits) - U256::ONE) << offset_bits;
    (local_slot & !field_mask) | (l1_slot & field_mask)
}

/// Convert a provider error into a precompile result using the provider's current gas counters.
pub(crate) fn storage_error_result(
    err: TempoPrecompileError,
    storage: &impl PrecompileStorageProvider,
) -> revm::precompile::PrecompileResult {
    err.into_precompile_result(storage.gas_used(), storage.reservoir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{MockL1Reader, TestCtx, test_context, test_storage_provider};
    use alloy_primitives::U256;
    use tempo_precompiles::PATH_USD_ADDRESS;

    fn with_zone_provider<T>(
        ctx: &mut TestCtx,
        l1: MockL1Reader,
        f: impl FnOnce(&mut ZonePrecompileStorageProvider<'_, MockL1Reader>) -> T,
    ) -> T {
        let mut inner = test_storage_provider(ctx, u64::MAX, false);
        inner
            .sstore(
                TEMPO_STATE_ADDRESS,
                tempo_state_slots::TEMPO_BLOCK_NUMBER,
                U256::from(123u64),
            )
            .expect("anchor write succeeds");
        let l1_block_number = read_l1_anchor(&mut inner).expect("anchor read succeeds");
        let mut provider = ZonePrecompileStorageProvider::new(inner, l1, l1_block_number);
        f(&mut provider)
    }

    #[test]
    fn read_l1_anchor_rejects_values_larger_than_u64() {
        let mut ctx = test_context();
        let mut inner = test_storage_provider(&mut ctx, u64::MAX, false);
        let oversized = U256::from(u64::MAX) + U256::ONE;
        inner
            .sstore(
                TEMPO_STATE_ADDRESS,
                tempo_state_slots::TEMPO_BLOCK_NUMBER,
                oversized,
            )
            .expect("anchor write succeeds");

        let err = read_l1_anchor(&mut inner).expect_err("oversized anchor must be rejected");
        assert!(
            matches!(err, TempoPrecompileError::Fatal(ref msg) if msg.contains("does not fit in u64") && msg.contains(&oversized.to_string()))
        );
    }

    #[test]
    fn fatal_l1_read_error_includes_storage_context() {
        let address = PATH_USD_ADDRESS;
        let key = tip20_slots::TRANSFER_POLICY_ID;
        let block_number = 123;
        let err = trace_err(
            PrecompileError::Fatal("RPC unavailable".into()),
            address,
            key,
            block_number,
        );

        assert!(matches!(
            err,
            TempoPrecompileError::Fatal(msg)
                if msg.contains("RPC unavailable")
                    && msg.contains(&address.to_string())
                    && msg.contains(&key.to_string())
                    && msg.contains("block=123")
        ));
    }

    #[test]
    fn sstore_sinc_sdec_reject_l1_slots() {
        let mut ctx = test_context();

        with_zone_provider(&mut ctx, MockL1Reader::default(), |provider| {
            let write_actions = [
                ZonePrecompileStorageProvider::sstore,
                ZonePrecompileStorageProvider::sinc,
                ZonePrecompileStorageProvider::sdec,
            ];
            let l1_slots = [
                (PATH_USD_ADDRESS, tip20_slots::TRANSFER_POLICY_ID),
                (TIP403_REGISTRY_ADDRESS, U256::ZERO),
            ];

            for action in write_actions {
                for (address, key) in l1_slots {
                    assert!(action(provider, address, key, U256::ONE).is_err());
                }
            }

            let (local_address, local_key) = (Address::random(), U256::random());
            for action in write_actions {
                assert!(action(provider, local_address, local_key, U256::ONE).is_ok())
            }
        });
    }

    #[test]
    fn sload_overlays_only_tip20_transfer_policy_field() {
        let mut ctx = test_context();
        let l1 = MockL1Reader::default();
        let offset_bits = tip20_slots::TRANSFER_POLICY_ID_OFFSET * 8;
        let local_low_bits = U256::from(0xdead_u64);
        let local_policy = U256::from(1u64) << offset_bits;
        let l1_policy = U256::from(99u64) << offset_bits;
        l1.set_u256(PATH_USD_ADDRESS, tip20_slots::TRANSFER_POLICY_ID, l1_policy);

        with_zone_provider(&mut ctx, l1, |provider| {
            provider
                .sstore(
                    PATH_USD_ADDRESS,
                    tip20_slots::TRANSFER_POLICY_ID + U256::from(100u64),
                    U256::from(7u64),
                )
                .expect("non-mirrored write succeeds");
            provider
                .inner
                .sstore(
                    PATH_USD_ADDRESS,
                    tip20_slots::TRANSFER_POLICY_ID,
                    local_low_bits | local_policy,
                )
                .expect("test local setup succeeds");
            let overlaid = provider
                .sload(PATH_USD_ADDRESS, tip20_slots::TRANSFER_POLICY_ID)
                .expect("overlaid sload succeeds");
            assert_eq!(overlaid & U256::from(0xffff_u64), local_low_bits);
            assert_eq!((overlaid >> offset_bits).to::<u64>(), 99);
        });
    }
}
