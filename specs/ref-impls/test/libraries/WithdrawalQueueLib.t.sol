// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import { Withdrawal } from "../../src/interfaces/IZone.sol";
import {
    EMPTY_SENTINEL,
    WITHDRAWAL_QUEUE_CAPACITY,
    WithdrawalQueue,
    WithdrawalQueueLib
} from "../../src/libraries/WithdrawalQueueLib.sol";
import { Test } from "forge-std/Test.sol";

/// @title WithdrawalQueueHarness
/// @notice Test harness that wraps the library to convert memory to calldata
contract WithdrawalQueueHarness {

    using WithdrawalQueueLib for WithdrawalQueue;

    WithdrawalQueue internal queue;

    function enqueue(bytes32 withdrawalQueueHash) external {
        queue.enqueue(withdrawalQueueHash);
    }

    function dequeue(Withdrawal calldata withdrawal, bytes32 remainingQueue) external {
        queue.dequeue(withdrawal, remainingQueue);
    }

    function hasWithdrawals() external view returns (bool) {
        return queue.hasWithdrawals();
    }

    function length() external view returns (uint256) {
        return queue.length();
    }

    function head() external view returns (uint256) {
        return queue.head;
    }

    function tail() external view returns (uint256) {
        return queue.tail;
    }

    function slots(uint256 index) external view returns (bytes32) {
        return queue.slots[index];
    }

    function isFull() external view returns (bool) {
        return queue.isFull();
    }

}

