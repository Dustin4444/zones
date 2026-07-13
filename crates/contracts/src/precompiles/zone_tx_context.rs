//! `ZoneTxContext` — Zone L2 precompile.

crate::sol! {
    #[derive(Debug)]
    contract ZoneTxContext {
        function currentUniqueTxIdentifier() external returns (bytes32);
    }
}
