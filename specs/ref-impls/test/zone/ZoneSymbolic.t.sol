// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import { ENCRYPTION_KEY_GRACE_PERIOD, EncryptionKeyEntry } from "../../src/interfaces/IZone.sol";
import { EncryptedDepositLib } from "../../src/libraries/EncryptedDeposit.sol";
import {
    WITHDRAWAL_QUEUE_CAPACITY,
    WithdrawalQueue,
    WithdrawalQueueLib
} from "../../src/libraries/WithdrawalQueueLib.sol";
import { ZonePortal } from "../../src/tempo/ZonePortal.sol";
import { ZonePortalTest } from "../tempo/ZonePortal.t.sol";
import { ZoneOutboxTest } from "./ZoneOutbox.t.sol";
import { Test } from "forge-std/Test.sol";

/// @title ZonePortal symbolic properties
/// @notice Curated symbolic (`check_*`) properties for the arithmetic-heavy parts of the zone
///         contracts. These complement the unit/fuzz suites: each `check_*` is explored over the
///         full symbolic input space (within configured bounds) rather than sampled.
///
///         Run with the symbolic-capable forge build:
///           forge test --symbolic --match-contract ZonePortalSymbolic
///           forge test --symbolic --match-contract WithdrawalQueueSymbolic
///
///         Guidance: prefer inequality / overflow-freedom / revert-freedom properties. Exact
///         equalities involving multiplication tend to return `incomplete` (the engine's
///         nonlinear "hard arithmetic" gap), which means "not established" — never treat it as a
///         pass.
///
///         Inherits ZonePortalTest to reuse its concrete setUp (a real ZonePortal deployed via
///         ZoneFactory, with separate portal admin and sequencer roles).
contract ZonePortalSymbolic is ZonePortalTest {

    /// @notice At the configured maximum rate the deposit fee still fits the uint128 return
    ///         type. The rate is concrete so the solver only has to establish a cast boundary,
    ///         not a multiplication of two symbolic values.
    function check_depositFeeAtRateCapFitsCast() external {
        uint128 rate = portal.MAX_GAS_FEE_RATE();
        vm.prank(admin);
        portal.setZoneGasRate(rate);

        uint256 wideFee = uint256(portal.FIXED_DEPOSIT_GAS()) * rate;
        assertLe(wideFee, type(uint128).max);
        assertEq(portal.calculateDepositFee(), uint128(wideFee));
    }

    /// @notice The bounce-back ceil-division is still exactly representable at the uint128 cast
    ///         boundary. Both factors are concrete to keep this out of nonlinear arithmetic.
    function check_bouncebackFeeAtUint128CastBoundary() external {
        vm.prank(admin);
        portal.setBouncebackGas(1);
        vm.fee(uint256(type(uint128).max) * 1e12);

        assertEq(portal.calculateBouncebackFee(), type(uint128).max);
    }

    /// @notice Deposit fee never overflows uint128 for any rate within the enforced cap, so
    ///         `calculateDepositFee` cannot revert. Proven over all 2^128 rate values.
    function check_depositFeeNeverOverflows(uint128 rate) external {
        vm.assume(rate <= portal.MAX_GAS_FEE_RATE());

        vm.prank(admin);
        portal.setZoneGasRate(rate);

        uint128 fee = portal.calculateDepositFee();
        assertLe(uint256(fee), uint256(type(uint128).max));
    }

    /// @notice The stored gas rate is always within the cap whenever `setZoneGasRate` succeeds,
    ///         for any input (over-cap inputs revert and are pruned). Encodes the MAX_GAS_FEE_RATE
    ///         invariant.
    function check_gasRateAlwaysWithinCap(uint128 rate) external {
        vm.prank(admin);
        try portal.setZoneGasRate(rate) {
            assertLe(uint256(portal.zoneGasRate()), uint256(portal.MAX_GAS_FEE_RATE()));
        } catch { }
    }

    /// @notice The admin-configured sequencer ceiling never exceeds the protocol maximum.
    function check_maxTempoGasRateAlwaysWithinProtocolCap(uint128 rate) external {
        vm.prank(admin);
        try portal.setMaxTempoGasRate(rate) {
            assertLe(uint256(portal.maxTempoGasRate()), uint256(portal.MAX_GAS_FEE_RATE()));
        } catch { }
    }

}

