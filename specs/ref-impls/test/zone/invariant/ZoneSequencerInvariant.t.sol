// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import { ZoneMessenger } from "../../../src/zone/ZoneMessenger.sol";
import { ZonePortal } from "../../../src/zone/ZonePortal.sol";
import { MockZoneToken } from "../mocks/MockZoneToken.sol";
import { Test } from "forge-std/Test.sol";

/// @title ZoneSequencerHandler
/// @notice Drives the two-step sequencer handover (`transferSequencer` / `acceptSequencer`)
///         while mirroring the expected `(sequencer, pendingSequencer)` state. Every action is
///         a legal operation replayed against a ghost model, so the invariant test can assert
///         the on-chain state machine never diverges from the reference (I-20).
contract ZoneSequencerHandler is Test {

    ZonePortal internal immutable portal;
    address[4] internal candidates;

    address public expectedSequencer;
    address public expectedPending;
    uint256 public handoverCount; // coverage: proves the accept path actually executed

    constructor(ZonePortal _portal, address _initialSequencer, address[4] memory _candidates) {
        portal = _portal;
        expectedSequencer = _initialSequencer;
        candidates = _candidates;
    }

    function _candidate(uint256 seed) internal view returns (address) {
        return candidates[seed % candidates.length];
    }

    /// @notice Current sequencer starts a transfer to a (non-zero) candidate.
    function propose(uint256 candidateSeed) external {
        address next = _candidate(candidateSeed);
        vm.prank(expectedSequencer);
        portal.transferSequencer(next);
        expectedPending = next;
    }

    /// @notice The pending sequencer accepts, completing the handover exactly once.
    function accept() external {
        if (expectedPending == address(0)) return;
        vm.prank(expectedPending);
        portal.acceptSequencer();
        expectedSequencer = expectedPending;
        expectedPending = address(0);
        handoverCount++;
    }

    /// @notice Propose then immediately accept, guaranteeing handovers regardless of
    ///         random interleaving (keeps the accept path well-covered).
    function proposeThenAccept(uint256 candidateSeed) external {
        address next = _candidate(candidateSeed);
        vm.prank(expectedSequencer);
        portal.transferSequencer(next);
        vm.prank(next);
        portal.acceptSequencer();
        expectedSequencer = next;
        expectedPending = address(0);
        handoverCount++;
    }

}

/// @title ZoneSequencerInvariantTest
/// @notice Stateful invariant for the two-step sequencer transfer (I-20): `pendingSequencer`
///         is set by the current sequencer, consumed exactly once by `acceptSequencer`, then
///         reset to zero — so block production can never be seized by an unintended address.
contract ZoneSequencerInvariantTest is Test {

    ZonePortal internal portal;
    MockZoneToken internal token;
    ZoneSequencerHandler internal handler;

    address internal constant SEQ0 = address(0x5e9001);
    address internal constant SEQ1 = address(0x5e9002);
    address internal constant SEQ2 = address(0x5e9003);
    address internal constant SEQ3 = address(0x5e9004);

    bytes32 constant GENESIS_BLOCK_HASH = keccak256("genesis");

    function setUp() public {
        token = new MockZoneToken("Zone USD", "zUSD");

        uint256 nonce = vm.getNonce(address(this));
        address predictedPortal = vm.computeCreateAddress(address(this), nonce + 1);
        ZoneMessenger messenger = new ZoneMessenger(predictedPortal);
        portal = new ZonePortal(
            1,
            address(token),
            address(messenger),
            address(this), // admin
            SEQ0, // initial sequencer
            address(0), // verifier (unused by sequencer handover)
            GENESIS_BLOCK_HASH,
            uint64(block.number),
            ""
        );

        handler = new ZoneSequencerHandler(portal, SEQ0, [SEQ0, SEQ1, SEQ2, SEQ3]);
        targetContract(address(handler));
    }

    /// @notice On-chain sequencer state never diverges from the legal-operation ghost model,
    ///         and the live sequencer is never the zero address (block production stays owned).
    function invariant_sequencerStateMachine() public view {
        assertEq(
            portal.sequencer(),
            handler.expectedSequencer(),
            "I-20: sequencer diverged from two-step handover model"
        );
        assertEq(
            portal.pendingSequencer(),
            handler.expectedPending(),
            "I-20: pendingSequencer diverged from two-step handover model"
        );
        assertTrue(portal.sequencer() != address(0), "I-20: sequencer became the zero address");
    }

    /// @notice Guard against a vacuous pass: at least one full handover must have completed.
    function afterInvariant() public view {
        assertGt(handler.handoverCount(), 0, "I-20: handover path never exercised");
    }

}
