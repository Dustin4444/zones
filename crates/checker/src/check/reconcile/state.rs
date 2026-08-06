//! Exact collateral, commitment, and supply comparisons.

use alloy_primitives::{Address, U256};

use crate::{
    check::finding::{Finding, FixedStateFinding},
    model::{output::ExpectedPostZoneState, state::TokenState},
    observe::ZonePostStateOutputs,
};

pub(in crate::check) fn reconcile_collateral(
    token: Address,
    state: &TokenState,
    actual: U256,
) -> Result<(), Finding> {
    let required = state
        .accounting()
        .collateral_requirement()
        .map_err(crate::model::transition::ModelError::from)?;
    if actual < required {
        return Err(Finding::CollateralDeficit {
            token,
            required,
            actual,
        });
    }
    Ok(())
}

pub(in crate::check) fn reconcile_post_zone_state<'a>(
    expected: ExpectedPostZoneState,
    expected_supplies: impl IntoIterator<Item = (Address, &'a TokenState)>,
    actual: &ZonePostStateOutputs,
) -> Result<(), Finding> {
    require_equal(
        expected.tempo_block_hash(),
        actual.tempo_block_hash(),
        |expected, actual| FixedStateFinding::TempoBlockHash { expected, actual },
    )?;
    require_equal(
        expected.tempo_block_number(),
        actual.tempo_block_number(),
        |expected, actual| FixedStateFinding::TempoBlockNumber { expected, actual },
    )?;
    require_equal(
        expected.processed_deposit().hash(),
        actual.processed_deposit_queue_hash(),
        |expected, actual| FixedStateFinding::ProcessedDepositHash { expected, actual },
    )?;
    require_equal(
        expected.processed_deposit().number(),
        actual.processed_deposit_number(),
        |expected, actual| FixedStateFinding::ProcessedDepositNumber { expected, actual },
    )?;
    require_equal(
        expected.last_batch().withdrawal_queue_hash(),
        actual.withdrawal_queue_hash(),
        |expected, actual| FixedStateFinding::WithdrawalQueueHash { expected, actual },
    )?;
    require_equal(
        expected.last_batch().withdrawal_batch_index(),
        actual.withdrawal_batch_index(),
        |expected, actual| FixedStateFinding::WithdrawalBatchIndex { expected, actual },
    )?;

    for (token, state) in expected_supplies {
        if !state.is_zone_enabled() {
            continue;
        }
        let actual = actual
            .token_supplies()
            .get(&token)
            .copied()
            .ok_or(Finding::MissingSupply { token })?;
        let expected = state.accounting().supply;
        if expected != actual {
            return Err(Finding::SupplyMismatch {
                token,
                expected,
                actual,
            });
        }
    }
    Ok(())
}

