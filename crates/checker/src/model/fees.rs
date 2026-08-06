//! Checker-owned fee arithmetic.

use alloy_primitives::U256;

use super::{
    constants::{BOUNCE_BACK_BASE_FEE_SCALE, WITHDRAWAL_BASE_GAS},
    encoding::WithdrawalGasLimit,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum FeeError {
    #[error("fee arithmetic overflow")]
    Overflow,
}

/// Derive `(50_000 + gas_limit) * tempo_gas_rate` with checked `u128`
/// multiplication. [`WithdrawalGasLimit`] owns the pinned admission bound.
pub(crate) fn withdrawal_fee(
    gas_limit: WithdrawalGasLimit,
    tempo_gas_rate: u128,
) -> Result<u128, FeeError> {
    let gas = u128::from(WITHDRAWAL_BASE_GAS)
        .checked_add(u128::from(gas_limit.get()))
        .ok_or(FeeError::Overflow)?;
    gas.checked_mul(tempo_gas_rate).ok_or(FeeError::Overflow)
}

/// Derive `min(ceil(bounceback_gas * block_base_fee / 10^12), amount)`.
///
/// All intermediates remain `U256`; multiplication and the rounding addition
/// are checked before the amount cap is applied.
pub(crate) fn bounce_back_fee(
    bounceback_gas: u64,
    block_base_fee: U256,
    withdrawal_amount: u128,
) -> Result<u128, FeeError> {
    let scale = U256::from(BOUNCE_BACK_BASE_FEE_SCALE);
    let gas_fee = U256::from(bounceback_gas)
        .checked_mul(block_base_fee)
        .ok_or(FeeError::Overflow)?;
    let rounded = gas_fee
        .checked_add(scale - U256::ONE)
        .ok_or(FeeError::Overflow)?
        / scale;
    let capped = rounded.min(U256::from(withdrawal_amount));
    Ok(capped.to::<u128>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{constants::MAX_WITHDRAWAL_GAS_LIMIT, encoding::WithdrawalDataError};

    fn gas(value: u64) -> WithdrawalGasLimit {
        WithdrawalGasLimit::new(value).unwrap()
    }

    #[test]
    fn model_withdrawal_fee_fixed_boundaries_and_overflow() {
        assert_eq!(withdrawal_fee(gas(0), 0), Ok(0));
        assert_eq!(withdrawal_fee(gas(0), 2), Ok(100_000));
        assert_eq!(
            withdrawal_fee(gas(MAX_WITHDRAWAL_GAS_LIMIT), 3),
            Ok(30_150_000)
        );
        assert_eq!(
            WithdrawalGasLimit::new(MAX_WITHDRAWAL_GAS_LIMIT + 1),
            Err(WithdrawalDataError::GasLimitTooHigh {
                actual: 10_000_001,
                maximum: 10_000_000,
            })
        );

        let exact_rate = u128::MAX / u128::from(WITHDRAWAL_BASE_GAS);
        assert_eq!(
            withdrawal_fee(gas(0), exact_rate),
            Ok(u128::from(WITHDRAWAL_BASE_GAS) * exact_rate)
        );
        assert_eq!(
            withdrawal_fee(gas(0), exact_rate + 1),
            Err(FeeError::Overflow)
        );
    }

    #[test]
    fn model_bounce_back_fee_rounding_zero_and_cap_vectors() {
        let scale = U256::from(BOUNCE_BACK_BASE_FEE_SCALE);

        assert_eq!(bounce_back_fee(0, scale, 100), Ok(0));
        assert_eq!(bounce_back_fee(1, scale, 100), Ok(1));
        assert_eq!(bounce_back_fee(1, scale + U256::ONE, 100), Ok(2));
        assert_eq!(bounce_back_fee(3, scale / U256::from(2), 100), Ok(2));
        assert_eq!(bounce_back_fee(100, scale, 7), Ok(7));
    }

    #[test]
    fn model_bounce_back_fee_uses_same_block_gas_updates_in_order() {
        let base_fee = U256::from(500_000_000_000_u64);
        let amount = 100;

        // First process call precedes the update; the second follows it in the
        // same authenticated Tempo event stream.
        assert_eq!(bounce_back_fee(2, base_fee, amount), Ok(1));
        assert_eq!(bounce_back_fee(3, base_fee, amount), Ok(2));
    }

    #[test]
    fn model_bounce_back_fee_checks_u256_intermediates() {
        assert_eq!(
            bounce_back_fee(2, U256::MAX, u128::MAX),
            Err(FeeError::Overflow)
        );
        assert_eq!(
            bounce_back_fee(1, U256::MAX, u128::MAX),
            Err(FeeError::Overflow),
            "rounding addition must also be checked"
        );
    }
}
