//! Mandatory privacy policy for account-indexed Permit2 getters.
//!
//! Direct transaction calls are rejected during Zone transaction validation. Nested `CALL` and
//! `STATICCALL` frames are guarded in the instruction table, where denied calls execute a tiny
//! `Unauthorized()` revert program instead of Permit2 bytecode. This avoids both RPC-only gaps and
//! the per-opcode overhead of enabling REVM's tracing inspector in consensus execution. Sequencer
//! access is derived from the current block beneficiary, which is committed in the Zone header.

use crate::database::L1OverlayDB;
use alloy_evm::Database;
use alloy_primitives::{Address, Bytes, TxKind, keccak256};
use alloy_sol_types::{SolCall, SolError};
use revm::{
    bytecode::{
        Bytecode,
        opcode::{CALL, STATICCALL},
    },
    interpreter::{
        FrameInput, Instruction, InstructionContext, InstructionResult, InterpreterAction,
        instructions::contract::call as revm_call, interpreter::EthInterpreter,
        interpreter_types::LoopControl,
    },
};
use tempo_contracts::{PERMIT2_ADDRESS, Permit2};
use tempo_evm::evm::TempoEvm;
use tempo_revm::{TempoInvalidTransaction, TempoTxEnv, evm::TempoContext};
use tempo_zone_contracts::Unauthorized;
use zone_precompiles::L1StorageReader;

type ZoneInstructionCtx<'a, DB, L1> =
    InstructionContext<'a, TempoContext<L1OverlayDB<DB, L1>>, EthInterpreter>;

pub(super) fn configure_runtime<DB, L1, I>(evm: &mut TempoEvm<L1OverlayDB<DB, L1>, I>)
where
    DB: Database,
    L1: L1StorageReader,
{
    let instructions = &mut evm.inner_mut().inner.instruction;
    let call_gas = instructions.gas_table()[CALL as usize];
    let staticcall_gas = instructions.gas_table()[STATICCALL as usize];
    instructions.insert_instruction(CALL, Instruction::new(call::<CALL, DB, L1>), call_gas);
    instructions.insert_instruction(
        STATICCALL,
        Instruction::new(call::<STATICCALL, DB, L1>),
        staticcall_gas,
    );
}

pub(super) fn validate_transaction(
    tx: &TempoTxEnv,
    current_sequencer: Address,
) -> Result<(), TempoInvalidTransaction> {
    for (kind, data) in tx.calls() {
        let TxKind::Call(target) = kind else {
            continue;
        };
        validate_call(tx.caller, *target, data, current_sequencer)?;
    }
    Ok(())
}

pub(super) fn validate_call(
    caller: Address,
    target: Address,
    data: &[u8],
    current_sequencer: Address,
) -> Result<(), TempoInvalidTransaction> {
    if target == PERMIT2_ADDRESS
        && permit2_authorized_accounts(data).is_some_and(|accounts| {
            !accounts.contains(&caller)
                && (current_sequencer == Address::ZERO || caller != current_sequencer)
        })
    {
        return Err(TempoInvalidTransaction::CallsValidation(
            "unauthorized Permit2 account read",
        ));
    }
    Ok(())
}

fn call<const KIND: u8, DB, L1>(
    context: ZoneInstructionCtx<'_, DB, L1>,
) -> Result<(), InstructionResult>
where
    DB: Database,
    L1: L1StorageReader,
{
    let result =
        revm_call::<KIND, EthInterpreter, TempoContext<L1OverlayDB<DB, L1>>>(InstructionContext {
            interpreter: context.interpreter,
            host: context.host,
        });
    if result != Err(InstructionResult::Suspend) {
        return result;
    }

    let Some((caller, data)) = pending_permit2_call(context.interpreter) else {
        return result;
    };
    let Some(accounts) = permit2_authorized_accounts(&data) else {
        return result;
    };
    let current_sequencer = context.host.block.beneficiary;
    if accounts.contains(&caller)
        || (current_sequencer != Address::ZERO && caller == current_sequencer)
    {
        return result;
    }

    if let Some(InterpreterAction::NewFrame(FrameInput::Call(inputs))) =
        context.interpreter.bytecode.action().as_mut()
    {
        inputs.known_bytecode = unauthorized_bytecode();
    }
    result
}

