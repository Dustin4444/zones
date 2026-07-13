use alloc::format;

use alloy_primitives::{Address, LogData, U256};
use revm::{
    context::journaled_state::JournalCheckpoint,
    precompile::PrecompileError,
    state::{AccountInfo, Bytecode},
};
use tempo_chainspec::hardfork::TempoHardfork;
use tempo_precompiles::storage::PrecompileStorageProvider;
use tempo_primitives::TempoBlockEnv;

use super::L1StorageReader;

/// Minimal Tempo precompile storage context bound to one raw L1 block.
///
/// Supported operations limited to:
///
/// - Persistent storage reads (`sload`) are delegated to [`L1StorageReader`] at the bound block.
/// - [`PrecompileStorageProvider::spec`] reports the hardfork resolved for that same block.
///
/// Persistent and transient writes, code changes, account access, transient reads, and event
/// emission fail with a fatal read-only error. Gas and checkpoint operations are inert because
/// evaluation is read-only and non-EVM. The block environment and chain ID are also inert and must
/// not be used by callers evaluating against this provider.
pub(super) struct ReadOnlyL1Storage<'a, R> {
    reader: &'a R,
    block_number: u64,
    spec: TempoHardfork,
    block_env: TempoBlockEnv,
}

impl<'a, R> ReadOnlyL1Storage<'a, R> {
    pub(super) fn new(reader: &'a R, block_number: u64, spec: TempoHardfork) -> Self {
        Self {
            reader,
            block_number,
            spec,
            block_env: TempoBlockEnv::default(),
        }
    }
}

impl<R: L1StorageReader> PrecompileStorageProvider for ReadOnlyL1Storage<'_, R> {
    fn chain_id(&self) -> u64 {
        0
    }

    fn block_env(&self) -> &TempoBlockEnv {
        &self.block_env
    }

    fn set_code(&mut self, _address: Address, _code: Bytecode) -> tempo_precompiles::Result<()> {
        Err(read_only_error("set_code"))
    }

    fn with_account_info(
        &mut self,
        _address: Address,
        _f: &mut dyn FnMut(&AccountInfo),
    ) -> tempo_precompiles::Result<()> {
        Err(read_only_error("with_account_info"))
    }

    fn sload(&mut self, address: Address, key: U256) -> tempo_precompiles::Result<U256> {
        self.reader
            .read_l1_storage(address, key.into(), self.block_number)
            .map(Into::into)
            .map_err(reader_error)
    }

    fn tload(&mut self, _address: Address, _key: U256) -> tempo_precompiles::Result<U256> {
        Err(read_only_error("tload"))
    }

    fn sstore(
        &mut self,
        _address: Address,
        _key: U256,
        _value: U256,
    ) -> tempo_precompiles::Result<()> {
        Err(read_only_error("sstore"))
    }

    fn sinc(
        &mut self,
        _address: Address,
        _key: U256,
        _delta: U256,
    ) -> tempo_precompiles::Result<()> {
        Err(read_only_error("sinc"))
    }

    fn sdec(
        &mut self,
        _address: Address,
        _key: U256,
        _delta: U256,
    ) -> tempo_precompiles::Result<()> {
        Err(read_only_error("sdec"))
    }

    fn tstore(
        &mut self,
        _address: Address,
        _key: U256,
        _value: U256,
    ) -> tempo_precompiles::Result<()> {
        Err(read_only_error("tstore"))
    }

    fn emit_event(&mut self, _address: Address, _event: LogData) -> tempo_precompiles::Result<()> {
        Err(read_only_error("emit_event"))
    }

    fn deduct_gas(&mut self, _gas: u64) -> tempo_precompiles::Result<()> {
        Ok(())
    }

    fn refund_gas(&mut self, _gas: i64) {}

    fn gas_limit(&self) -> u64 {
        u64::MAX
    }

    fn gas_used(&self) -> u64 {
        0
    }

    fn state_gas_used(&self) -> u64 {
        0
    }

    fn gas_refunded(&self) -> i64 {
        0
    }

    fn reservoir(&self) -> u64 {
        0
    }

    fn spec(&self) -> TempoHardfork {
        self.spec
    }

    fn amsterdam_eip8037_enabled(&self) -> bool {
        false
    }

    fn is_static(&self) -> bool {
        true
    }

    fn checkpoint(&mut self) -> JournalCheckpoint {
        JournalCheckpoint::default()
    }

    fn checkpoint_commit(&mut self, _checkpoint: JournalCheckpoint) {}

    fn checkpoint_revert(&mut self, _checkpoint: JournalCheckpoint) {}

    fn set_tip1060_storage_credits(&mut self, _enabled: bool) {}

    fn set_tip1060_storage_credit_minting(&mut self, _enabled: bool) {}
}

fn read_only_error(operation: &str) -> tempo_precompiles::error::TempoPrecompileError {
    tempo_precompiles::error::TempoPrecompileError::Fatal(format!(
        "{operation} is not available during read-only L1 evaluation"
    ))
}

pub(super) fn reader_error(err: PrecompileError) -> tempo_precompiles::error::TempoPrecompileError {
    let message = match err {
        PrecompileError::Fatal(message) => message,
        other => format!("{other:?}"),
    };
    tempo_precompiles::error::TempoPrecompileError::Fatal(message)
}
