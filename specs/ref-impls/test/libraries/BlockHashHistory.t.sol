// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import { EIP2935, getBlockHash } from "../../src/libraries/BlockHashHistory.sol";
import { BaseTest } from "../BaseTest.t.sol";

contract BlockHashHistoryTest is BaseTest {

    /// @notice Verifies recent in-window blocks return the same hash as the BLOCKHASH opcode.
    function test_getBlockHash_returnsInWindowHash() public {
        vm.roll(10_000);
        uint256 blockNumber = block.number - 1;

        bytes32 hash = getBlockHash(blockNumber);

        assertEq(hash, blockhash(blockNumber));
        assertTrue(hash != bytes32(0));
    }

    /// @notice Verifies a nonzero EIP-2935 response is preferred over the native opcode.
    function test_getBlockHash_prefersEIP2935Hash() public {
        vm.roll(10_000);
        uint256 blockNumber = block.number - 1;
        bytes32 expected = keccak256("eip-2935 hash");
        vm.mockCall(EIP2935, abi.encode(blockNumber), abi.encode(expected));

        assertEq(getBlockHash(blockNumber), expected);
        assertNotEq(expected, blockhash(blockNumber));
    }

    /// @notice Verifies an unpopulated EIP-2935 contract falls back to the native opcode.
    function test_getBlockHash_zeroEIP2935ResponseFallsBackToBlockhash() public {
        vm.roll(10_000);
        uint256 blockNumber = block.number - 1;
        vm.mockCall(EIP2935, abi.encode(blockNumber), abi.encode(bytes32(0)));

        assertEq(getBlockHash(blockNumber), blockhash(blockNumber));
        assertNotEq(getBlockHash(blockNumber), bytes32(0));
    }

    /// @notice Verifies a chain without EIP-2935 falls back to the native opcode.
    function test_getBlockHash_emptyEIP2935ResponseFallsBackToBlockhash() public {
        vm.roll(10_000);
        uint256 blockNumber = block.number - 1;
        vm.etch(EIP2935, "");

        assertEq(getBlockHash(blockNumber), blockhash(blockNumber));
        assertNotEq(getBlockHash(blockNumber), bytes32(0));
    }

    /// @notice Verifies unavailable hashes remain zero when EIP-2935 is absent.
    function test_getBlockHash_withoutEIP2935ReturnsZeroOutsideNativeWindow() public {
        vm.roll(10_000);
        vm.etch(EIP2935, "");

        assertEq(getBlockHash(block.number - 257), bytes32(0));
    }

    /// @notice Verifies blocks older than the history window return zero.
    function test_getBlockHash_returnsZeroForOutOfWindowBlock() public {
        vm.roll(20_000);
        uint256 blockNumber = block.number - BLOCKHASH_HISTORY_WINDOW - 1;

        assertEq(getBlockHash(blockNumber), bytes32(0));
    }

    /// @notice Verifies genesis, current, and future unknown blocks return zero.
    function test_getBlockHash_returnsZeroForUnknownBlocks() public {
        vm.roll(10_000);

        assertEq(getBlockHash(0), bytes32(0));
        assertEq(getBlockHash(block.number), bytes32(0));
        assertEq(getBlockHash(block.number + 1), bytes32(0));
    }

}
