// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import {
    BlockTransition,
    DecryptionData,
    Deposit,
    DepositQueueTransition,
    DepositType,
    EnabledToken,
    PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT,
    PORTAL_IS_SEQUENCER_SLOT,
    QueuedDeposit,
    Withdrawal,
    ZONE_TX_CONTEXT
} from "../../src/interfaces/IZone.sol";
import {
    EMPTY_SENTINEL,
    WITHDRAWAL_QUEUE_CAPACITY
} from "../../src/libraries/WithdrawalQueueLib.sol";
import { ZoneMessenger } from "../../src/tempo/ZoneMessenger.sol";
import { ZonePortal } from "../../src/tempo/ZonePortal.sol";
import { ZoneConfig } from "../../src/zone/ZoneConfig.sol";
import { ZoneInbox } from "../../src/zone/ZoneInbox.sol";
import { ZoneOutbox } from "../../src/zone/ZoneOutbox.sol";
import { BaseTest } from "../BaseTest.t.sol";
import { MockTempoState } from "../mocks/MockTempoState.sol";
import { MockZoneToken } from "../mocks/MockZoneToken.sol";
import { MockZoneTxContext } from "../mocks/MockZoneTxContext.sol";
import { Test } from "forge-std/Test.sol";

/// @title ZoneLifecycleHandler
/// @notice Honest-sequencer driver for the zone deposit/withdrawal lifecycle. Every action
///         mirrors the cross-domain state the sequencer is trusted to maintain and asserts the
///         on-chain hash chains match the mirror, so the fuzzer can only explore honest
///         interleavings (the stub verifier and empty deposit-contiguity check would otherwise
///         let it model a malicious sequencer).
contract ZoneLifecycleHandler is Test {

    ZonePortal internal immutable portal;
    ZoneInbox internal immutable inbox;
    ZoneOutbox internal immutable outbox;
    MockZoneToken internal immutable token;
    MockTempoState internal immutable tempoState;
    MockZoneTxContext internal constant txCtx = MockZoneTxContext(ZONE_TX_CONTEXT);

    address internal immutable sequencer;
    address[3] internal actors;

    // Ghost ledger (honest-sequencer expectation of cross-domain state).
    uint256 public initialSupply;
    uint256 public minted; // cumulative ZoneInbox mints after setUp
    uint256 public burned; // cumulative ZoneOutbox burns after setUp
    uint256 public escrowIn; // cumulative net deposits into portal escrow
    uint256 public escrowOut; // cumulative escrow released by processWithdrawal
    mapping(address => uint256) public zoneCredit; // minted-and-unburned balance per actor

    // Deposit mirror (enqueued on L1, awaiting zone-side advanceTempo).
    Deposit[] internal depositMirror;
    uint256 internal depositHead;
    bytes32 internal mirrorDepositHash; // matches portal.currentDepositQueueHash()
    bytes32 internal mirrorProcessedHash; // matches inbox.processedDepositQueueHash()

    // Withdrawal mirror (requested+burned on the zone, awaiting finalize+submit).
    Withdrawal[] internal pendingWithdrawals;
    uint256 internal withdrawalFinalizeHead;

    // Submitted L1 batches awaiting processWithdrawal (FIFO, one ring-buffer slot each).
    struct Batch {
        Withdrawal[] ws;
        uint256 head;
    }

    Batch[] internal batches;
    uint256 internal batchHead;
    uint256 internal blockNonce;

    // Coverage counters (proves each lifecycle leg actually executed real work, not just
    // hit an early return). Guards the invariants against a vacuous pass.
    uint256 public numDeposits;
    uint256 public numAdvances; // advanceDeposits calls that processed >=1 deposit
    uint256 public numDepositsProcessed; // total deposits minted on the zone
    uint256 public numWithdrawalRequests;
    uint256 public numFinalizes;
    uint256 public numWithdrawalsProcessed;

    // High-water marks for monotonic counters (TEMPO-ZONE-WITHDRAWAL-BATCH-INDEX, TEMPO-ZONE-DEPOSIT-NUMBER-MONOTONIC).
    uint64 internal prevDepositCount;
    uint64 internal prevNextWithdrawalIndex;
    uint64 internal prevPortalBatchIndex;
    uint64 internal prevOutboxBatchIndex;

    constructor(
        ZonePortal _portal,
        ZoneInbox _inbox,
        ZoneOutbox _outbox,
        MockZoneToken _token,
        MockTempoState _tempoState,
        address _sequencer,
        address _alice,
        address _bob,
        address _charlie
    ) {
        portal = _portal;
        inbox = _inbox;
        outbox = _outbox;
        token = _token;
        tempoState = _tempoState;
        sequencer = _sequencer;
        actors = [_alice, _bob, _charlie];
        initialSupply = _token.totalSupply();
    }

    function outstandingSupply() public view returns (uint256) {
        return minted - burned;
    }

    /// @notice The honest-sequencer expectation of the zone's processed deposit-queue hash,
    ///         advanced only over contiguous prefixes (no skips/duplicates). Mirrors what
    ///         `ZoneInbox.processedDepositQueueHash()` must equal under an honest sequencer.
    function processedHashMirror() public view returns (bytes32) {
        return mirrorProcessedHash;
    }

    /// @notice Number of mirrored deposits the honest sequencer has processed on the zone.
    function processedDepositCount() public view returns (uint256) {
        return depositHead;
    }

    function _actor(uint256 seed) internal view returns (address) {
        return actors[seed % 3];
    }

    /// @notice Assert the protocol's lifecycle counters never decrease (TEMPO-ZONE-WITHDRAWAL-BATCH-INDEX, TEMPO-ZONE-DEPOSIT-NUMBER-MONOTONIC).
    /// @dev Called at the end of every mutating action; a decrement reverts and (under
    ///      fail_on_revert) breaks the run.
    function _recordMonotonicCounters() internal {
        uint64 dc = portal.depositCount();
        require(
            dc >= prevDepositCount, "TEMPO-ZONE-DEPOSIT-NUMBER-MONOTONIC: depositCount decreased"
        );
        prevDepositCount = dc;

        uint64 nwi = outbox.nextWithdrawalIndex();
        require(
            nwi >= prevNextWithdrawalIndex,
            "TEMPO-ZONE-DEPOSIT-NUMBER-MONOTONIC: nextWithdrawalIndex decreased"
        );
        prevNextWithdrawalIndex = nwi;

        uint64 pbi = portal.withdrawalBatchIndex();
        require(
            pbi >= prevPortalBatchIndex,
            "TEMPO-ZONE-WITHDRAWAL-BATCH-INDEX: portal withdrawalBatchIndex decreased"
        );
        prevPortalBatchIndex = pbi;

        uint64 obi = outbox.lastBatch().withdrawalBatchIndex;
        require(
            obi >= prevOutboxBatchIndex,
            "TEMPO-ZONE-WITHDRAWAL-BATCH-INDEX: outbox withdrawalBatchIndex decreased"
        );
        prevOutboxBatchIndex = obi;
    }

    /// @notice A user deposits on L1; escrow grows and the deposit is mirrored for later minting.
    function deposit(uint256 actorSeed, uint256 amountSeed) external {
        address user = _actor(actorSeed);
        uint256 bal = token.balanceOf(user);
        if (bal == 0) return;
        uint128 amount = uint128(bound(amountSeed, 1, bal));

        vm.startPrank(user);
        token.approve(address(portal), amount);
        bytes32 newHash = portal.deposit(address(token), user, amount, bytes32(0), user);
        vm.stopPrank();

        uint128 net = amount - portal.calculateDepositFee(); // fee is zero in this suite
        Deposit memory d = Deposit({
            token: address(token),
            sender: user,
            to: user,
            amount: net,
            tempoRefundRecipient: user,
            memo: bytes32(0)
        });
        mirrorDepositHash = keccak256(abi.encode(DepositType.Regular, d, mirrorDepositHash));
        require(newHash == mirrorDepositHash, "deposit hash mismatch");
        require(portal.currentDepositQueueHash() == mirrorDepositHash, "portal hash mismatch");

        depositMirror.push(d);
        escrowIn += net;
        numDeposits++;
        _recordMonotonicCounters();
    }

    /// @notice The sequencer processes a contiguous prefix of mirrored deposits on the zone,
    ///         minting the corresponding zone-token supply.
    function advanceDeposits(uint256 countSeed) external {
        uint256 pending = depositMirror.length - depositHead;
        if (pending == 0) return;
        uint256 count = bound(countSeed, 1, pending);

        QueuedDeposit[] memory queued = new QueuedDeposit[](count);
        bytes32 expectedHash = mirrorProcessedHash;
        for (uint256 i = 0; i < count; i++) {
            Deposit memory d = depositMirror[depositHead + i];
            queued[i] = QueuedDeposit({
                depositType: DepositType.Regular, depositData: abi.encode(d), rejected: false
            });
            expectedHash = keccak256(abi.encode(DepositType.Regular, d, expectedHash));
        }

        // The honest sequencer mirrors the L1 deposit hash into the zone's Tempo view.
        tempoState.setMockStorageValue(
            address(portal),
            PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT,
            portal.currentDepositQueueHash()
        );

        vm.prank(sequencer);
        inbox.advanceTempo("", queued, new DecryptionData[](0), new EnabledToken[](0));
        require(inbox.processedDepositQueueHash() == expectedHash, "processed hash mismatch");

        for (uint256 i = 0; i < count; i++) {
            Deposit memory d = depositMirror[depositHead + i];
            minted += d.amount;
            zoneCredit[d.to] += d.amount;
        }
        mirrorProcessedHash = expectedHash;
        depositHead += count;
        numAdvances++;
        numDepositsProcessed += count;
        _recordMonotonicCounters();
    }

    /// @notice A holder burns minted zone tokens to request a withdrawal back to L1.
    function requestWithdrawal(uint256 actorSeed, uint256 amountSeed) external {
        address holder = _actor(actorSeed);
        uint256 credit = zoneCredit[holder];
        uint256 bal = token.balanceOf(holder);
        uint256 maxAmount = credit < bal ? credit : bal;
        if (maxAmount == 0) return;
        uint128 amount = uint128(bound(amountSeed, 1, maxAmount)); // fee is zero, so totalBurn == amount

        uint256 seqBefore = txCtx.sequence();
        vm.startPrank(holder);
        token.approve(address(outbox), amount);
        outbox.requestWithdrawal(address(token), holder, amount, bytes32(0), 0, holder, "");
        vm.stopPrank();

        bytes32 txHash = txCtx.txHashFor(seqBefore + 1);
        Withdrawal memory w = Withdrawal({
            token: address(token),
            senderTag: keccak256(abi.encodePacked(holder, txHash)),
            to: holder,
            amount: amount,
            memo: bytes32(0),
            gasLimit: 0,
            fallbackNonce: uint64(seqBefore + 1),
            callbackData: "",
            encryptedSender: ""
        });
        pendingWithdrawals.push(w);
        burned += amount;
        zoneCredit[holder] -= amount;
        numWithdrawalRequests++;
        _recordMonotonicCounters();
    }

    /// @notice The sequencer finalizes a withdrawal batch on the zone and submits it to L1.
    function finalizeAndSubmit(uint256 countSeed) external {
        uint256 pending = pendingWithdrawals.length - withdrawalFinalizeHead;
        if (pending == 0) return;
        // Leave headroom in the L1 ring buffer (one slot per batch).
        if (
            portal.withdrawalQueueTail() - portal.withdrawalQueueHead()
                >= WITHDRAWAL_QUEUE_CAPACITY - 1
        ) {
            return;
        }
        uint256 count = bound(countSeed, 1, pending);

        Withdrawal[] memory ws = new Withdrawal[](count);
        for (uint256 i = 0; i < count; i++) {
            ws[i] = pendingWithdrawals[withdrawalFinalizeHead + i];
        }
        // Withdrawal queue hash: oldest outermost, EMPTY_SENTINEL innermost.
        bytes32 wHash = EMPTY_SENTINEL;
        for (uint256 i = count; i > 0; i--) {
            wHash = keccak256(abi.encode(ws[i - 1], wHash));
        }

        bytes[] memory encryptedSenders = new bytes[](count);
        vm.prank(sequencer);
        bytes32 got = outbox.finalizeWithdrawalBatch(count, uint64(block.number), encryptedSenders);
        require(got == wHash, "finalize hash mismatch");

        // Precompute all args (incl. external view calls) before pranking: argument
        // evaluation happens first and would otherwise consume the prank.
        BlockTransition memory bt = BlockTransition({
            prevBlockHash: portal.blockHash(),
            nextBlockHash: keccak256(abi.encode("nextBlock", blockNonce++))
        });
        DepositQueueTransition memory dt = DepositQueueTransition({
            prevProcessedHash: bytes32(0),
            nextProcessedHash: inbox.processedDepositQueueHash(),
            prevDepositNumber: portal.lastProcessedDepositNumber(),
            nextDepositNumber: inbox.processedDepositNumber()
        });
        vm.roll(block.number + 1);
        uint64 anchor = uint64(block.number - 1);
        vm.prank(sequencer);
        bytes[] memory signatures = new bytes[](1);
        signatures[0] = hex"01";
        portal.submitBatch(anchor, 0, bt, dt, wHash, "", "", numFinalizes + 1, signatures);

        batches.push();
        Batch storage b = batches[batches.length - 1];
        for (uint256 i = 0; i < count; i++) {
            b.ws.push(ws[i]);
        }
        withdrawalFinalizeHead += count;
        numFinalizes++;
        _recordMonotonicCounters();
    }

    /// @notice The sequencer processes the next queued withdrawal on L1, releasing escrow.
    function processWithdrawal() external {
        while (
            batchHead < batches.length && batches[batchHead].head >= batches[batchHead].ws.length
        ) {
            batchHead++;
        }
        if (batchHead >= batches.length) return;

        Batch storage b = batches[batchHead];
        uint256 idx = b.head;
        uint256 n = b.ws.length;

        bytes32 remaining;
        if (idx + 1 == n) {
            remaining = bytes32(0); // last item in slot; portal maps 0 -> EMPTY_SENTINEL
        } else {
            remaining = EMPTY_SENTINEL;
            for (uint256 i = n; i > idx + 1; i--) {
                remaining = keccak256(abi.encode(b.ws[i - 1], remaining));
            }
        }

        Withdrawal memory w = b.ws[idx];
        vm.prank(sequencer);
        Withdrawal[] memory withdrawals = new Withdrawal[](1);
        withdrawals[0] = w;
        portal.processWithdrawals(withdrawals, remaining);

        b.head = idx + 1;
        escrowOut += w.amount;
        numWithdrawalsProcessed++;
        _recordMonotonicCounters();
    }

}