/// @notice Harness exposing the WithdrawalQueueLib ring-buffer over a storage queue so its
///         index arithmetic can be explored symbolically.
contract WithdrawalQueueHarness {

    using WithdrawalQueueLib for WithdrawalQueue;

    WithdrawalQueue internal q;

    function setHeadTail(uint256 _head, uint256 _tail) external {
        q.head = _head;
        q.tail = _tail;
    }

    function head() external view returns (uint256) {
        return q.head;
    }

    function tail() external view returns (uint256) {
        return q.tail;
    }

    function slot(uint256 index) external view returns (bytes32) {
        return q.slots[index];
    }

    function length() external view returns (uint256) {
        return q.length();
    }

    function isFull() external view returns (bool) {
        return q.isFull();
    }

    function hasWithdrawals() external view returns (bool) {
        return q.hasWithdrawals();
    }

    function enqueue(bytes32 h) external {
        q.enqueue(h);
    }

    function capacity() external pure returns (uint256) {
        return WITHDRAWAL_QUEUE_CAPACITY;
    }

}

/// @title WithdrawalQueueLib symbolic properties
/// @notice Symbolic checks for the withdrawal ring-buffer's pure index arithmetic
///         (head/tail). The dequeue hash-chain path is intentionally excluded because it relies
///         on keccak injectivity, which the symbolic engine does not model.
contract WithdrawalQueueSymbolic is Test {

    WithdrawalQueueHarness internal qh;

    function setUp() public {
        qh = new WithdrawalQueueHarness();
    }

    /// @notice For any valid queue state (head <= tail, length <= capacity),
    ///         isFull() <=> length() == capacity.
    function check_isFullIffLengthEqualsCapacity(uint256 _head, uint256 _tail) external {
        vm.assume(_tail >= _head);
        vm.assume(_tail - _head <= qh.capacity());

        qh.setHeadTail(_head, _tail);

        assertEq(qh.isFull(), qh.length() == qh.capacity());
    }

    /// @notice For any valid queue state, hasWithdrawals() <=> length() != 0.
    function check_hasWithdrawalsIffNonEmpty(uint256 _head, uint256 _tail) external {
        vm.assume(_tail >= _head);
        vm.assume(_tail - _head <= qh.capacity());

        qh.setHeadTail(_head, _tail);

        assertEq(qh.hasWithdrawals(), qh.length() != 0);
    }

    /// @notice A non-empty enqueue on a non-full queue advances tail by exactly one and never
    ///         pushes length past capacity.
    function check_enqueueAdvancesTailAndRespectsCapacity(
        uint256 _head,
        uint256 _tail,
        bytes32 h
    )
        external
    {
        vm.assume(h != bytes32(0));
        vm.assume(_tail >= _head);
        vm.assume(_tail - _head < qh.capacity()); // not full
        vm.assume(_tail < type(uint256).max); // tail + 1 cannot overflow

        qh.setHeadTail(_head, _tail);
        uint256 lenBefore = qh.length();

        qh.enqueue(h);

        assertEq(qh.tail(), _tail + 1);
        assertEq(qh.length(), lenBefore + 1);
        assertLe(qh.length(), qh.capacity());
    }

    /// @notice Enqueuing the zero hash (a batch with no withdrawals) is a no-op: head and tail
    ///         are unchanged, for any starting state.
    function check_enqueueZeroIsNoop(uint256 _head, uint256 _tail) external {
        qh.setHeadTail(_head, _tail);

        qh.enqueue(bytes32(0));

        assertEq(qh.head(), _head);
        assertEq(qh.tail(), _tail);
    }

    /// @notice A non-empty enqueue on a full queue always reverts, for any full state.
    function check_enqueueRevertsWhenFull(uint256 _head, uint256 _tail, bytes32 h) external {
        vm.assume(h != bytes32(0));
        vm.assume(_tail >= _head);
        vm.assume(_tail - _head == qh.capacity()); // full

        qh.setHeadTail(_head, _tail);

        vm.expectRevert(WithdrawalQueueLib.WithdrawalQueueFull.selector);
        qh.enqueue(h);
    }

    /// @notice A non-full queue at the maximum tail cannot wrap, and the failed enqueue is atomic.
    function check_enqueueAtMaxTailRevertsWithoutMutation(bytes32 h) external {
        vm.assume(h != bytes32(0));
        uint256 tail = type(uint256).max;
        uint256 head = tail - qh.capacity() + 1;
        uint256 slotIndex = tail % qh.capacity();
        qh.setHeadTail(head, tail);
        bytes32 slotBefore = qh.slot(slotIndex);

        vm.expectRevert(abi.encodeWithSignature("Panic(uint256)", 0x11));
        qh.enqueue(h);

        assertEq(qh.head(), head);
        assertEq(qh.tail(), tail);
        assertEq(qh.slot(slotIndex), slotBefore);
    }

}

