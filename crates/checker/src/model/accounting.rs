//! Exact checker-owned `S/D/W` transition table.

use alloy_primitives::U256;

/// Expected Zone supply (`S`), deposit-origin liability (`D`), and
/// user-withdrawal liability (`W`) for one enabled token.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TokenAccounting {
    pub(crate) supply: U256,
    pub(crate) deposit_liability: U256,
    pub(crate) withdrawal_liability: U256,
}

impl TokenAccounting {
    pub(crate) const ZERO: Self = Self {
        supply: U256::ZERO,
        deposit_liability: U256::ZERO,
        withdrawal_liability: U256::ZERO,
    };

    /// Minimum Portal balance required at the post-L1/pre-Zone cut.
    ///
    /// This is intentionally a direct checked fold over the three authoritative
    /// accounting components, not an entry in a generic invariant registry.
    pub(crate) fn collateral_requirement(self) -> Result<U256, AccountingError> {
        self.supply
            .checked_add(self.deposit_liability)
            .and_then(|total| total.checked_add(self.withdrawal_liability))
            .ok_or(AccountingError::Overflow(Component::CollateralRequirement))
    }

    /// Apply one post-enablement row from DESIGN section 5.5 with checked
    /// arithmetic. Enablement itself is handled by [`apply_token_accounting`]
    /// because map presence, not zero aggregate values, proves uniqueness.
    fn apply_enabled(self, transition: AccountingTransition) -> Result<Self, AccountingError> {
        use AccountingTransition::*;

        let mut next = self;
        match transition {
            TokenEnabled => unreachable!("enablement requires an absent accounting record"),
            OrdinaryDepositMade { net_amount } => {
                next.deposit_liability = checked_add(
                    Component::DepositLiability,
                    next.deposit_liability,
                    net_amount,
                )?;
            }
            OrdinaryDepositMinted { amount } => {
                next.supply = checked_add(Component::Supply, next.supply, amount)?;
                next.deposit_liability =
                    checked_sub(Component::DepositLiability, next.deposit_liability, amount)?;
            }
            OrdinaryDepositFailed
            | WithdrawalDeliveryFailed
            | WithdrawalBounceBackRefundPending => {}
            FailedDepositRefundPaid { original_amount } => {
                next.deposit_liability = checked_sub(
                    Component::DepositLiability,
                    next.deposit_liability,
                    original_amount,
                )?;
            }
            FailedDepositRefundPending { bounceback_fee } => {
                next.deposit_liability = checked_sub(
                    Component::DepositLiability,
                    next.deposit_liability,
                    bounceback_fee,
                )?;
            }
            PortalRefundClaimed { refund_amount } => {
                next.deposit_liability = checked_sub(
                    Component::DepositLiability,
                    next.deposit_liability,
                    refund_amount,
                )?;
            }
            UserWithdrawalAccepted { amount, fee } => {
                let burned = amount
                    .checked_add(fee)
                    .ok_or(AccountingError::Overflow(Component::WithdrawalBurn))?;
                next.supply = checked_sub(Component::Supply, next.supply, burned)?;
                next.withdrawal_liability = checked_add(
                    Component::WithdrawalLiability,
                    next.withdrawal_liability,
                    amount,
                )?;
            }
            UserWithdrawalDelivered { amount } => {
                next.withdrawal_liability = checked_sub(
                    Component::WithdrawalLiability,
                    next.withdrawal_liability,
                    amount,
                )?;
            }
            WithdrawalBounceBackMinted { amount } | InboxRefundClaimed { amount } => {
                next.supply = checked_add(Component::Supply, next.supply, amount)?;
                next.withdrawal_liability = checked_sub(
                    Component::WithdrawalLiability,
                    next.withdrawal_liability,
                    amount,
                )?;
            }
        }
        Ok(next)
    }
}

/// Apply one exact section 5.5 row to an optional per-token record.
///
/// A token is enabled only when its record is absent. A present zero record is
/// already enabled and a duplicate enablement fails closed.
pub(crate) fn apply_token_accounting(
    current: Option<TokenAccounting>,
    transition: AccountingTransition,
) -> Result<TokenAccounting, AccountingError> {
    match (current, transition) {
        (None, AccountingTransition::TokenEnabled) => Ok(TokenAccounting::ZERO),
        (Some(_), AccountingTransition::TokenEnabled) => Err(AccountingError::AlreadyInitialized),
        (None, _) => Err(AccountingError::NotInitialized),
        (Some(accounting), transition) => accounting.apply_enabled(transition),
    }
}