/// @title ZoneLifecycleInvariantTest
/// @notice Stateful invariants over the honest-sequencer deposit/withdrawal lifecycle:
///         bridge solvency (escrow covers outstanding supply, TEMPO-ZONE-PORTAL-SOLVENCY), withdrawal queue
///         bounds (TEMPO-ZONE-WITHDRAWAL-QUEUE-RING), L1/zone batch-index lockstep (TEMPO-ZONE-WITHDRAWAL-BATCH-INDEX) and counter monotonicity
///         (TEMPO-ZONE-WITHDRAWAL-BATCH-INDEX/TEMPO-ZONE-DEPOSIT-NUMBER-MONOTONIC). These properties have no on-chain enforcement (they are gated by the
///         stub verifier) and were previously untested by any stateful fuzzing.
/// @dev Raise the per-run call depth above the default: the `afterInvariant` guard requires the
///      fuzzer to organically complete the full deposit -> advance -> request -> finalize ->
///      process chain, and the deep withdrawal legs are not reliably reached within 50 calls.
/// forge-config: default.invariant.depth = 200
contract ZoneLifecycleInvariantTest is BaseTest {

    ZonePortal internal portal;
    MockZoneToken internal token;
    MockTempoState internal tempoState;
    ZoneConfig internal config;
    ZoneInbox internal inbox;
    ZoneOutbox internal outbox;
    ZoneLifecycleHandler internal handler;

    bytes32 constant GENESIS_BLOCK_HASH = keccak256("genesis");
    bytes32 constant GENESIS_TEMPO_BLOCK_HASH = keccak256("tempoGenesis");

    function setUp() public override {
        super.setUp();
        vm.fee(0); // zero basefee => zero bounceback fee, so deposits never hit DepositTooSmall

        token = new MockZoneToken("Zone USD", "zUSD");

        // Pre-fund actors with L1 balance to deposit.
        token.setMinter(address(this), true);
        token.mint(alice, 1_000_000e6);
        token.mint(bob, 1_000_000e6);
        token.mint(charlie, 1_000_000e6);
        token.setMinter(address(this), false);

        uint64 genesisTempoBlockNumber = uint64(block.number);

        address[] memory sequencers = new address[](1);
        sequencers[0] = address(this);
        portal = _createZonePortal(1, address(token), address(this), sequencers, 1, "");

        // Zone side.
        tempoState =
            new MockTempoState(address(this), GENESIS_TEMPO_BLOCK_HASH, genesisTempoBlockNumber);
        config = new ZoneConfig(address(portal), address(tempoState));
        tempoState.setMockStorageValue(
            address(portal),
            keccak256(abi.encode(address(this), PORTAL_IS_SEQUENCER_SLOT)),
            bytes32(uint256(1))
        );
        tempoState.setMockTokenEnabled(address(portal), address(token), true);
        inbox = new ZoneInbox(address(config), address(portal), address(tempoState));
        outbox = new ZoneOutbox(address(config));

        token.setMinter(address(inbox), true);
        token.setBurner(address(outbox), true);

        handler = new ZoneLifecycleHandler(
            portal, inbox, outbox, token, tempoState, address(this), alice, bob, charlie
        );

        targetContract(address(handler));
    }

    /// @notice Bridge solvency under an honest sequencer.
    /// @dev Two checks:
    ///      1. TEMPO-ZONE-PORTAL-SOLVENCY core property: portal escrow >= outstanding zone-token supply, compared
    ///         against live on-chain quantities (totalSupply - initialSupply), so an over-mint or
    ///         under-collateralized release is caught regardless of the ghost ledger.
    ///      2. Live state exactly matches the honest-sequencer ledger, guarding against a vacuous
    ///         pass (e.g. deposits that never mint).
    function invariant_zoneSolvency() public view {
        uint256 escrow = token.balanceOf(address(portal));

        assertGe(
            escrow,
            token.totalSupply() - handler.initialSupply(),
            "TEMPO-ZONE-PORTAL-SOLVENCY: escrow does not cover outstanding zone supply"
        );

        assertEq(
            token.totalSupply(),
            handler.initialSupply() + handler.outstandingSupply(),
            "supply drifted from honest-sequencer ledger (mint/burn mismatch)"
        );
        assertEq(
            escrow,
            handler.escrowIn() - handler.escrowOut(),
            "escrow drifted from honest-sequencer ledger (deposit/release mismatch)"
        );
    }

    /// @notice Withdrawal ring buffer stays well-formed (TEMPO-ZONE-WITHDRAWAL-QUEUE-RING) and the L1/zone withdrawal
    ///         batch indices advance in lockstep (TEMPO-ZONE-WITHDRAWAL-BATCH-INDEX).
    function invariant_zoneQueueAndBatchIndices() public view {
        uint256 head = portal.withdrawalQueueHead();
        uint256 tail = portal.withdrawalQueueTail();
        assertGe(tail, head, "TEMPO-ZONE-WITHDRAWAL-QUEUE-RING: withdrawal queue tail < head");
        assertLe(
            tail - head,
            WITHDRAWAL_QUEUE_CAPACITY,
            "TEMPO-ZONE-WITHDRAWAL-QUEUE-RING: withdrawal queue exceeds capacity"
        );
        assertEq(
            portal.withdrawalBatchIndex(),
            outbox.lastBatch().withdrawalBatchIndex,
            "TEMPO-ZONE-WITHDRAWAL-BATCH-INDEX: L1 and zone withdrawal batch indices out of lockstep"
        );
    }

    /// @notice Deposit processing stays contiguous under an honest sequencer (TEMPO-ZONE-DEPOSIT-NUMBER-MONOTONIC / TEMPO-ZONE-DEPOSIT-PROCESSED-PREFIX).
    /// @dev Neither property is enforced on-chain (both gated by the stub verifier); this
    ///      pins the reference honest-sequencer behaviour:
    ///      1. TEMPO-ZONE-DEPOSIT-NUMBER-MONOTONIC: the zone never marks more deposits processed than were enqueued on L1.
    ///      2. TEMPO-ZONE-DEPOSIT-PROCESSED-PREFIX: the zone's processed deposit-queue hash equals the contiguous-prefix hash
    ///         chain (no skipped or duplicated deposits).
    function invariant_zoneDepositContiguity() public view {
        assertLe(
            inbox.processedDepositNumber(),
            portal.depositCount(),
            "TEMPO-ZONE-DEPOSIT-NUMBER-MONOTONIC: processed deposit number exceeds enqueued count"
        );
        assertEq(
            inbox.processedDepositQueueHash(),
            handler.processedHashMirror(),
            "TEMPO-ZONE-DEPOSIT-PROCESSED-PREFIX: processed deposit hash diverged from contiguous chain"
        );
    }

    /// @notice Guard against a vacuous pass: every leg of the deposit/withdrawal lifecycle must
    ///         have executed real work. In particular `numAdvances > 0` ensures the zone actually
    ///         processed deposits, so `invariant_zoneDepositContiguity` compares a non-empty
    ///         processed hash chain rather than passing trivially at `0 == 0`.
    function afterInvariant() public view {
        assertGt(handler.numDeposits(), 0, "lifecycle: no L1 deposits were made");
        assertGt(handler.numAdvances(), 0, "lifecycle: no deposits were processed on the zone");
        assertGt(handler.numDepositsProcessed(), 0, "lifecycle: zero deposits minted");
        assertGt(handler.numWithdrawalRequests(), 0, "lifecycle: no withdrawals were requested");
        assertGt(handler.numFinalizes(), 0, "lifecycle: no withdrawal batch was submitted");
        assertGt(
            handler.numWithdrawalsProcessed(), 0, "lifecycle: no withdrawal was processed on L1"
        );
        // The contiguity invariant must have compared a real (non-empty) processed hash chain.
        assertTrue(
            inbox.processedDepositQueueHash() != bytes32(0),
            "lifecycle: processed deposit hash never advanced (contiguity would pass vacuously)"
        );
    }

}
