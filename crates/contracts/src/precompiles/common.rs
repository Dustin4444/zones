crate::sol! {
    /// Generic unauthorized access error used by zone wrapper logic.
    #[derive(Debug)]
    error Unauthorized();

    /// Minimal TIP-20 read interface used by sequencer-side accounting.
    #[derive(Debug)]
    interface ITIP20 {
        function totalSupply() external view returns (uint256);
    }
}
