// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import { ENCRYPTION_KEY_GRACE_PERIOD } from "../../src/interfaces/IZone.sol";
import { ZoneMessenger } from "../../src/tempo/ZoneMessenger.sol";
import { ZonePortal } from "../../src/tempo/ZonePortal.sol";
import { BaseTest } from "../BaseTest.t.sol";
import { MockZoneToken } from "../mocks/MockZoneToken.sol";
import { Test } from "forge-std/Test.sol";
import { Vm } from "forge-std/Vm.sol";

/// @title ZoneEncryptionKeyHandler
/// @notice Registers real secp256k1 encryption keys (with valid proof-of-possession) and rolls
///         the block number forward, mirroring each key's activation block. Drives the temporal
///         key-rotation behaviour (TEMPO-ZONE-ENCRYPTION-KEY-GRACE) so the invariant test can compare on-chain validity
///         against the reference grace-period rule.
contract ZoneEncryptionKeyHandler is Test {

    ZonePortal internal immutable portal;
    address internal immutable sequencer;

    uint64[] public activationBlocks; // ghost: activation block of each registered key, in order

    constructor(ZonePortal _portal, address _sequencer) {
        portal = _portal;
        sequencer = _sequencer;
    }

    function keyCount() external view returns (uint256) {
        return activationBlocks.length;
    }

    function activationAt(uint256 i) external view returns (uint64) {
        return activationBlocks[i];
    }

    /// @notice Register a fresh key with a valid proof-of-possession at the current block.
    function registerKey(uint256 walletSeed) external {
        uint256 pk = bound(walletSeed, 1, type(uint128).max);
        Vm.Wallet memory w = vm.createWallet(pk);
        bytes32 x = bytes32(w.publicKeyX);
        uint8 yParity = w.publicKeyY % 2 == 0 ? 0x02 : 0x03;

        bytes32 message = keccak256(abi.encode(address(portal), x, yParity));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(w.privateKey, message);

        vm.prank(sequencer);
        portal.setSequencerEncryptionKey(x, yParity, v, r, s);
        activationBlocks.push(uint64(block.number));
    }

    /// @notice Advance the chain, sometimes far enough to push old keys past their grace window.
    function advanceBlocks(uint256 amountSeed) external {
        uint256 delta = bound(amountSeed, 1, 100_000);
        vm.roll(block.number + delta);
    }

}

/// @title ZoneEncryptionKeyInvariantTest
/// @notice Stateful invariant for encryption-key rotation (TEMPO-ZONE-ENCRYPTION-KEY-GRACE): the latest key never expires,
///         and an older key is valid for new deposits exactly while
///         `block.number < nextKey.activationBlock + ENCRYPTION_KEY_GRACE_PERIOD`.
contract ZoneEncryptionKeyInvariantTest is BaseTest {

    ZonePortal internal portal;
    MockZoneToken internal token;
    ZoneEncryptionKeyHandler internal handler;

    address internal constant SEQ = address(0x5e9);
    bytes32 constant GENESIS_BLOCK_HASH = keccak256("genesis");

    function setUp() public override {
        super.setUp();
        token = new MockZoneToken("Zone USD", "zUSD");
        _mockTokenPolicyMigration(address(token), true);

        address[] memory sequencers = new address[](1);
        sequencers[0] = SEQ;
        portal = _createZonePortal(1, address(token), address(this), sequencers, 1, "");

        handler = new ZoneEncryptionKeyHandler(portal, SEQ);
        targetContract(address(handler));
    }

    /// @notice On-chain key validity matches the reference grace-period rule for every key.
    function invariant_encryptionKeyGrace() public view {
        uint256 count = portal.encryptionKeyCount();
        assertEq(count, handler.keyCount(), "key count diverged from ghost ledger");
        if (count == 0) return;

        // Latest key never expires.
        (bool latestValid, uint64 latestExpiry) = portal.isEncryptionKeyValid(count - 1);
        assertTrue(latestValid, "TEMPO-ZONE-ENCRYPTION-KEY-GRACE: latest key reported invalid");
        assertEq(
            latestExpiry,
            0,
            "TEMPO-ZONE-ENCRYPTION-KEY-GRACE: latest key reported a non-zero expiry"
        );

        // Each older key is valid iff still inside its successor's grace window.
        for (uint256 i = 0; i + 1 < count; i++) {
            uint64 expectedExpiry = handler.activationAt(i + 1) + ENCRYPTION_KEY_GRACE_PERIOD;
            bool expectedValid = block.number < expectedExpiry;
            (bool valid, uint64 expiry) = portal.isEncryptionKeyValid(i);
            assertEq(
                expiry,
                expectedExpiry,
                "TEMPO-ZONE-ENCRYPTION-KEY-GRACE: old-key expiry diverged from grace rule"
            );
            assertEq(
                valid,
                expectedValid,
                "TEMPO-ZONE-ENCRYPTION-KEY-GRACE: old-key validity diverged from grace rule"
            );
        }

        // Out-of-range index is always invalid.
        (bool oobValid,) = portal.isEncryptionKeyValid(count);
        assertFalse(
            oobValid, "TEMPO-ZONE-ENCRYPTION-KEY-GRACE: out-of-range key index reported valid"
        );
    }

    /// @notice Guard against a vacuous pass: the old-key branch needs at least two keys.
    function afterInvariant() public view {
        assertGe(
            handler.keyCount(),
            2,
            "TEMPO-ZONE-ENCRYPTION-KEY-GRACE: rotation path (>=2 keys) never exercised"
        );
    }

}
