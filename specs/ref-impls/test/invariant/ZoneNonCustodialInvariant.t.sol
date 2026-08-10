// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import {
    AES_GCM_DECRYPT,
    CHAUM_PEDERSEN_VERIFY,
    ChaumPedersenProof,
    DecryptionData,
    Deposit,
    DepositPayload,
    DepositType,
    EnabledToken,
    IAesGcmDecrypt,
    IChaumPedersenVerify,
    PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT,
    PORTAL_ENCRYPTION_KEYS_SLOT,
    PORTAL_IS_SEQUENCER_SLOT,
    PORTAL_TOKEN_CONFIGS_SLOT,
    QueuedDeposit,
    ZONE_TX_CONTEXT
} from "../../src/interfaces/IZone.sol";
import { EncryptedDepositLib } from "../../src/libraries/EncryptedDeposit.sol";
import { ZonePortal } from "../../src/tempo/ZonePortal.sol";
import { ZoneInbox } from "../../src/zone/ZoneInbox.sol";
import { ZoneOutbox } from "../../src/zone/ZoneOutbox.sol";
import { BaseTest } from "../BaseTest.t.sol";
import { MockTempoState } from "../mocks/MockTempoState.sol";
import { MockZoneToken } from "../mocks/MockZoneToken.sol";
import { MockZoneTxContext } from "../mocks/MockZoneTxContext.sol";
import { Test } from "forge-std/Test.sol";

/// @title ZoneNonCustodialHandler
/// @notice Drives token pause/resume against deposits and withdrawals to exercise two
///         guarantees: the token-enabled latch (TEMPO-ZONE-TOKEN-ENABLEMENT-APPEND-ONLY) and the non-custodial property that
///         withdrawals are never blocked for an enabled token even while deposits are paused
///         (TEMPO-ZONE-TOKEN-DEPOSIT-PAUSE-ONLY). Withdrawals fire regardless of pause state; under fail_on_revert any
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

    uint256 public withdrawalsWhilePaused; // coverage: proves TEMPO-ZONE-TOKEN-DEPOSIT-PAUSE-ONLY is actually exercised

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

    function _payload() internal pure returns (DepositPayload memory) {
        return DepositPayload({
            ephemeralPubkeyX: bytes32(uint256(0x1234)),
            ephemeralPubkeyYParity: 0x02,
            ciphertext: new bytes(64),
            nonce: bytes12(0),
            tag: bytes16(0)
        });
    }

    function _decryptions(uint256 count) internal pure returns (DecryptionData[] memory decs) {
        decs = new DecryptionData[](count);
        for (uint256 i; i < count; ++i) {
            decs[i] = DecryptionData({
                sharedSecret: bytes32(uint256(0xdeadbeef)),
                sharedSecretYParity: 0x02,
                cpProof: ChaumPedersenProof({ s: bytes32(uint256(1)), c: bytes32(uint256(2)) })
            });
        }
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
        actorSeed;
        address user = actors[0];
        uint256 bal = token.balanceOf(user);
        if (bal == 0) return;
        uint128 amount = uint128(bound(amountSeed, 1, bal));

        vm.startPrank(user);
        token.approve(address(portal), amount);
        portal.deposit(address(token), amount, 0, _payload(), user);
        vm.stopPrank();

        Deposit memory d = Deposit({
            token: address(token),
            sender: user,
            amount: amount, // fee is zero in this suite
            tempoRefundRecipient: user,
            keyIndex: 0,
            encrypted: _payload()
        });
        mirrorDepositHash = keccak256(abi.encode(DepositType.Deposit, d, mirrorDepositHash));
        depositMirror.push(d);
    }

    /// @notice Mint a contiguous prefix of mirrored deposits on the zone (credit to withdraw).
    function advanceDeposits(uint256) external {
        uint256 pending = depositMirror.length - depositHead;
        if (pending == 0) return;
        uint256 count = pending;

        QueuedDeposit[] memory queued = new QueuedDeposit[](count);
        bytes32 expectedHash = mirrorProcessedHash;
        for (uint256 i = 0; i < count; i++) {
            Deposit memory d = depositMirror[depositHead + i];
            queued[i] = QueuedDeposit({
                depositType: DepositType.Deposit, depositData: abi.encode(d), rejected: false
            });
            expectedHash = keccak256(abi.encode(DepositType.Deposit, d, expectedHash));
        }

        tempoState.setMockStorageValue(
            address(portal),
            PORTAL_CURRENT_DEPOSIT_QUEUE_HASH_SLOT,
            portal.currentDepositQueueHash()
        );
        vm.prank(address(0));
        inbox.advanceTempo(new bytes[](1), queued, _decryptions(count), new EnabledToken[](0));

        mirrorProcessedHash = expectedHash;
        depositHead += count;
    }

    /// @notice Request a withdrawal regardless of the deposit pause state. The non-custodial
    ///         guarantee (TEMPO-ZONE-TOKEN-DEPOSIT-PAUSE-ONLY) requires this to succeed for an enabled token even when paused.
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

    /// @notice Force the deposits-paused state and then withdraw, so TEMPO-ZONE-TOKEN-DEPOSIT-PAUSE-ONLY is exercised
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
        // Must not revert: withdrawals are non-custodial for an enabled token (TEMPO-ZONE-TOKEN-DEPOSIT-PAUSE-ONLY).
        outbox.requestWithdrawal(address(token), holder, amount, bytes32(0), 0, holder, "");
        vm.stopPrank();
        withdrawalsWhilePaused++;
    }

}