fn require_equal<T: PartialEq>(
    expected: T,
    actual: T,
    finding: impl FnOnce(T, T) -> FixedStateFinding,
) -> Result<(), Finding> {
    if expected != actual {
        return Err(finding(expected, actual).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, B256, U256, address, b256};

    use super::*;
    use crate::{
        model::{
            accounting::{AccountingError, Component, TokenAccounting},
            state::{TokenPhase, ZoneLastBatch, ZoneProcessedDepositCursor},
            transition::ModelError,
        },
        observe::ZonePostStateOutputs,
    };

    const TOKEN: Address = address!("20c0000000000000000000000000000000000001");
    const TEMPO_HASH: B256 =
        b256!("1111111111111111111111111111111111111111111111111111111111111111");
    const PROCESSED_HASH: B256 =
        b256!("2222222222222222222222222222222222222222222222222222222222222222");
    const WITHDRAWAL_HASH: B256 =
        b256!("3333333333333333333333333333333333333333333333333333333333333333");
    const OTHER_HASH: B256 =
        b256!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    #[derive(Clone, Copy)]
    struct Commitments {
        tempo_hash: B256,
        tempo_number: u64,
        processed_hash: B256,
        processed_number: u64,
        withdrawal_hash: B256,
        withdrawal_batch_index: u64,
    }

    impl Commitments {
        const EXPECTED: Self = Self {
            tempo_hash: TEMPO_HASH,
            tempo_number: 11,
            processed_hash: PROCESSED_HASH,
            processed_number: 22,
            withdrawal_hash: WITHDRAWAL_HASH,
            withdrawal_batch_index: 33,
        };

        fn expected(self) -> ExpectedPostZoneState {
            ExpectedPostZoneState::new(
                self.tempo_hash,
                self.tempo_number,
                ZoneProcessedDepositCursor::new(self.processed_hash, self.processed_number),
                ZoneLastBatch::for_test(self.withdrawal_hash, self.withdrawal_batch_index),
            )
        }

        fn actual(self) -> ZonePostStateOutputs {
            ZonePostStateOutputs::for_test(
                self.tempo_hash,
                self.tempo_number,
                self.processed_hash,
                self.processed_number,
                self.withdrawal_hash,
                self.withdrawal_batch_index,
            )
        }
    }

    fn enabled_token(supply: U256, deposit: U256, withdrawal: U256) -> TokenState {
        TokenState::for_test(
            TokenPhase::ZoneEnabled,
            TokenAccounting {
                supply,
                deposit_liability: deposit,
                withdrawal_liability: withdrawal,
            },
        )
    }

    #[test]
    fn collateral_accepts_the_checked_sdw_requirement_and_surplus() {
        let state = enabled_token(U256::from(10), U256::from(20), U256::from(30));

        assert!(reconcile_collateral(TOKEN, &state, U256::from(60)).is_ok());
        assert!(reconcile_collateral(TOKEN, &state, U256::from(61)).is_ok());
    }

    #[test]
    fn collateral_reports_a_deficit_against_the_checked_sdw_requirement() {
        let state = enabled_token(U256::from(10), U256::from(20), U256::from(30));

        let error = reconcile_collateral(TOKEN, &state, U256::from(59)).unwrap_err();
        assert!(matches!(
            error,
            Finding::CollateralDeficit {
                token: TOKEN,
                required,
                actual,
            } if required == U256::from(60) && actual == U256::from(59)
        ));
    }

    #[test]
    fn collateral_requirement_overflow_is_a_model_finding() {
        let state = enabled_token(U256::MAX, U256::ONE, U256::ZERO);

        assert!(matches!(
            reconcile_collateral(TOKEN, &state, U256::MAX),
            Err(Finding::Model(ModelError::Accounting(
                AccountingError::Overflow(Component::CollateralRequirement)
            )))
        ));
    }

    #[test]
    fn each_fixed_state_commitment_has_a_distinct_finding() {
        let expected = Commitments::EXPECTED;
        let cases = [
            (
                Commitments {
                    tempo_hash: OTHER_HASH,
                    ..expected
                },
                FixedStateFinding::TempoBlockHash {
                    expected: TEMPO_HASH,
                    actual: OTHER_HASH,
                },
            ),
            (
                Commitments {
                    tempo_number: 12,
                    ..expected
                },
                FixedStateFinding::TempoBlockNumber {
                    expected: 11,
                    actual: 12,
                },
            ),
            (
                Commitments {
                    processed_hash: OTHER_HASH,
                    ..expected
                },
                FixedStateFinding::ProcessedDepositHash {
                    expected: PROCESSED_HASH,
                    actual: OTHER_HASH,
                },
            ),
            (
                Commitments {
                    processed_number: 23,
                    ..expected
                },
                FixedStateFinding::ProcessedDepositNumber {
                    expected: 22,
                    actual: 23,
                },
            ),
            (
                Commitments {
                    withdrawal_hash: OTHER_HASH,
                    ..expected
                },
                FixedStateFinding::WithdrawalQueueHash {
                    expected: WITHDRAWAL_HASH,
                    actual: OTHER_HASH,
                },
            ),
            (
                Commitments {
                    withdrawal_batch_index: 34,
                    ..expected
                },
                FixedStateFinding::WithdrawalBatchIndex {
                    expected: 33,
                    actual: 34,
                },
            ),
        ];

        for (actual, expected_finding) in cases {
            let error = reconcile_post_zone_state(
                expected.expected(),
                std::iter::empty(),
                &actual.actual(),
            )
            .unwrap_err();
            assert!(matches!(error, Finding::FixedState(finding) if finding == expected_finding));
        }
    }

    #[test]
    fn exact_state_and_enabled_token_supply_match() {
        let state = enabled_token(U256::from(70), U256::ZERO, U256::ZERO);
        let actual = Commitments::EXPECTED
            .actual()
            .with_token_supply_for_test(TOKEN, U256::from(70));

        assert!(matches!(
            reconcile_post_zone_state(Commitments::EXPECTED.expected(), [(TOKEN, &state)], &actual),
            Ok(())
        ));
    }

    #[test]
    fn exact_state_reports_supply_mismatch() {
        let state = enabled_token(U256::from(70), U256::ZERO, U256::ZERO);
        let actual = Commitments::EXPECTED
            .actual()
            .with_token_supply_for_test(TOKEN, U256::from(71));

        assert!(matches!(
            reconcile_post_zone_state(
                Commitments::EXPECTED.expected(),
                [(TOKEN, &state)],
                &actual,
            ),
            Err(Finding::SupplyMismatch {
                token: TOKEN,
                expected,
                actual,
            }) if expected == U256::from(70) && actual == U256::from(71)
        ));
    }

    #[test]
    fn exact_state_reports_missing_enabled_token_supply() {
        let state = enabled_token(U256::from(70), U256::ZERO, U256::ZERO);

        assert!(matches!(
            reconcile_post_zone_state(
                Commitments::EXPECTED.expected(),
                [(TOKEN, &state)],
                &Commitments::EXPECTED.actual(),
            ),
            Err(Finding::MissingSupply { token: TOKEN })
        ));
    }
}
