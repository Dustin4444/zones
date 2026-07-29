// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import { ZonePortal } from "../../src/tempo/ZonePortal.sol";
import { BaseTest } from "../BaseTest.t.sol";
import { Test } from "forge-std/Test.sol";

/// @notice Drives valid TIP-1091 sequencer-set and threshold replacements.
contract ZoneSequencerHandler is Test {

    ZonePortal internal immutable portal;
    address internal immutable admin;
    address[4] internal candidates;

    uint64 public successfulUpdates;

    constructor(ZonePortal _portal, address _admin) {
        portal = _portal;
        admin = _admin;
        candidates = [address(0xA1), address(0xA2), address(0xA3), address(0xA4)];
    }

    function replaceSequencerSet(uint8 countSeed, uint8 thresholdSeed) external {
        uint256 count = uint256(countSeed % 4) + 1;
        address[] memory members = new address[](count);
        for (uint256 i; i < count; ++i) {
            members[i] = candidates[i];
        }
        uint8 threshold = uint8(uint256(thresholdSeed) % count) + 1;

        vm.prank(admin);
        try portal.setSequencerSet(members, threshold) {
            ++successfulUpdates;
        } catch { }
    }

}

/// @notice Stateful invariants for the current multi-sequencer configuration model.
contract ZoneSequencerInvariantTest is BaseTest {

    ZonePortal internal portal;
    ZoneSequencerHandler internal handler;
    uint64 internal initialVersion;

    function setUp() public override {
        super.setUp();
        address[] memory sequencers = new address[](1);
        sequencers[0] = sequencer;
        portal = _createZonePortal(1, address(pathUSD), admin, sequencers, 1, "");
        initialVersion = portal.sequencerSetVersion();
        handler = new ZoneSequencerHandler(portal, admin);
        targetContract(address(handler));
    }

    function invariant_sequencerSetIsValid() public view {
        uint256 count = portal.sequencerCount();
        assertGt(count, 0);
        assertLe(count, portal.MAX_SEQUENCERS());
        assertGt(portal.sequencerThreshold(), 0);
        assertLe(portal.sequencerThreshold(), count);

        for (uint256 i; i < count; ++i) {
            address member = portal.sequencerAt(i);
            assertTrue(portal.isSequencer(member));
            for (uint256 j; j < i; ++j) {
                assertNotEq(member, portal.sequencerAt(j));
            }
        }
    }

    function invariant_versionTracksSuccessfulUpdates() public view {
        assertEq(portal.sequencerSetVersion(), initialVersion + handler.successfulUpdates());
    }

    function afterInvariant() public view {
        assertGt(handler.successfulUpdates(), 0, "sequencer update path not exercised");
    }

}