/// Every row in the release-one accounting table. No event or invariant
/// registry dispatches these transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccountingTransition {
    TokenEnabled,
    OrdinaryDepositMade { net_amount: U256 },
    OrdinaryDepositMinted { amount: U256 },
    OrdinaryDepositFailed,
    FailedDepositRefundPaid { original_amount: U256 },
    FailedDepositRefundPending { bounceback_fee: U256 },
    PortalRefundClaimed { refund_amount: U256 },
    UserWithdrawalAccepted { amount: U256, fee: U256 },
    UserWithdrawalDelivered { amount: U256 },
    WithdrawalDeliveryFailed,
    WithdrawalBounceBackMinted { amount: U256 },
    WithdrawalBounceBackRefundPending,
    InboxRefundClaimed { amount: U256 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Component {
    Supply,
    DepositLiability,
    WithdrawalLiability,
    WithdrawalBurn,
    CollateralRequirement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum AccountingError {
    #[error("token accounting is not initialized")]
    NotInitialized,
    #[error("token accounting is already initialized")]
    AlreadyInitialized,
    #[error("{0:?} accounting overflow")]
    Overflow(Component),
    #[error("{0:?} accounting underflow")]
    Underflow(Component),
}

fn checked_add(component: Component, lhs: U256, rhs: U256) -> Result<U256, AccountingError> {
    lhs.checked_add(rhs)
        .ok_or(AccountingError::Overflow(component))
}

fn checked_sub(component: Component, lhs: U256, rhs: U256) -> Result<U256, AccountingError> {
    lhs.checked_sub(rhs)
        .ok_or(AccountingError::Underflow(component))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(supply: u64, deposit: u64, withdrawal: u64) -> TokenAccounting {
        TokenAccounting {
            supply: U256::from(supply),
            deposit_liability: U256::from(deposit),
            withdrawal_liability: U256::from(withdrawal),
        }
    }

    #[test]
    fn model_sdw_transition_table_is_exact() {
        use AccountingTransition::*;

        let cases = [
            (None, TokenEnabled, a(0, 0, 0)),
            (
                Some(a(10, 20, 30)),
                OrdinaryDepositMade {
                    net_amount: U256::from(7),
                },
                a(10, 27, 30),
            ),
            (
                Some(a(10, 20, 30)),
                OrdinaryDepositMinted {
                    amount: U256::from(7),
                },
                a(17, 13, 30),
            ),
            (Some(a(10, 20, 30)), OrdinaryDepositFailed, a(10, 20, 30)),
            (
                Some(a(10, 20, 30)),
                FailedDepositRefundPaid {
                    original_amount: U256::from(7),
                },
                a(10, 13, 30),
            ),
            (
                Some(a(10, 20, 30)),
                FailedDepositRefundPending {
                    bounceback_fee: U256::from(7),
                },
                a(10, 13, 30),
            ),
            (
                Some(a(10, 20, 30)),
                PortalRefundClaimed {
                    refund_amount: U256::from(7),
                },
                a(10, 13, 30),
            ),
            (
                Some(a(20, 20, 30)),
                UserWithdrawalAccepted {
                    amount: U256::from(7),
                    fee: U256::from(2),
                },
                a(11, 20, 37),
            ),
            (
                Some(a(10, 20, 30)),
                UserWithdrawalDelivered {
                    amount: U256::from(7),
                },
                a(10, 20, 23),
            ),
            (Some(a(10, 20, 30)), WithdrawalDeliveryFailed, a(10, 20, 30)),
            (
                Some(a(10, 20, 30)),
                WithdrawalBounceBackMinted {
                    amount: U256::from(7),
                },
                a(17, 20, 23),
            ),
            (
                Some(a(10, 20, 30)),
                WithdrawalBounceBackRefundPending,
                a(10, 20, 30),
            ),
            (
                Some(a(10, 20, 30)),
                InboxRefundClaimed {
                    amount: U256::from(7),
                },
                a(17, 20, 23),
            ),
        ];

        for (before, transition, expected) in cases {
            assert_eq!(apply_token_accounting(before, transition), Ok(expected));
        }

        assert_eq!(
            apply_token_accounting(Some(TokenAccounting::ZERO), TokenEnabled),
            Err(AccountingError::AlreadyInitialized),
            "record presence must reject duplicate zero-state enablement"
        );
        assert_eq!(
            apply_token_accounting(
                None,
                OrdinaryDepositMade {
                    net_amount: U256::ONE,
                }
            ),
            Err(AccountingError::NotInitialized)
        );
    }

    #[test]
    fn model_sdw_arithmetic_fails_closed() {
        assert_eq!(
            apply_token_accounting(
                Some(a(0, 0, 0)),
                AccountingTransition::OrdinaryDepositMinted { amount: U256::ONE }
            ),
            Err(AccountingError::Underflow(Component::DepositLiability))
        );
        assert_eq!(
            apply_token_accounting(
                Some(TokenAccounting {
                    supply: U256::MAX,
                    ..TokenAccounting::ZERO
                }),
                AccountingTransition::WithdrawalBounceBackMinted { amount: U256::ONE }
            ),
            Err(AccountingError::Overflow(Component::Supply))
        );
        assert_eq!(
            apply_token_accounting(
                Some(a(1, 0, 0)),
                AccountingTransition::UserWithdrawalAccepted {
                    amount: U256::MAX,
                    fee: U256::ONE,
                }
            ),
            Err(AccountingError::Overflow(Component::WithdrawalBurn))
        );

        assert_eq!(a(10, 20, 30).collateral_requirement(), Ok(U256::from(60)));
        assert_eq!(
            TokenAccounting {
                supply: U256::MAX,
                deposit_liability: U256::ONE,
                withdrawal_liability: U256::ZERO,
            }
            .collateral_requirement(),
            Err(AccountingError::Overflow(Component::CollateralRequirement))
        );
    }
}
