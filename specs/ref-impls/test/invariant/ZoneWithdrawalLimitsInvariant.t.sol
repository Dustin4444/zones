// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import {
    PORTAL_IS_SEQUENCER_SLOT,
    ZONE_INBOX,
    ZONE_TX_CONTEXT
} from "../../src/interfaces/IZone.sol";
import { ZoneOutbox } from "../../src/zone/ZoneOutbox.sol";
import { MockTempoState } from "../mocks/MockTempoState.sol";
import { MockZoneToken } from "../mocks/MockZoneToken.sol";
import { MockZoneTxContext } from "../mocks/MockZoneTxContext.sol";
import { Test } from "forge-std/Test.sol";

/// @title ZoneOutboxHarness
/// @notice Exposes the outbox's stored pending-withdrawal array so invariants can assert the
///         per-item bounds (TEMPO-ZONE-WITHDRAWAL-CALLBACK-BOUNDS: gas limit and callback data size) that are otherwise internal.
contract ZoneOutboxHarness is ZoneOutbox {

    constructor(address _tempoPortal, address _tempoState) ZoneOutbox(_tempoPortal, _tempoState) { }

    function rawLength() external view returns (uint256) {
        return _pendingWithdrawals.length;
    }

    function gasLimitAt(uint256 i) external view returns (uint64) {
        return _pendingWithdrawals[i].gasLimit;
    }

    function callbackLenAt(uint256 i) external view returns (uint256) {
        return _pendingWithdrawals[i].callbackData.length;
    }

}

/// @title ZoneWithdrawalLimitsHandler
/// @notice Drives withdrawal requests with random gas limits and callback-data sizes (some
///         deliberately over the cap), so the invariant test can confirm the outbox never
///         stores an out-of-bounds withdrawal (TEMPO-ZONE-WITHDRAWAL-CALLBACK-BOUNDS) and never accepts more than
///         `maxWithdrawalsPerBlock` requests per block.
/// @dev Foundry keeps `block.number` constant across invariant calls (it never auto-advances),
///      so successful in-bounds requests accumulate toward the per-block cap across separate
///      `request` calls; once the cap is reached further in-bounds requests revert. Block
///      cadence and the in-/over-bounds mix are driven by a deterministic request counter
///      rather than fuzzed seeds, because Foundry's fuzzer biases seeds toward 0 (which would
///      otherwise roll a new block almost every call and prevent any accumulation).
contract ZoneWithdrawalLimitsHandler is Test {

    ZoneOutboxHarness internal immutable outbox;
    MockZoneToken internal immutable token;
    address internal immutable sequencer;
    address[3] internal actors;

    uint256 public immutable cap;
    uint64 public immutable maxGas;
    uint256 public immutable maxData;

    uint256 internal ghostBlock;
    uint256 internal reqCount; // deterministic driver for block cadence and bounds mix
    uint256 public successesThisBlock; // valid requests stored in the current block

    uint256 public capHits; // coverage: per-block cap was actually hit
    uint256 public boundRejects; // coverage: over-cap gas/data attempts were rejected
    uint256 public stored; // coverage: at least some withdrawals were stored

    constructor(
        ZoneOutboxHarness _outbox,
        MockZoneToken _token,
        address _sequencer,
        address _alice,
        address _bob,
        address _charlie
    ) {
        outbox = _outbox;
        token = _token;
        sequencer = _sequencer;
        actors = [_alice, _bob, _charlie];
        cap = _outbox.maxWithdrawalsPerBlock();
        maxGas = _outbox.MAX_WITHDRAWAL_GAS_LIMIT();
        maxData = _outbox.MAX_CALLBACK_DATA_SIZE();
        ghostBlock = block.number;
    }

    function _actor(uint256 seed) internal view returns (address) {
        return actors[seed % 3];
    }

    function _syncBlock() internal {
        if (block.number != ghostBlock) {
            ghostBlock = block.number;
            successesThisBlock = 0;
        }
    }

    /// @notice Attempt a withdrawal whose gas limit and callback size straddle the bounds.
    /// @dev Rolls a new block every 12th request so the cap (reset per block) can be hit by
    ///      accumulation and its reset exercised; forces an over-bounds request every 5th/7th
    ///      to drive the gas-limit / callback-size guards. Cadence is deterministic; the actual
    ///      gas/data/amount values stay fuzzed.
    function request(
        uint256 actorSeed,
        uint256 gasSeed,
        uint256 dataLenSeed,
        uint256 amountSeed
    )
        external
    {
        reqCount++;
        if (reqCount % 12 == 0) vm.roll(block.number + 1);
        _syncBlock();
        address holder = _actor(actorSeed);
        uint256 bal = token.balanceOf(holder);
        if (bal == 0) return;

        // Mostly within bounds (so the per-block cap is reachable), periodically over the
        // limit to exercise the gas-limit / callback-size guards.
        bool overGas = reqCount % 5 == 0;
        bool overData = reqCount % 7 == 0;
        uint64 gasLimit = overGas
            ? uint64(bound(gasSeed, uint256(maxGas) + 1, uint256(maxGas) * 2))
            : uint64(bound(gasSeed, 0, maxGas));
        uint256 dataLen = overData
            ? bound(dataLenSeed, maxData + 1, maxData * 2)
            : bound(dataLenSeed, 0, maxData);
        uint128 amount = uint128(bound(amountSeed, 1, bal < 1e6 ? bal : 1e6));
        bytes memory data = new bytes(dataLen);

        bool overBounds = overGas || overData;
        bool capFull = cap > 0 && successesThisBlock >= cap;

        vm.prank(holder);
        token.approve(address(outbox), amount);
        vm.prank(holder);
        try outbox.requestWithdrawal(
            address(token), holder, amount, bytes32(0), gasLimit, holder, data
        ) {
            // Stored only if within bounds and under the per-block cap.
            successesThisBlock++;
            stored++;
        } catch (bytes memory err) {
            bytes4 sel = bytes4(err);
            if (sel == ZoneOutbox.TooManyWithdrawalsThisBlock.selector) {
                capHits++;
            } else if (
                sel == ZoneOutbox.GasLimitTooHigh.selector
                    || sel == ZoneOutbox.CallbackDataTooLarge.selector
            ) {
                boundRejects++;
            }
            // Sanity: a within-bounds, under-cap request must never revert.
            require(overBounds || capFull, "valid withdrawal unexpectedly reverted");
        }
    }

    /// @notice Finalize the pending queue so the active range advances (head moves).
    function finalize(uint256) external {
        vm.prank(sequencer);
        uint256 pending = outbox.pendingWithdrawalsCount();
        if (pending == 0) return;
        bytes[] memory encryptedSenders = new bytes[](pending);
        vm.prank(sequencer);
        outbox.finalizeWithdrawalBatch(pending, uint64(block.number), encryptedSenders);
    }

}

