// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import { EIP2935 } from "../../src/zone/BlockHashHistory.sol";
import {
    BlockTransition,
    DepositQueueTransition,
    IZonePortal,
    Withdrawal
} from "../../src/zone/IZone.sol";
import { EMPTY_SENTINEL } from "../../src/zone/WithdrawalQueueLib.sol";
import { ZonePortal } from "../../src/zone/ZonePortal.sol";
import { MockVerifier } from "./mocks/MockVerifier.sol";
import { Test } from "forge-std/Test.sol";

contract MockPortalToken {

    string public name = "Mock USD";
    string public symbol = "mUSD";
    string public currency = "USD";

    function approve(address, uint256) external pure returns (bool) {
        return true;
    }

}

contract MockBlockHashHistory {

    fallback(bytes calldata input) external returns (bytes memory) {
        uint256 blockNumber = abi.decode(input, (uint256));
        return abi.encode(keccak256(abi.encode(blockNumber)));
    }

}

contract ZonePortalGasLimitTest is Test {

    uint256 internal constant WITHDRAWAL_QUEUE_TAIL_SLOT = 10;
    uint256 internal constant WITHDRAWAL_QUEUE_SLOTS_MAPPING_SLOT = 11;

    ZonePortal public portal;
    MockPortalToken public token;

    address public fallbackRecipient = address(0x200);
    address public recipient = address(0x300);

    function setUp() public {
        token = new MockPortalToken();
        portal = new ZonePortal(
            1,
            address(token),
            address(0x400),
            address(this),
            keccak256("genesis"),
            uint64(block.number)
        );
    }

    function test_processWithdrawal_overMaxGasLimit_bouncesBackAndClearsQueue() public {
        Withdrawal memory w = Withdrawal({
            token: address(token),
            senderTag: keccak256("sender"),
            to: recipient,
            amount: 500e6,
            fee: 0,
            memo: bytes32(0),
            gasLimit: portal.MAX_WITHDRAWAL_GAS_LIMIT() + 1,
            fallbackRecipient: fallbackRecipient,
            callbackData: "test",
            encryptedSender: ""
        });
        bytes32 wHash = keccak256(abi.encode(w, EMPTY_SENTINEL));

        vm.store(address(portal), bytes32(WITHDRAWAL_QUEUE_TAIL_SLOT), bytes32(uint256(1)));
        vm.store(address(portal), _withdrawalQueueSlot(0), wHash);

        vm.expectEmit(true, false, false, true, address(portal));
        emit IZonePortal.WithdrawalProcessed(recipient, address(token), 500e6, false);
        portal.processWithdrawal(w, bytes32(0));

        assertEq(portal.withdrawalQueueHead(), 1);
        assertEq(portal.withdrawalQueueSlot(0), EMPTY_SENTINEL);
        assertTrue(portal.currentDepositQueueHash() != bytes32(0));
    }

    function _withdrawalQueueSlot(uint256 slot) internal pure returns (bytes32) {
        return keccak256(abi.encode(slot, WITHDRAWAL_QUEUE_SLOTS_MAPPING_SLOT));
    }

}

contract ZonePortalFactoryVerifierTest is Test {

    ZonePortal public portal;
    MockPortalToken public token;
    MockVerifier public verifier;
    MockVerifier public forkVerifier;

    address public activeVerifier;

    function setUp() public {
        MockBlockHashHistory history = new MockBlockHashHistory();
        vm.etch(EIP2935, address(history).code);

        token = new MockPortalToken();
        verifier = new MockVerifier();
        forkVerifier = new MockVerifier();
        activeVerifier = address(verifier);

        portal = new ZonePortal(
            1,
            address(token),
            address(0x400),
            address(this),
            keccak256("genesis"),
            uint64(block.number)
        );
    }

    function verifierForTempoBlock(uint64) external view returns (address) {
        return activeVerifier;
    }

    function test_submitBatch_usesFactoryVerifierOnEachSubmission() public {
        _submitBatch(keccak256("state-1"));

        activeVerifier = address(forkVerifier);
        forkVerifier.setShouldAccept(false);

        assertEq(portal.factory(), address(this));
        assertEq(this.verifierForTempoBlock(uint64(block.number)), address(forkVerifier));
        assertFalse(forkVerifier.shouldAccept());

        vm.roll(block.number + 1);
        bytes32 prevBlockHash = portal.blockHash();
        vm.expectRevert(IZonePortal.InvalidProof.selector);
        _submitBatchAtCurrentBlock(prevBlockHash, keccak256("state-2-rejected"));

        forkVerifier.setShouldAccept(true);
        _submitBatch(keccak256("state-2"));

        assertEq(portal.withdrawalBatchIndex(), 2);
        assertEq(portal.blockHash(), keccak256("state-2"));
    }

    function _submitBatch(bytes32 nextBlockHash) internal {
        vm.roll(block.number + 1);
        _submitBatchAtCurrentBlock(portal.blockHash(), nextBlockHash);
    }

    function _submitBatchAtCurrentBlock(bytes32 prevBlockHash, bytes32 nextBlockHash) internal {
        portal.submitBatch(
            uint64(block.number - 1),
            0,
            BlockTransition({ prevBlockHash: prevBlockHash, nextBlockHash: nextBlockHash }),
            DepositQueueTransition({
                prevProcessedHash: bytes32(0),
                nextProcessedHash: bytes32(0),
                prevDepositNumber: 0,
                nextDepositNumber: 0
            }),
            bytes32(0),
            "",
            ""
        );
    }

}