fn pending_permit2_call(
    interpreter: &mut revm::interpreter::Interpreter<EthInterpreter>,
) -> Option<(Address, Bytes)> {
    let (bytecode, memory) = (&mut interpreter.bytecode, &interpreter.memory);
    let Some(InterpreterAction::NewFrame(FrameInput::Call(inputs))) = bytecode.action().as_mut()
    else {
        return None;
    };
    if inputs.target_address != PERMIT2_ADDRESS {
        return None;
    }
    let data = inputs.input.as_bytes_memory(memory);
    Some((inputs.caller, Bytes::copy_from_slice(&data)))
}

fn unauthorized_bytecode() -> (alloy_primitives::B256, Bytecode) {
    // PUSH4 Unauthorized.selector; PUSH1 0; MSTORE; PUSH1 4; PUSH1 28; REVERT
    let mut bytes = Vec::with_capacity(13);
    bytes.push(0x63);
    bytes.extend_from_slice(&Unauthorized::SELECTOR);
    bytes.extend_from_slice(&[0x60, 0x00, 0x52, 0x60, 0x04, 0x60, 0x1c, 0xfd]);
    let bytes = Bytes::from(bytes);
    (keccak256(&bytes), Bytecode::new_raw(bytes))
}

fn permit2_authorized_accounts(data: &[u8]) -> Option<Vec<Address>> {
    let (selector, args) = (data.get(..4)?, data.get(4..)?);
    if selector == Permit2::allowanceCall::SELECTOR {
        let call = Permit2::allowanceCall::abi_decode_raw(args).ok()?;
        Some(vec![call._0, call._2])
    } else if selector == Permit2::nonceBitmapCall::SELECTOR {
        let call = Permit2::nonceBitmapCall::abi_decode_raw(args).ok()?;
        Some(vec![call._0])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_evm::EvmEnv;
    use alloy_primitives::U256;
    use revm::{database::EmptyDB, inspector::NoOpInspector};
    use tempo_evm::TempoBlockEnv;
    use zone_precompiles::test_utils::MockL1Reader;

    #[test]
    fn runtime_preserves_call_opcode_base_gas() {
        let db = L1OverlayDB::new(EmptyDB::default(), MockL1Reader::default(), Address::ZERO);
        let mut evm = TempoEvm::<_, NoOpInspector>::new(
            db,
            EvmEnv::<tempo_chainspec::hardfork::TempoHardfork, TempoBlockEnv>::default(),
        );
        let before = {
            let gas = evm.inner_mut().inner.instruction.gas_table();
            (gas[CALL as usize], gas[STATICCALL as usize])
        };

        configure_runtime(&mut evm);

        let gas = evm.inner_mut().inner.instruction.gas_table();
        assert_eq!((gas[CALL as usize], gas[STATICCALL as usize]), before);
    }

    #[test]
    fn extracts_only_privacy_bearing_permit2_getters() {
        let owner = Address::repeat_byte(0x11);
        let token = Address::repeat_byte(0x22);
        let spender = Address::repeat_byte(0x33);

        assert_eq!(
            permit2_authorized_accounts(
                &Permit2::allowanceCall {
                    _0: owner,
                    _1: token,
                    _2: spender,
                }
                .abi_encode()
            ),
            Some(vec![owner, spender])
        );
        assert_eq!(
            permit2_authorized_accounts(
                &Permit2::nonceBitmapCall {
                    _0: owner,
                    _1: U256::from(7),
                }
                .abi_encode()
            ),
            Some(vec![owner])
        );
        assert_eq!(
            permit2_authorized_accounts(&Permit2::DOMAIN_SEPARATORCall {}.abi_encode()),
            None
        );
    }

    #[test]
    fn noncanonical_address_padding_cannot_change_authorization_subject() {
        let owner = Address::repeat_byte(0x11);
        let mut data = Permit2::nonceBitmapCall {
            _0: owner,
            _1: U256::ZERO,
        }
        .abi_encode();
        data[4] = 1;

        assert!(Permit2::nonceBitmapCall::abi_decode_raw_validate(&data[4..]).is_err());
        assert_eq!(permit2_authorized_accounts(&data), Some(vec![owner]));
    }

    #[test]
    fn zero_beneficiary_does_not_authorize_zero_caller() {
        let owner = Address::repeat_byte(0x11);
        let data = Permit2::nonceBitmapCall {
            _0: owner,
            _1: U256::ZERO,
        }
        .abi_encode();

        assert!(validate_call(Address::ZERO, PERMIT2_ADDRESS, &data, Address::ZERO).is_err());
    }
}
