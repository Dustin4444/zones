//! `NativeSignatureVerifier` — local verifier for native end-to-end tests.

crate::sol! {
    #[derive(Debug)]
    contract NativeSignatureVerifier {
        struct Policy {
            address signer;
            uint64 verifierVersion;
            bool enabled;
        }

        struct BlockTransition {
            bytes32 prevBlockHash;
            bytes32 nextBlockHash;
        }

        struct DepositQueueTransition {
            bytes32 prevProcessedHash;
            bytes32 nextProcessedHash;
            uint64 prevDepositNumber;
            uint64 nextDepositNumber;
        }

        function registerPortal(address portal, address signer, uint64 verifierVersion) external;
        function disablePortal(address portal) external;
        function policies(address portal) external view returns (Policy memory);
        function verifierAdmin() external view returns (address);
        function PROTOCOL_VERSION() external view returns (uint16);
        function verify(
            uint64 tempoBlockNumber,
            uint64 anchorBlockNumber,
            bytes32 anchorBlockHash,
            uint64 expectedWithdrawalBatchIndex,
            address sequencer,
            BlockTransition calldata blockTransition,
            DepositQueueTransition calldata depositQueueTransition,
            bytes32 withdrawalQueueHash,
            bytes calldata verifierConfig,
            bytes calldata proof
        ) external view returns (bool);
    }
}