/// @title ZoneOutbox symbolic properties
/// @notice Symbolic checks for the zone→Tempo withdrawal fee arithmetic. Inherits ZoneOutboxTest
///         to reuse its concrete setUp (real ZoneOutbox + ZoneConfig, `sequencer` authorized).
contract ZoneOutboxSymbolic is ZoneOutboxTest {

    /// @notice At both callback-gas boundaries the fee fits uint128 and the cast is lossless at
    ///         the maximum configured rate.
    function check_withdrawalFeeCastFitsAtGasBoundaries(bool useMaximum) external {
        uint64 gasLimit = useMaximum ? outbox.MAX_WITHDRAWAL_GAS_LIMIT() : 0;
        uint128 rate = config.maxTempoGasRate();
        vm.prank(sequencer);
        outbox.setTempoGasRate(rate);

        uint256 wideFee = uint256(outbox.WITHDRAWAL_BASE_GAS() + gasLimit) * rate;
        assertLe(wideFee, type(uint128).max);
        assertEq(outbox.calculateWithdrawalFee(gasLimit), uint128(wideFee));
    }

    /// @notice The withdrawal fee `(WITHDRAWAL_BASE_GAS + gasLimit) * tempoGasRate` never
    ///         overflows uint128, so `calculateWithdrawalFee` cannot revert. Verifies the
    ///         overflow-safety invariant the contract documents, explored over all 2^64 gas
    ///         limits.
    /// @dev The rate is pinned to its admin-configured maximum because the fee is monotonic in
    ///      the rate, so the cap is the worst case for overflow: proving no overflow here proves
    ///      it for every rate <= cap. Pinning the rate also keeps the multiplication linear
    ///      (constant * symbolic); leaving both operands symbolic hits the engine's nonlinear
    ///      "hard arithmetic" gap and returns `incomplete`.
    function check_withdrawalFeeNeverOverflows(uint64 gasLimit) external {
        vm.assume(gasLimit <= outbox.MAX_WITHDRAWAL_GAS_LIMIT());

        uint128 cap = config.maxTempoGasRate();
        vm.prank(sequencer);
        outbox.setTempoGasRate(cap);

        uint128 fee = outbox.calculateWithdrawalFee(gasLimit);
        assertLe(uint256(fee), uint256(type(uint128).max));
    }

    /// @notice The stored Tempo gas rate never exceeds the portal-admin ceiling whenever
    ///         `setTempoGasRate` succeeds, for any input.
    function check_tempoGasRateAlwaysWithinCap(uint128 rate) external {
        vm.prank(sequencer);
        try outbox.setTempoGasRate(rate) {
            assertLe(uint256(outbox.tempoGasRate()), uint256(config.maxTempoGasRate()));
        } catch { }
    }

    /// @notice `calculateWithdrawalFee` rejects any gas limit above MAX_WITHDRAWAL_GAS_LIMIT,
    ///         for every over-cap value.
    function check_withdrawalFeeRejectsOverCapGasLimit(uint64 gasLimit) external {
        vm.assume(gasLimit > outbox.MAX_WITHDRAWAL_GAS_LIMIT());

        try outbox.calculateWithdrawalFee(gasLimit) returns (uint128) {
            fail(); // an over-cap gas limit must never produce a fee
        } catch { }
    }

}

