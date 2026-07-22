//! `ZoneFeeManager` — direct Zone fee custody and distribution.

crate::sol! {
    #[derive(Debug)]
    contract IZoneFeeManager {
        event FeesDistributed(address indexed beneficiary, address indexed token, uint256 amount);

        /// Aggregate beneficiary fees are public and do not expose fee-payer state.
        function collectedFees(address beneficiary, address token) external view returns (uint256);
        function distributeFees(address beneficiary, address token) external;
    }
}