/// @title ZoneWithdrawalLimitsInvariantTest
/// @notice Stateful invariants for stored-withdrawal bounds (TEMPO-ZONE-WITHDRAWAL-CALLBACK-BOUNDS:
///         gas limit and callback data size) and the per-block withdrawal cap.
contract ZoneWithdrawalLimitsInvariantTest is Test {

    ZoneOutboxHarness internal outbox;
    MockTempoState internal tempoState;
    MockZoneToken internal token;
    ZoneWithdrawalLimitsHandler internal handler;

    address internal constant SEQ = address(0x5e9);
    address internal constant MOCK_PORTAL = address(0x9999);
    address internal alice = address(0x200);
    address internal bob = address(0x300);
    address internal charlie = address(0x400);

    bytes32 constant GENESIS_TEMPO_BLOCK_HASH = keccak256("tempoGenesis");
    uint64 constant GENESIS_TEMPO_BLOCK_NUMBER = 1;
    uint256 constant CAP = 5;

    function setUp() public {
        MockZoneTxContext mockTxContext = new MockZoneTxContext();
        vm.etch(ZONE_TX_CONTEXT, address(mockTxContext).code);

        token = new MockZoneToken("Zone USD", "zUSD");
        tempoState = new MockTempoState(SEQ, GENESIS_TEMPO_BLOCK_HASH, GENESIS_TEMPO_BLOCK_NUMBER);
        tempoState.setMockStorageValue(
            MOCK_PORTAL, keccak256(abi.encode(SEQ, PORTAL_IS_SEQUENCER_SLOT)), bytes32(uint256(1))
        );
        tempoState.setMockTokenEnabled(MOCK_PORTAL, address(token), true);
        outbox = new ZoneOutboxHarness(MOCK_PORTAL, address(tempoState));

        token.setBurner(address(outbox), true);
        token.setMinter(address(this), true);
        token.mint(alice, 1e24);
        token.mint(bob, 1e24);
        token.mint(charlie, 1e24);
        token.setMinter(address(this), false);

        vm.prank(SEQ);
        outbox.setMaxWithdrawalsPerBlock(uint32(CAP));

        handler = new ZoneWithdrawalLimitsHandler(outbox, token, SEQ, alice, bob, charlie);
        targetContract(address(handler));
    }

    /// @notice TEMPO-ZONE-WITHDRAWAL-CALLBACK-BOUNDS: every stored pending withdrawal respects the gas-limit and
    ///         callback-data-size caps; an over-bounds request can never enter the queue.
    function invariant_storedWithdrawalBounds() public view {
        uint256 len = outbox.rawLength();
        for (uint256 i; i < len; i++) {
            assertLe(
                outbox.gasLimitAt(i),
                handler.maxGas(),
                "TEMPO-ZONE-WITHDRAWAL-CALLBACK-BOUNDS: stored withdrawal exceeds MAX_WITHDRAWAL_GAS_LIMIT"
            );
            assertLe(
                outbox.callbackLenAt(i),
                handler.maxData(),
                "TEMPO-ZONE-WITHDRAWAL-CALLBACK-BOUNDS: stored withdrawal exceeds MAX_CALLBACK_DATA_SIZE"
            );
        }
    }

    /// @notice Per-block withdrawal cap: no more than `maxWithdrawalsPerBlock` user requests are accepted per block.
    function invariant_perBlockWithdrawalCap() public view {
        assertLe(
            handler.successesThisBlock(), CAP, "accepted more withdrawals than the per-block cap"
        );
    }

    /// @notice Guard against a vacuous pass: bounds rejection, cap saturation, and successful
    ///         storage must all have been exercised.
    function afterInvariant() public view {
        assertGt(handler.stored(), 0, "no withdrawals were ever stored");
        assertGt(
            handler.boundRejects(),
            0,
            "TEMPO-ZONE-WITHDRAWAL-CALLBACK-BOUNDS: over-bounds requests never exercised"
        );
        assertGt(handler.capHits(), 0, "per-block cap was never hit");
    }

}