/// @notice Exposes production ZonePortal key-history storage setup to the symbolic properties.
contract EncryptionGraceHarness is ZonePortal {

    function setTwoKeys(uint64 supersedingActivation) external {
        delete _encryptionKeys;
        _encryptionKeys.push(
            EncryptionKeyEntry({ x: bytes32(uint256(1)), yParity: 0x02, activationBlock: 0 })
        );
        _encryptionKeys.push(
            EncryptionKeyEntry({
                x: bytes32(uint256(2)), yParity: 0x03, activationBlock: supersedingActivation
            })
        );
    }

}

contract EncryptionGraceSymbolic is Test {

    EncryptionGraceHarness internal h;

    function setUp() public {
        h = new EncryptionGraceHarness();
    }

    /// @notice Production key validity changes exactly at the superseding key's grace boundary.
    function check_encryptionGraceExpiresAtExactBoundary(
        uint64 activation,
        uint256 currentBlock
    )
        external
    {
        vm.assume(activation <= type(uint64).max - ENCRYPTION_KEY_GRACE_PERIOD);
        h.setTwoKeys(activation);
        vm.roll(currentBlock);

        (bool valid, uint64 expiry) = h.isEncryptionKeyValid(0);
        assertEq(expiry, uint256(activation) + ENCRYPTION_KEY_GRACE_PERIOD);
        assertEq(valid, currentBlock < expiry);
    }

    /// @notice An overflowing grace-period calculation reverts without altering key history.
    function check_encryptionGraceAdditionCannotWrap(uint64 activation) external {
        vm.assume(activation > type(uint64).max - ENCRYPTION_KEY_GRACE_PERIOD);
        h.setTwoKeys(activation);

        try h.isEncryptionKeyValid(0) returns (bool, uint64) {
            fail();
        } catch { }
        assertEq(h.encryptionKeyAt(1).activationBlock, activation);
    }

    /// @notice Production key lookup accepts stored indices and safely rejects all others.
    function check_encryptionKeyIndexRange(uint256 index) external {
        h.setTwoKeys(1);
        (bool valid, uint64 expiry) = h.isEncryptionKeyValid(index);
        if (index >= 2) {
            assertFalse(valid);
            assertEq(expiry, 0);
        } else if (index == 1) {
            assertTrue(valid);
            assertEq(expiry, 0);
        }
    }

}

/// @notice Harness exposing the EncryptedDepositLib plaintext packing helpers so the
///         encode/decode assembly can be explored symbolically.
contract EncryptedDepositHarness {

    function roundtrip(address to, bytes32 memo) external pure returns (address, bytes32) {
        return EncryptedDepositLib.decodePlaintext(EncryptedDepositLib.encodePlaintext(to, memo));
    }

}

/// @title EncryptedDeposit symbolic properties
/// @notice Symbolic check for the (to, memo) plaintext packing round-trip. Pure byte manipulation
///         (no keccak, no external calls) — a clean symbolic-execution target.
contract EncryptedDepositSymbolic is Test {

    EncryptedDepositHarness internal h;

    function setUp() public {
        h = new EncryptedDepositHarness();
    }

    /// @notice decode(encode(to, memo)) == (to, memo) for every address and memo. Catches any
    ///         offset/packing bug in the assembly layout.
    function check_plaintextRoundTrip(address to, bytes32 memo) external view {
        (address gotTo, bytes32 gotMemo) = h.roundtrip(to, memo);
        assertEq(gotTo, to);
        assertEq(gotMemo, memo);
    }

}
