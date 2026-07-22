// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

// EIP-2935 system contract address (Pectra).
// Takes raw 32-byte calldata (block number, no function selector) and returns the block hash.
address constant EIP2935 = 0x0000F90827F1C53a10cb7A02335B175320002935;

/// @notice Reads a historical block hash from EIP-2935 or the native BLOCKHASH opcode.
/// @dev EIP-2935 expects raw 32-byte calldata (no function selector). Chains that have not
///      activated EIP-2935 can still serve hashes from the native 256-block history window.
function getBlockHash(uint256 blockNumber) view returns (bytes32 hash) {
    (bool success, bytes memory result) = EIP2935.staticcall(abi.encode(blockNumber));
    if (success && result.length >= 32) {
        hash = abi.decode(result, (bytes32));
    }

    if (hash == bytes32(0)) {
        hash = blockhash(blockNumber);
    }
}