/// @title WithdrawalQueueLibTest
/// @notice Direct tests for WithdrawalQueueLib functionality
contract WithdrawalQueueLibTest is Test {

    WithdrawalQueueHarness internal harness;

    address public alice = address(0x200);
    address public bob = address(0x300);
    address public charlie = address(0x400);

    function setUp() public {
        harness = new WithdrawalQueueHarness();
    }

    /*//////////////////////////////////////////////////////////////
                          INITIAL STATE TESTS
    //////////////////////////////////////////////////////////////*/

    function test_initialState() public view {
        assertEq(harness.head(), 0);
        assertEq(harness.tail(), 0);
        assertFalse(harness.hasWithdrawals());
        assertEq(harness.length(), 0);
    }

    /*//////////////////////////////////////////////////////////////
                            ENQUEUE TESTS
    //////////////////////////////////////////////////////////////*/

    function test_enqueue_singleBatch() public {
        Withdrawal memory w = _makeWithdrawal(alice, bob, 100e6);
        bytes32 wHash = keccak256(abi.encode(w, EMPTY_SENTINEL));

        harness.enqueue(wHash);

        assertEq(harness.head(), 0);
        assertEq(harness.tail(), 1);
        assertEq(harness.slots(0), wHash);
        assertTrue(harness.hasWithdrawals());
        assertEq(harness.length(), 1);
    }

    function test_enqueue_multipleBatches() public {
        bytes32 h1 = keccak256("batch1");
        bytes32 h2 = keccak256("batch2");
        bytes32 h3 = keccak256("batch3");

        harness.enqueue(h1);
        assertEq(harness.tail(), 1);

        harness.enqueue(h2);
        assertEq(harness.tail(), 2);

        harness.enqueue(h3);
        assertEq(harness.tail(), 3);

        assertEq(harness.slots(0), h1);
        assertEq(harness.slots(1), h2);
        assertEq(harness.slots(2), h3);
        assertEq(harness.length(), 3);
    }

    function test_enqueue_emptyTransition_noOp() public {
        harness.enqueue(bytes32(0));

        assertEq(harness.head(), 0);
        assertEq(harness.tail(), 0);
        assertFalse(harness.hasWithdrawals());
    }

    function test_enqueue_mixedEmptyAndNonEmpty() public {
        bytes32 h1 = keccak256("batch1");
        bytes32 h2 = keccak256("batch2");

        harness.enqueue(h1);
        assertEq(harness.tail(), 1);

        // Empty batch - no change
        harness.enqueue(bytes32(0));
        assertEq(harness.tail(), 1);

        harness.enqueue(h2);
        assertEq(harness.tail(), 2);

        // Slots should be contiguous
        assertEq(harness.slots(0), h1);
        assertEq(harness.slots(1), h2);
    }

    function test_enqueue_revertsWhenFull() public {
        for (uint256 i = 0; i < WITHDRAWAL_QUEUE_CAPACITY; i++) {
            harness.enqueue(keccak256(abi.encode("b", i)));
        }
        assertEq(harness.length(), WITHDRAWAL_QUEUE_CAPACITY);

        vm.expectRevert(WithdrawalQueueLib.WithdrawalQueueFull.selector);
        harness.enqueue(keccak256("overflow"));
    }

    function test_enqueue_afterDequeueReuseSlots() public {
        Withdrawal memory w1 = _makeWithdrawal(alice, bob, 100e6);
        bytes32 h1 = keccak256(abi.encode(w1, EMPTY_SENTINEL));

        // Fill all slots
        harness.enqueue(h1);
        for (uint256 i = 1; i < WITHDRAWAL_QUEUE_CAPACITY; i++) {
            harness.enqueue(keccak256(abi.encode("b", i)));
        }
        assertEq(harness.length(), WITHDRAWAL_QUEUE_CAPACITY);

        // Dequeue first to free a slot
        harness.dequeue(w1, bytes32(0));
        assertEq(harness.length(), WITHDRAWAL_QUEUE_CAPACITY - 1);

        // Enqueue again — should succeed since we freed a slot
        bytes32 hNew = keccak256("new");
        harness.enqueue(hNew);
        assertEq(harness.length(), WITHDRAWAL_QUEUE_CAPACITY);

        // hNew should be written to slots[tail % capacity] = slots[CAPACITY % CAPACITY] = slots[0]
        assertEq(harness.slots(0), hNew);
    }

    /*//////////////////////////////////////////////////////////////
                            DEQUEUE TESTS
    //////////////////////////////////////////////////////////////*/

    function test_dequeue_singleWithdrawal() public {
        Withdrawal memory w = _makeWithdrawal(alice, bob, 100e6);
        bytes32 wHash = keccak256(abi.encode(w, EMPTY_SENTINEL));

        harness.enqueue(wHash);

        harness.dequeue(w, bytes32(0));

        assertEq(harness.head(), 1);
        assertEq(harness.tail(), 1);
        assertEq(harness.slots(0), EMPTY_SENTINEL);
        assertFalse(harness.hasWithdrawals());
    }

    function test_dequeue_multipleWithdrawalsInBatch() public {
        Withdrawal memory w1 = _makeWithdrawal(alice, bob, 100e6);
        Withdrawal memory w2 = _makeWithdrawal(bob, charlie, 200e6);

        // Build queue: w1 outermost, w2 innermost (wraps EMPTY_SENTINEL)
        bytes32 innerHash = keccak256(abi.encode(w2, EMPTY_SENTINEL));
        bytes32 batchHash = keccak256(abi.encode(w1, innerHash));

        harness.enqueue(batchHash);

        // Dequeue w1
        harness.dequeue(w1, innerHash);
        assertEq(harness.head(), 0); // Still on slot 0
        assertEq(harness.slots(0), innerHash);

        // Dequeue w2
        harness.dequeue(w2, bytes32(0));
        assertEq(harness.head(), 1);
        assertEq(harness.slots(0), EMPTY_SENTINEL);
    }

    function test_dequeue_multipleSlots() public {
        Withdrawal memory w1 = _makeWithdrawal(alice, bob, 100e6);
        Withdrawal memory w2 = _makeWithdrawal(bob, charlie, 200e6);

        bytes32 h1 = keccak256(abi.encode(w1, EMPTY_SENTINEL));
        bytes32 h2 = keccak256(abi.encode(w2, EMPTY_SENTINEL));

        harness.enqueue(h1);
        harness.enqueue(h2);

        // Dequeue from slot 0
        harness.dequeue(w1, bytes32(0));
        assertEq(harness.head(), 1);
        assertEq(harness.length(), 1);

        // Dequeue from slot 1
        harness.dequeue(w2, bytes32(0));
        assertEq(harness.head(), 2);
        assertEq(harness.length(), 0);
    }

    function test_dequeue_revertsIfEmpty() public {
        Withdrawal memory w = _makeWithdrawal(alice, bob, 100e6);

        vm.expectRevert(WithdrawalQueueLib.NoWithdrawalsInQueue.selector);
        harness.dequeue(w, bytes32(0));
    }

    function test_dequeue_revertsIfInvalidHash() public {
        Withdrawal memory w1 = _makeWithdrawal(alice, bob, 100e6);
        Withdrawal memory w2 = _makeWithdrawal(bob, charlie, 200e6);

        bytes32 h1 = keccak256(abi.encode(w1, EMPTY_SENTINEL));
        harness.enqueue(h1);

        // Try to dequeue w2 (wrong withdrawal)
        vm.expectRevert(WithdrawalQueueLib.InvalidWithdrawalHash.selector);
        harness.dequeue(w2, bytes32(0));
    }

    function test_dequeue_revertsIfWrongRemainingQueue() public {
        Withdrawal memory w1 = _makeWithdrawal(alice, bob, 100e6);
        Withdrawal memory w2 = _makeWithdrawal(bob, charlie, 200e6);

        bytes32 innerHash = keccak256(abi.encode(w2, EMPTY_SENTINEL));
        bytes32 batchHash = keccak256(abi.encode(w1, innerHash));

        harness.enqueue(batchHash);

        // Try to dequeue with wrong remaining queue
        vm.expectRevert(WithdrawalQueueLib.InvalidWithdrawalHash.selector);
        harness.dequeue(w1, keccak256("wrongHash"));
    }

    /*//////////////////////////////////////////////////////////////
                      REVERT WHEN FULL TESTS
    //////////////////////////////////////////////////////////////*/

    function test_enqueue_emptyTransitionSucceedsWhenFull() public {
        for (uint256 i = 0; i < WITHDRAWAL_QUEUE_CAPACITY; i++) {
            harness.enqueue(keccak256(abi.encode("b", i)));
        }
        assertEq(harness.head(), 0);
        assertEq(harness.tail(), WITHDRAWAL_QUEUE_CAPACITY);
        assertEq(harness.length(), WITHDRAWAL_QUEUE_CAPACITY);

        harness.enqueue(bytes32(0));

        assertEq(harness.head(), 0);
        assertEq(harness.tail(), WITHDRAWAL_QUEUE_CAPACITY);
        assertEq(harness.length(), WITHDRAWAL_QUEUE_CAPACITY);
    }

    function test_ringBuffer_multiCycleWraparound() public {
        Withdrawal[] memory ws = new Withdrawal[](4);
        ws[0] = _makeWithdrawal(alice, bob, 100e6);
        ws[1] = _makeWithdrawal(bob, charlie, 200e6);
        ws[2] = _makeWithdrawal(alice, charlie, 300e6);
        ws[3] = _makeWithdrawal(charlie, alice, 400e6);

        bytes32[] memory hs = new bytes32[](4);
        for (uint256 i = 0; i < 4; i++) {
            hs[i] = keccak256(abi.encode(ws[i], EMPTY_SENTINEL));
        }

        // Fill to capacity
        harness.enqueue(hs[0]);
        harness.enqueue(hs[1]);
        for (uint256 i = 2; i < WITHDRAWAL_QUEUE_CAPACITY; i++) {
            harness.enqueue(keccak256(abi.encode("fill", i)));
        }
        assertEq(harness.head(), 0);
        assertEq(harness.tail(), WITHDRAWAL_QUEUE_CAPACITY);

        // Dequeue first (head=1), enqueue C (tail=CAPACITY, slot 0)
        harness.dequeue(ws[0], bytes32(0));
        harness.enqueue(hs[2]);
        assertEq(harness.head(), 1);
        assertEq(harness.tail(), WITHDRAWAL_QUEUE_CAPACITY + 1);
        assertEq(harness.slots(0), hs[2]); // slot 0 reused

        // Dequeue second (head=2), enqueue D (tail=CAPACITY+1, slot 1)
        harness.dequeue(ws[1], bytes32(0));
        harness.enqueue(hs[3]);
        assertEq(harness.head(), 2);
        assertEq(harness.tail(), WITHDRAWAL_QUEUE_CAPACITY + 2);
        assertEq(harness.slots(1), hs[3]); // slot 1 reused

        // Verify wrapping worked by checking slot contents
        assertEq(harness.slots(0), hs[2]);
        assertEq(harness.slots(1), hs[3]);
        assertEq(harness.length(), WITHDRAWAL_QUEUE_CAPACITY);
    }

    /// @notice A full queue dequeues every withdrawal in FIFO order and empties.
    function test_enqueueDequeue_fullCapacityInFifoOrder() public {
        Withdrawal[] memory withdrawals = new Withdrawal[](WITHDRAWAL_QUEUE_CAPACITY);

        for (uint256 i = 0; i < WITHDRAWAL_QUEUE_CAPACITY; i++) {
            withdrawals[i] = _makeWithdrawal(alice, bob, uint128(i + 1));
            harness.enqueue(keccak256(abi.encode(withdrawals[i], EMPTY_SENTINEL)));
            assertEq(harness.length(), i + 1);
        }

        assertEq(harness.length(), WITHDRAWAL_QUEUE_CAPACITY);

        for (uint256 i = 0; i < WITHDRAWAL_QUEUE_CAPACITY; i++) {
            harness.dequeue(withdrawals[i], bytes32(0));
            assertEq(harness.length(), WITHDRAWAL_QUEUE_CAPACITY - i - 1);
        }

        assertFalse(harness.hasWithdrawals());
        assertEq(harness.head(), WITHDRAWAL_QUEUE_CAPACITY);
        assertEq(harness.tail(), WITHDRAWAL_QUEUE_CAPACITY);
    }

    /// @notice Dequeuing the last item marks the exhausted slot empty.
    function test_dequeue_setsEmptySentinelWhenSlotExhausted() public {
        Withdrawal memory w = _makeWithdrawal(alice, bob, 100e6);

        harness.enqueue(keccak256(abi.encode(w, EMPTY_SENTINEL)));
        harness.dequeue(w, bytes32(0));

        assertEq(harness.slots(0), EMPTY_SENTINEL);
        assertEq(harness.head(), 1);
        assertEq(harness.length(), 0);
    }

    /// @notice Fuzzed enqueues and dequeues preserve FIFO position and length.
    function testFuzz_enqueueDequeue_preservesFifoAndLength(bytes32 seed) public {
        uint256 count = (uint256(seed) % WITHDRAWAL_QUEUE_CAPACITY) + 1;
        Withdrawal[] memory withdrawals = new Withdrawal[](count);

        for (uint256 i = 0; i < count; i++) {
            uint128 amount = uint128(uint256(keccak256(abi.encode(seed, "amount", i))));
            if (amount == 0) amount = 1;
            withdrawals[i] = _makeWithdrawal(
                address(uint160(uint256(keccak256(abi.encode(seed, "sender", i))))),
                address(uint160(uint256(keccak256(abi.encode(seed, "to", i))))),
                amount
            );
            harness.enqueue(keccak256(abi.encode(withdrawals[i], EMPTY_SENTINEL)));
            assertEq(harness.length(), i + 1);
        }

        uint256 dequeues = uint256(keccak256(abi.encode(seed, "dequeues"))) % (count + 1);
        for (uint256 i = 0; i < dequeues; i++) {
            harness.dequeue(withdrawals[i], bytes32(0));
            assertEq(harness.length(), count - i - 1);
            assertEq(harness.head(), i + 1);
        }

        if (dequeues == count) {
            assertFalse(harness.hasWithdrawals());
        } else {
            assertTrue(harness.hasWithdrawals());
            assertEq(harness.head(), dequeues);
            assertEq(harness.tail(), count);
        }
    }

    /*//////////////////////////////////////////////////////////////
                        LENGTH & HAS WITHDRAWALS
    //////////////////////////////////////////////////////////////*/

    function test_length_accurate() public {
        assertEq(harness.length(), 0);

        harness.enqueue(keccak256("b1"));
        assertEq(harness.length(), 1);

        harness.enqueue(keccak256("b2"));
        assertEq(harness.length(), 2);
    }

    function test_hasWithdrawals_accurate() public {
        assertFalse(harness.hasWithdrawals());

        harness.enqueue(keccak256("b1"));
        assertTrue(harness.hasWithdrawals());
    }

    /*//////////////////////////////////////////////////////////////
                            isFull TESTS
    //////////////////////////////////////////////////////////////*/

    /// @notice isFull is true only at exactly capacity, false otherwise.
    /// @dev Kills mutants on `tail - head == CAPACITY`: `==`->`!=`, `==`->`<`.
    function test_isFull_trueOnlyAtCapacity() public {
        assertFalse(harness.isFull()); // empty: length 0

        for (uint256 i = 0; i < WITHDRAWAL_QUEUE_CAPACITY - 1; i++) {
            harness.enqueue(keccak256(abi.encode("b", i)));
            assertFalse(harness.isFull()); // below capacity stays false
        }

        harness.enqueue(keccak256("last"));
        assertTrue(harness.isFull()); // exactly capacity
    }

    /// @notice isFull uses tail - head (a difference), not a bitwise/other op.
    /// @dev With a non-zero head, `tail & head` and similar diverge from `tail - head`,
    ///      so asserting isFull in a wrapped full state kills those arithmetic mutants.
    function test_isFull_trueWithNonZeroHead() public {
        Withdrawal memory w0 = _makeWithdrawal(alice, bob, 100e6);
        harness.enqueue(keccak256(abi.encode(w0, EMPTY_SENTINEL)));
        for (uint256 i = 1; i < WITHDRAWAL_QUEUE_CAPACITY; i++) {
            harness.enqueue(keccak256(abi.encode("b", i)));
        }
        // Free one slot then refill so head advances past zero while staying full.
        harness.dequeue(w0, bytes32(0));
        harness.enqueue(keccak256("refill"));

        assertEq(harness.head(), 1);
        assertEq(harness.tail(), WITHDRAWAL_QUEUE_CAPACITY + 1);
        assertEq(harness.length(), WITHDRAWAL_QUEUE_CAPACITY);
        assertTrue(harness.isFull()); // tail-head == 100; tail & head == 101 & 1 == 1
    }

    /// @notice enqueue reverts when full even after head has advanced past zero.
    /// @dev Kills the `tail - head` -> `tail >> head` mutant on the full-check: with
    ///      head=1, tail=101 the real length is 100 (revert) but `101 >> 1 == 50` would
    ///      not, so the mutant would wrongly accept the enqueue.
    function test_enqueue_revertsWhenFullWithNonZeroHead() public {
        Withdrawal memory w0 = _makeWithdrawal(alice, bob, 100e6);
        harness.enqueue(keccak256(abi.encode(w0, EMPTY_SENTINEL)));
        for (uint256 i = 1; i < WITHDRAWAL_QUEUE_CAPACITY; i++) {
            harness.enqueue(keccak256(abi.encode("b", i)));
        }
        harness.dequeue(w0, bytes32(0)); // head = 1, length 99
        harness.enqueue(keccak256("refill")); // tail = 101, length 100 (full again)

        assertEq(harness.head(), 1);
        assertEq(harness.tail(), WITHDRAWAL_QUEUE_CAPACITY + 1);
        vm.expectRevert(WithdrawalQueueLib.WithdrawalQueueFull.selector);
        harness.enqueue(keccak256("overflow"));
    }

    /// @notice dequeue rejects a wrong withdrawal whose hash is numerically below the
    ///         stored slot, not just above it.
    /// @dev Kills the `!= currentSlot` -> `> currentSlot` mutant: a strict `>` check
    ///      would let a wrong withdrawal with hash < slot pass. We search for such a
    ///      withdrawal so the inequality direction is actually exercised.
    function test_dequeue_revertsIfWrongHashBelowSlot() public {
        Withdrawal memory real = _makeWithdrawal(alice, bob, 100e6);
        bytes32 slot = keccak256(abi.encode(real, EMPTY_SENTINEL));
        harness.enqueue(slot);

        // Find a wrong withdrawal whose hash is strictly less than the stored slot.
        Withdrawal memory wrong;
        bool found;
        for (uint128 amount = 1; amount < 2000; amount++) {
            wrong = _makeWithdrawal(bob, charlie, amount);
            bytes32 wrongHash = keccak256(abi.encode(wrong, EMPTY_SENTINEL));
            if (wrongHash < slot && wrongHash != slot) {
                found = true;
                break;
            }
        }
        assertTrue(found, "no wrong hash below slot found in search range");

        vm.expectRevert(WithdrawalQueueLib.InvalidWithdrawalHash.selector);
        harness.dequeue(wrong, bytes32(0));
    }

    /*//////////////////////////////////////////////////////////////
                            HELPER FUNCTIONS
    //////////////////////////////////////////////////////////////*/

    function _makeWithdrawal(
        address sender,
        address to,
        uint128 amount
    )
        internal
        pure
        returns (Withdrawal memory)
    {
        return Withdrawal({
            token: address(0x100),
            senderTag: keccak256(abi.encodePacked(sender)),
            to: to,
            amount: amount,
            fee: 0,
            memo: bytes32(0),
            gasLimit: 0,
            fallbackRecipient: sender,
            callbackData: "",
            encryptedSender: ""
        });
    }

}
