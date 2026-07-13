// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

/// @notice Mock tx context precompile for Solidity-only tests.
/// @dev Returns deterministic unique transaction identifiers in increasing sequence order.
contract MockZoneTxContext {

    uint256 public sequence;

    function currentUniqueTxIdentifier() external returns (bytes32) {
        sequence++;
        return uniqueTxIdentifierFor(sequence);
    }

    function uniqueTxIdentifierFor(uint256 seq) public pure returns (bytes32) {
        return keccak256(abi.encodePacked("mock-zone-unique-tx-identifier", seq));
    }

}
