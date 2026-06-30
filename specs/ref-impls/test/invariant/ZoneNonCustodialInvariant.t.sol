// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import {
    DecryptionData,
    Deposit,
    DepositType,
    EnabledToken,
    PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT,
    PORTAL_SEQUENCER_SLOT,
    QueuedDeposit,
    ZONE_TX_CONTEXT
} from "../../src/interfaces/IZone.sol";
import { ZoneFactory } from "../../src/l1/ZoneFactory.sol";
import { ZoneMessenger } from "../../src/l1/ZoneMessenger.sol";
import { ZonePortal } from "../../src/l1/ZonePortal.sol";
import { ZoneConfig } from "../../src/predeploys/ZoneConfig.sol";
import { ZoneInbox } from "../../src/predeploys/ZoneInbox.sol";
import { ZoneOutbox } from "../../src/predeploys/ZoneOutbox.sol";
import { BaseTest } from "../BaseTest.t.sol";
import { MockTempoState } from "../mocks/MockTempoState.sol";
import { MockZoneToken } from "../mocks/MockZoneToken.sol";
import { MockZoneTxContext } from "../mocks/MockZoneTxContext.sol";
import { Test } from "forge-std/Test.sol";

/// @title ZoneNonCustodialHandler
/// @notice Drives token pause/resume against deposits and withdrawals to exercise two
///         guarantees: the token-enabled latch (I-7) and the non-custodial property that
///         withdrawals are never blocked for an enabled token even while deposits are paused
///         (I-8). Withdrawals fire regardless of pause state; under fail_on_revert any
///         withdrawal that a (buggy) pause would block breaks the run.
contract ZoneNonCustodialHandler is Test {

    ZonePortal internal immutable portal;
    ZoneInbox internal immutable inbox;
    ZoneOutbox internal immutable outbox;
    MockZoneToken internal immutable token;
    MockTempoState internal immutable tempoState;
    MockZoneTxContext internal constant txCtx = MockZoneTxContext(ZONE_TX_CONTEXT);

    address internal immutable admin; // admin == sequencer == the test contract
    address[3] internal actors;

    Deposit[] internal depositMirror;
    uint256 internal depositHead;
    bytes32 internal mirrorDepositHash;
    bytes32 internal mirrorProcessedHash;

    uint256 public withdrawalsWhilePaused; // coverage: proves I-8 is actually exercised

    constructor(
        ZonePortal _portal,
        ZoneInbox _inbox,
        ZoneOutbox _outbox,
        MockZoneToken _token,
        MockTempoState _tempoState,
        address _admin,
        address _alice,
        address _bob,
        address _charlie
    ) {
        portal = _portal;
        inbox = _inbox;
        outbox = _outbox;
        token = _token;
        tempoState = _tempoState;
        admin = _admin;
        actors = [_alice, _bob, _charlie];
    }

    function _actor(uint256 seed) internal view returns (address) {
        return actors[seed % 3];
    }

    function pauseDeposits() external {
        if (!portal.areDepositsActive(address(token))) return;
        vm.prank(admin);
        portal.pauseDeposits(address(token));
    }

    function resumeDeposits() external {
        if (portal.areDepositsActive(address(token))) return;
        vm.prank(admin);
        portal.resumeDeposits(address(token));
    }

    /// @notice Deposit on L1 (only valid while deposits are active) and mirror it for minting.
    function deposit(uint256 actorSeed, uint256 amountSeed) external {
        if (!portal.areDepositsActive(address(token))) return;
        address user = _actor(actorSeed);
        uint256 bal = token.balanceOf(user);
        if (bal == 0) return;
        uint128 amount = uint128(bound(amountSeed, 1, bal));

        vm.startPrank(user);
        token.approve(address(portal), amount);
        portal.deposit(address(token), user, amount, bytes32(0), user);
        vm.stopPrank();

        Deposit memory d = Deposit({
            token: address(token),
            sender: user,
            to: user,
            amount: amount, // fee is zero in this suite
            bouncebackRecipient: user,
            memo: bytes32(0)
        });
        mirrorDepositHash = keccak256(abi.encode(DepositType.Regular, d, mirrorDepositHash));
        depositMirror.push(d);
    }

    /// @notice Mint a contiguous prefix of mirrored deposits on the zone (credit to withdraw).
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

        tempoState.setMockStorageValue(
            address(portal),
            PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT,
            portal.currentDepositQueueHash()
        );
        vm.prank(admin);
        inbox.advanceTempo("", queued, new DecryptionData[](0), new EnabledToken[](0));

        mirrorProcessedHash = expectedHash;
        depositHead += count;
    }

    /// @notice Request a withdrawal regardless of the deposit pause state. The non-custodial
    ///         guarantee (I-8) requires this to succeed for an enabled token even when paused.
    ///         Holders carry zone-token balance from genesis, so withdrawals never depend on
    ///         prior deposits being processed.
    function requestWithdrawal(uint256 actorSeed, uint256 amountSeed) external {
        address holder = _actor(actorSeed);
        uint256 bal = token.balanceOf(holder);
        if (bal == 0) return;
        uint128 amount = uint128(bound(amountSeed, 1, bal));

        bool paused = !portal.areDepositsActive(address(token));
        vm.startPrank(holder);
        token.approve(address(outbox), amount);
        outbox.requestWithdrawal(address(token), holder, amount, bytes32(0), 0, holder, "");
        vm.stopPrank();
        if (paused) withdrawalsWhilePaused++;
    }

    /// @notice Force the deposits-paused state and then withdraw, so I-8 is exercised
    ///         deterministically rather than only by chance interleaving.
    function pauseThenWithdraw(uint256 actorSeed, uint256 amountSeed) external {
        address holder = _actor(actorSeed);
        uint256 bal = token.balanceOf(holder);
        if (bal == 0) return;
        uint128 amount = uint128(bound(amountSeed, 1, bal));

        if (portal.areDepositsActive(address(token))) {
            vm.prank(admin);
            portal.pauseDeposits(address(token));
        }
        require(!portal.areDepositsActive(address(token)), "deposits should be paused");

        vm.startPrank(holder);
        token.approve(address(outbox), amount);
        // Must not revert: withdrawals are non-custodial for an enabled token (I-8).
        outbox.requestWithdrawal(address(token), holder, amount, bytes32(0), 0, holder, "");
        vm.stopPrank();
        withdrawalsWhilePaused++;
    }

}