/// @title ZoneNonCustodialInvariantTest
/// @notice Stateful invariants for the token-enabled latch (TEMPO-ZONE-TOKEN-ENABLEMENT-APPEND-ONLY) and the non-custodial
///         withdrawal guarantee (TEMPO-ZONE-TOKEN-DEPOSIT-PAUSE-ONLY) under random pause/resume interleavings.
contract ZoneNonCustodialInvariantTest is BaseTest {

    ZonePortal internal portal;
    MockZoneToken internal token;
    MockTempoState internal tempoState;
    ZoneInbox internal inbox;
    ZoneOutbox internal outbox;
    ZoneNonCustodialHandler internal handler;

    bytes32 constant GENESIS_BLOCK_HASH = keccak256("genesis");
    bytes32 constant GENESIS_TEMPO_BLOCK_HASH = keccak256("tempoGenesis");

    function setUp() public override {
        super.setUp();
        vm.fee(0);

        token = new MockZoneToken("Zone USD", "zUSD");
        _mockTokenPolicyMigration(address(token), true);

        token.setMinter(address(this), true);
        token.mint(alice, 1_000_000e6);
        token.mint(bob, 1_000_000e6);
        token.mint(charlie, 1_000_000e6);
        token.setMinter(address(this), false);

        uint64 genesisTempoBlockNumber = uint64(block.number);

        address[] memory sequencers = new address[](1);
        sequencers[0] = address(this);
        portal = _createZonePortal(1, address(token), address(this), sequencers, 1, "");

        bytes32 entriesBase = keccak256(abi.encode(uint256(PORTAL_ENCRYPTION_KEYS_SLOT)));
        bytes32 keyX = bytes32(uint256(0x1234));
        bytes32 keyMeta = bytes32((uint256(uint64(block.number)) << 8) | uint256(0x02));
        vm.store(address(portal), PORTAL_ENCRYPTION_KEYS_SLOT, bytes32(uint256(1)));
        vm.store(address(portal), entriesBase, keyX);
        vm.store(address(portal), bytes32(uint256(entriesBase) + 1), keyMeta);

        tempoState =
            new MockTempoState(address(this), GENESIS_TEMPO_BLOCK_HASH, genesisTempoBlockNumber);
        tempoState.setMockStorageValue(
            address(portal),
            keccak256(abi.encode(address(this), PORTAL_IS_SEQUENCER_SLOT)),
            bytes32(uint256(1))
        );
        tempoState.setMockTokenEnabled(address(portal), address(token), true);
        tempoState.setMockStorageValue(
            address(portal), PORTAL_ENCRYPTION_KEYS_SLOT, bytes32(uint256(1))
        );
        tempoState.setMockStorageValue(address(portal), entriesBase, keyX);
        tempoState.setMockStorageValue(address(portal), bytes32(uint256(entriesBase) + 1), keyMeta);
        inbox = new ZoneInbox(address(portal), address(tempoState));
        outbox = new ZoneOutbox(address(portal), address(tempoState));

        token.setMinter(address(inbox), true);
        token.setBurner(address(outbox), true);
        vm.etch(CHAUM_PEDERSEN_VERIFY, hex"00");
        vm.etch(AES_GCM_DECRYPT, hex"00");
        vm.mockCall(
            CHAUM_PEDERSEN_VERIFY,
            abi.encodeWithSelector(IChaumPedersenVerify.verifyProof.selector),
            abi.encode(true)
        );
        vm.mockCall(
            AES_GCM_DECRYPT,
            abi.encodeWithSelector(IAesGcmDecrypt.decrypt.selector),
            abi.encode(EncryptedDepositLib.encodePlaintext(alice, bytes32(0)), true)
        );

        handler = new ZoneNonCustodialHandler(
            portal, inbox, outbox, token, tempoState, address(this), alice, bob, charlie
        );

        targetContract(address(handler));
    }

    /// @notice TEMPO-ZONE-TOKEN-ENABLEMENT-APPEND-ONLY: once enabled, a token never becomes disabled (pause/resume must only
    ///         toggle depositsActive, never the enabled latch).
    function invariant_tokenEnabledLatch() public view {
        assertTrue(
            portal.isTokenEnabled(address(token)),
            "TEMPO-ZONE-TOKEN-ENABLEMENT-APPEND-ONLY: token became disabled on L1"
        );
        assertTrue(
            uint256(
                    tempoState.readTempoStorageSlot(
                        address(portal),
                        keccak256(abi.encode(address(token), PORTAL_TOKEN_CONFIGS_SLOT))
                    )
                ) & 0xff == 1,
            "TEMPO-ZONE-TOKEN-ENABLEMENT-APPEND-ONLY: token became disabled on zone"
        );
    }

    /// @notice TEMPO-ZONE-TOKEN-DEPOSIT-PAUSE-ONLY is enforced inside the handler: every withdrawal-while-paused must not
    ///         revert under fail_on_revert. This hook guards against a vacuous pass by
    ///         requiring the paused-withdrawal path to have actually executed.
    function afterInvariant() public view {
        assertGt(
            handler.withdrawalsWhilePaused(),
            0,
            "TEMPO-ZONE-TOKEN-DEPOSIT-PAUSE-ONLY: non-custodial withdrawal-while-paused path never exercised"
        );
    }

}
