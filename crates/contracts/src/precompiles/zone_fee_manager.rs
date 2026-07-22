//! `ZoneFeeManager` — direct Zone fee custody and distribution.

crate::sol! {
    #[derive(Debug)]
    contract IZoneFeeManager {
        event FeesDistributed(address indexed beneficiary, address indexed token, uint256 amount);

        /// Only the beneficiary may read its aggregate accrued fees.
        function collectedFees(address beneficiary, address token) external view returns (uint256);

        /// Only the beneficiary may distribute its accrued fees.
        function distributeFees(address beneficiary, address token) external;
    }
}