/// @title ZoneNonCustodialInvariantTest
/// @notice Stateful invariants for the token-enabled latch (I-7) and the non-custodial
///         withdrawal guarantee (I-8) under random pause/resume interleavings.
contract ZoneNonCustodialInvariantTest is BaseTest {

    ZoneFactory internal zoneFactory;
    ZonePortal internal portal;
    ZoneMessenger internal messenger;
    MockZoneToken internal token;
    MockTempoState internal tempoState;
    ZoneConfig internal config;
    ZoneInbox internal inbox;
    ZoneOutbox internal outbox;
    ZoneNonCustodialHandler internal handler;

    bytes32 constant GENESIS_BLOCK_HASH = keccak256("genesis");
    bytes32 constant GENESIS_TEMPO_BLOCK_HASH = keccak256("tempoGenesis");

    function setUp() public override {
        super.setUp();
        vm.fee(0);

        zoneFactory = new ZoneFactory();
        token = new MockZoneToken("Zone USD", "zUSD");

        token.setMinter(address(this), true);
        token.mint(alice, 1_000_000e6);
        token.mint(bob, 1_000_000e6);
        token.mint(charlie, 1_000_000e6);
        token.setMinter(address(this), false);

        uint64 genesisTempoBlockNumber = uint64(block.number);

        uint256 nonce = vm.getNonce(address(this));
        address predictedPortal = vm.computeCreateAddress(address(this), nonce + 1);
        messenger = new ZoneMessenger(predictedPortal);
        portal = new ZonePortal(
            1,
            address(token),
            address(messenger),
            address(this),
            address(this),
            zoneFactory.verifier(),
            GENESIS_BLOCK_HASH,
            genesisTempoBlockNumber,
            ""
        );

        tempoState =
            new MockTempoState(address(this), GENESIS_TEMPO_BLOCK_HASH, genesisTempoBlockNumber);
        config = new ZoneConfig(address(portal), address(tempoState));
        tempoState.setMockStorageValue(
            address(portal), PORTAL_SEQUENCER_SLOT, bytes32(uint256(uint160(address(this))))
        );
        tempoState.setMockTokenEnabled(address(portal), address(token), true);
        inbox = new ZoneInbox(address(config), address(portal), address(tempoState));
        outbox = new ZoneOutbox(address(config));

        token.setMinter(address(inbox), true);
        token.setBurner(address(outbox), true);

        handler = new ZoneNonCustodialHandler(
            portal, inbox, outbox, token, tempoState, address(this), alice, bob, charlie
        );

        targetContract(address(handler));
    }

    /// @notice I-7: once enabled, a token never becomes disabled (pause/resume must only
    ///         toggle depositsActive, never the enabled latch).
    function invariant_tokenEnabledLatch() public view {
        assertTrue(portal.isTokenEnabled(address(token)), "I-7: token became disabled on L1");
        assertTrue(config.isEnabledToken(address(token)), "I-7: token became disabled on zone");
    }

    /// @notice X-3: the portal keeps the messenger at max escrow allowance for an enabled
    ///         token. Enabling grants `type(uint256).max`; nothing in this suite relays
    ///         (which would spend it), so it must stay max — escrow can always be released.
    function invariant_messengerAllowanceMax() public view {
        assertEq(
            token.allowance(address(portal), address(messenger)),
            type(uint256).max,
            "X-3: messenger lost max escrow allowance"
        );
    }

    /// @notice I-8 is enforced inside the handler: every withdrawal-while-paused must not
    ///         revert under fail_on_revert. This hook guards against a vacuous pass by
    ///         requiring the paused-withdrawal path to have actually executed.
    function afterInvariant() public view {
        assertGt(
            handler.withdrawalsWhilePaused(),
            0,
            "I-8: non-custodial withdrawal-while-paused path never exercised"
        );
    }

}
