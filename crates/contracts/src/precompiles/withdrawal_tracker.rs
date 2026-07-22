//! `WithdrawalTracker` — Zone L2 withdrawal-balance accounting precompile.

pub use IWithdrawalTracker::IWithdrawalTrackerErrors as WithdrawalTrackerError;

crate::sol! {
    #[derive(Debug, PartialEq, Eq)]
    interface IWithdrawalTracker {
        /// Withdrawal balance attributed to one user for one bridged token.
        function zoneBalance(address user, address token) external view returns (uint256);

        /// Aggregate Zone supply for one bridged token.
        function zoneTotalSupply(address token) external view returns (uint256);

        /// Credit Zone balance after ZoneInbox successfully mints a deposit.
        function deposit(address user, address token, uint256 amount) external;

        /// Debit Zone balance before ZoneOutbox burns a user withdrawal and its fee.
        function withdraw(address user, address token, uint256 amount, uint256 fee) external;

        error OnlyZoneInbox();
        error OnlyZoneOutbox();
        error InsufficientZoneBalance(
            address user,
            address token,
            uint256 requested,
            uint256 available
        );
    }
}
