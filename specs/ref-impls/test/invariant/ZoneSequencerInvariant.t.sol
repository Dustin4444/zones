// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import {
    BlockTransition,
    DepositQueueTransition,
    IZonePortal
} from "../../src/interfaces/IZone.sol";
import { getBlockHash } from "../../src/libraries/BlockHashHistory.sol";
import { ZonePortal } from "../../src/tempo/ZonePortal.sol";
import { BaseTest } from "../BaseTest.t.sol";
import { Test } from "forge-std/Test.sol";

/// @notice Drives valid TIP-1091 rotations and settlement certificates.
contract ZoneSequencerHandler is Test {

    bytes32 internal constant EIP712_DOMAIN_TYPEHASH = keccak256(
        "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"
    );
    bytes32 internal constant SETTLEMENT_ATTESTATION_TYPEHASH = keccak256(
        "SettlementAttestation(uint32 zoneId,uint64 sequencerSetVersion,uint256 zoneHeight,uint256 withdrawalBatchIndex,address verifier,uint64 tempoBlockNumber,uint64 anchorBlockNumber,bytes32 anchorBlockHash,bytes32 blockTransitionHash,bytes32 depositQueueTransitionHash,bytes32 withdrawalQueueHash,bytes32 verifierConfigHash)"
    );

    ZonePortal internal immutable portal;
    address internal immutable admin;
    uint256[4] internal keys = [uint256(1), uint256(2), uint256(3), uint256(4)];
    address[4] internal candidates;

    uint64 public successfulUpdates;
    uint64 public validSettlements;
    uint64 public staleRejections;
    uint64 public duplicateRejections;
    uint64 public subthresholdRejections;

    struct SettlementState {
        uint256 zoneHeight;
        uint64 withdrawalBatchIndex;
        bytes32 blockHash;
        uint64 lastSyncedTempoBlockNumber;
        uint64 lastProcessedDepositNumber;
        uint256 queueHead;
        uint256 queueTail;
    }

    constructor(ZonePortal _portal, address _admin) {
        portal = _portal;
        admin = _admin;
        for (uint256 i; i < candidates.length; ++i) {
            candidates[i] = vm.addr(keys[i]);
        }
    }

    function replaceSequencerSet(uint8 countSeed, uint8 thresholdSeed) external {
        uint256 count = uint256(countSeed % 4) + 1;
        _rotate(count, uint8(uint256(thresholdSeed) % count) + 1);
    }

    function submitValidSettlement() external {
        _ensureQuorumSet();
        (
            uint64 tempoBlockNumber,
            BlockTransition memory blocks,
            DepositQueueTransition memory deposits
        ) = _settlementInputs();
        uint256 nextHeight = portal.zoneHeight() + 1;
        bytes32 digest = _attestationDigest(
            portal.sequencerSetVersion(), nextHeight, tempoBlockNumber, blocks, deposits
        );
        bytes[] memory signatures = _signatures(digest, portal.sequencerThreshold(), false);
        SettlementState memory beforeState = _state();

        vm.prank(portal.sequencerAt(0));
        portal.submitBatch(
            tempoBlockNumber, 0, blocks, deposits, bytes32(0), "", "", nextHeight, signatures
        );

        assertEq(portal.zoneHeight(), beforeState.zoneHeight + 1);
        assertEq(portal.withdrawalBatchIndex(), beforeState.withdrawalBatchIndex + 1);
        assertEq(portal.withdrawalQueueHead(), beforeState.queueHead);
        assertEq(portal.withdrawalQueueTail(), beforeState.queueTail);
        ++validSettlements;
    }

    function submitStaleVersionCertificate() external {
        _ensureQuorumSet();
        (
            uint64 tempoBlockNumber,
            BlockTransition memory blocks,
            DepositQueueTransition memory deposits
        ) = _settlementInputs();
        uint256 nextHeight = portal.zoneHeight() + 1;
        bytes32 digest = _attestationDigest(
            portal.sequencerSetVersion(), nextHeight, tempoBlockNumber, blocks, deposits
        );
        bytes[] memory signatures = _signatures(digest, portal.sequencerThreshold(), false);

        // Rotate the version while retaining every signer and the same threshold, so rejection
        // can only be caused by the certificate's stale sequencer-set version.
        _rotate(4, 2);
        _expectCertificateRejection(tempoBlockNumber, blocks, deposits, nextHeight, signatures);
        ++staleRejections;
    }

    function submitDuplicateSignatures() external {
        _ensureQuorumSet();
        (
            uint64 tempoBlockNumber,
            BlockTransition memory blocks,
            DepositQueueTransition memory deposits
        ) = _settlementInputs();
        uint256 nextHeight = portal.zoneHeight() + 1;
        bytes32 digest = _attestationDigest(
            portal.sequencerSetVersion(), nextHeight, tempoBlockNumber, blocks, deposits
        );
        bytes[] memory signatures = _signatures(digest, portal.sequencerThreshold(), true);
        _expectCertificateRejection(tempoBlockNumber, blocks, deposits, nextHeight, signatures);
        ++duplicateRejections;
    }

    function submitSubthresholdSignatures() external {
        _ensureQuorumSet();
        (
            uint64 tempoBlockNumber,
            BlockTransition memory blocks,
            DepositQueueTransition memory deposits
        ) = _settlementInputs();
        uint256 nextHeight = portal.zoneHeight() + 1;
        bytes32 digest = _attestationDigest(
            portal.sequencerSetVersion(), nextHeight, tempoBlockNumber, blocks, deposits
        );
        bytes[] memory signatures = _signatures(digest, portal.sequencerThreshold() - 1, false);
        _expectCertificateRejection(tempoBlockNumber, blocks, deposits, nextHeight, signatures);
        ++subthresholdRejections;
    }

    function _ensureQuorumSet() internal {
        if (portal.sequencerCount() != 3 || portal.sequencerThreshold() != 2) _rotate(3, 2);
    }

    function _rotate(uint256 count, uint8 threshold) internal {
        address[] memory members = new address[](count);
        for (uint256 i; i < count; ++i) {
            members[i] = candidates[i];
        }
        vm.prank(admin);
        try portal.setSequencerSet(members, threshold) {
            ++successfulUpdates;
        } catch { }
    }

    function _settlementInputs()
        internal
        returns (
            uint64 tempoBlockNumber,
            BlockTransition memory blocks,
            DepositQueueTransition memory deposits
        )
    {
        vm.roll(block.number + 1);
        tempoBlockNumber = uint64(block.number - 1);
        blocks = BlockTransition({
            prevBlockHash: portal.blockHash(),
            nextBlockHash: keccak256(abi.encode(portal.zoneHeight(), tempoBlockNumber))
        });
        uint64 depositNumber = portal.lastProcessedDepositNumber();
        deposits = DepositQueueTransition({
            prevProcessedHash: bytes32(0),
            nextProcessedHash: bytes32(0),
            prevDepositNumber: depositNumber,
            nextDepositNumber: depositNumber
        });
    }

    function _attestationDigest(
        uint64 version,
        uint256 height,
        uint64 tempoBlockNumber,
        BlockTransition memory blocks,
        DepositQueueTransition memory deposits
    )
        internal
        view
        returns (bytes32)
    {
        bytes32 domainSeparator = keccak256(
            abi.encode(
                EIP712_DOMAIN_TYPEHASH,
                keccak256("ZonePortal"),
                keccak256("1"),
                block.chainid,
                address(portal)
            )
        );
        bytes32 structHash = keccak256(
            abi.encode(
                SETTLEMENT_ATTESTATION_TYPEHASH,
                portal.zoneId(),
                version,
                height,
                portal.withdrawalBatchIndex() + 1,
                portal.verifier(),
                tempoBlockNumber,
                tempoBlockNumber,
                getBlockHash(tempoBlockNumber),
                keccak256(abi.encode(blocks)),
                keccak256(abi.encode(deposits)),
                bytes32(0),
                keccak256("")
            )
        );
        return keccak256(abi.encodePacked("\x19\x01", domainSeparator, structHash));
    }

    function _signatures(
        bytes32 digest,
        uint256 count,
        bool duplicate
    )
        internal
        returns (bytes[] memory signatures)
    {
        signatures = new bytes[](count);
        for (uint256 i; i < count; ++i) {
            uint256 key = keys[duplicate ? 0 : i];
            (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, digest);
            signatures[i] = abi.encodePacked(r, s, v);
        }
    }

    function _expectCertificateRejection(
        uint64 tempoBlockNumber,
        BlockTransition memory blocks,
        DepositQueueTransition memory deposits,
        uint256 nextHeight,
        bytes[] memory signatures
    )
        internal
    {
        SettlementState memory beforeState = _state();
        vm.prank(portal.sequencerAt(0));
        try portal.submitBatch(
            tempoBlockNumber, 0, blocks, deposits, bytes32(0), "", "", nextHeight, signatures
        ) {
            assertTrue(false, "invalid certificate accepted");
        } catch (bytes memory reason) {
            assertEq(bytes4(reason), IZonePortal.InvalidQuorumCertificate.selector);
        }
        _assertStateUnchanged(beforeState);
    }

    function _state() internal view returns (SettlementState memory state) {
        state = SettlementState({
            zoneHeight: portal.zoneHeight(),
            withdrawalBatchIndex: portal.withdrawalBatchIndex(),
            blockHash: portal.blockHash(),
            lastSyncedTempoBlockNumber: portal.lastSyncedTempoBlockNumber(),
            lastProcessedDepositNumber: portal.lastProcessedDepositNumber(),
            queueHead: portal.withdrawalQueueHead(),
            queueTail: portal.withdrawalQueueTail()
        });
    }

    function _assertStateUnchanged(SettlementState memory expected) internal view {
        assertEq(portal.zoneHeight(), expected.zoneHeight);
        assertEq(portal.withdrawalBatchIndex(), expected.withdrawalBatchIndex);
        assertEq(portal.blockHash(), expected.blockHash);
        assertEq(portal.lastSyncedTempoBlockNumber(), expected.lastSyncedTempoBlockNumber);
        assertEq(portal.lastProcessedDepositNumber(), expected.lastProcessedDepositNumber);
        assertEq(portal.withdrawalQueueHead(), expected.queueHead);
        assertEq(portal.withdrawalQueueTail(), expected.queueTail);
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
        sequencers[0] = vm.addr(1);
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
        assertGt(handler.successfulUpdates(), 0, "sequencer rotation not exercised");
        assertGt(handler.validSettlements(), 0, "valid settlement not exercised");
        assertGt(handler.staleRejections(), 0, "stale certificate not exercised");
        assertGt(handler.duplicateRejections(), 0, "duplicate signatures not exercised");
        assertGt(handler.subthresholdRejections(), 0, "sub-threshold signatures not exercised");
    }

}
