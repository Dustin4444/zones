// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import { IZoneFactory, ZoneInfo, ZoneParams } from "../../src/zone/IZone.sol";
import { ZoneFactory } from "../../src/zone/ZoneFactory.sol";
import { ZoneMessenger } from "../../src/zone/ZoneMessenger.sol";
import { ZonePortal } from "../../src/zone/ZonePortal.sol";
import { BaseTest } from "../BaseTest.t.sol";
import { Vm } from "forge-std/Vm.sol";
import { ITIP20 } from "tempo-std/interfaces/ITIP20.sol";

/// @title ZoneFactoryTest
/// @notice Comprehensive tests for ZoneFactory validation and zone creation
contract ZoneFactoryTest is BaseTest {

    ZoneFactory public zoneFactory;

    bytes32 constant GENESIS_BLOCK_HASH = keccak256("genesis");
    bytes32 constant GENESIS_TEMPO_BLOCK_HASH = keccak256("tempoGenesis");
    string constant ZONE_RPC_URL = "https://rpc.zone.example:8545/path";

    function setUp() public override {
        super.setUp();
        zoneFactory = new ZoneFactory();
    }

    /*//////////////////////////////////////////////////////////////
                          VALID CREATION TESTS
    //////////////////////////////////////////////////////////////*/

    function test_createZone_success() public {
        IZoneFactory.CreateZoneParams memory params = IZoneFactory.CreateZoneParams({
            initialToken: address(pathUSD),
            sequencer: admin,
            verifier: zoneFactory.verifier(),
            zoneParams: ZoneParams({
                genesisBlockHash: GENESIS_BLOCK_HASH,
                genesisTempoBlockHash: GENESIS_TEMPO_BLOCK_HASH,
                genesisTempoBlockNumber: uint64(block.number)
            }),
            zoneRpcUrl: ZONE_RPC_URL
        });

        (uint32 zoneId, address portal) = zoneFactory.createZone(params);

        assertEq(zoneId, 1);
        assertTrue(portal != address(0));
        assertEq(zoneFactory.zoneCount(), 1);
        assertTrue(zoneFactory.isZonePortal(portal));

        ZoneInfo memory info = zoneFactory.zones(zoneId);
        assertEq(info.zoneId, 1);
        assertEq(info.portal, portal);
        assertTrue(info.messenger != address(0));
        assertEq(info.initialToken, address(pathUSD));
        assertEq(info.sequencer, admin);
        assertEq(info.verifier, zoneFactory.verifier());
        assertEq(info.genesisBlockHash, GENESIS_BLOCK_HASH);
        assertEq(info.genesisTempoBlockHash, GENESIS_TEMPO_BLOCK_HASH);
        assertEq(info.zoneRpcUrl, ZONE_RPC_URL);
        assertEq(ZonePortal(portal).zoneRpcUrl(), ZONE_RPC_URL);
    }

    function test_createZone_deploysMessenger() public {
        IZoneFactory.CreateZoneParams memory params = IZoneFactory.CreateZoneParams({
            initialToken: address(pathUSD),
            sequencer: admin,
            verifier: zoneFactory.verifier(),
            zoneParams: ZoneParams({
                genesisBlockHash: GENESIS_BLOCK_HASH,
                genesisTempoBlockHash: GENESIS_TEMPO_BLOCK_HASH,
                genesisTempoBlockNumber: uint64(block.number)
            }),
            zoneRpcUrl: ZONE_RPC_URL
        });

        (uint32 zoneId, address portal) = zoneFactory.createZone(params);

        ZoneInfo memory info = zoneFactory.zones(zoneId);
        address messengerAddr = info.messenger;

        // Verify messenger is deployed and configured correctly
        ZoneMessenger messenger = ZoneMessenger(messengerAddr);
        assertEq(messenger.portal(), portal);

        // Verify portal references the messenger
        ZonePortal portalContract = ZonePortal(portal);
        assertEq(portalContract.messenger(), messengerAddr);
    }

    function test_createZone_multipleZones() public {
        IZoneFactory.CreateZoneParams memory params1 = IZoneFactory.CreateZoneParams({
            initialToken: address(pathUSD),
            sequencer: admin,
            verifier: zoneFactory.verifier(),
            zoneParams: ZoneParams({
                genesisBlockHash: GENESIS_BLOCK_HASH,
                genesisTempoBlockHash: GENESIS_TEMPO_BLOCK_HASH,
                genesisTempoBlockNumber: uint64(block.number)
            }),
            zoneRpcUrl: ZONE_RPC_URL
        });

        (uint32 zoneId1, address portal1) = zoneFactory.createZone(params1);

        IZoneFactory.CreateZoneParams memory params2 = IZoneFactory.CreateZoneParams({
            initialToken: address(pathUSD),
            sequencer: alice,
            verifier: zoneFactory.verifier(),
            zoneParams: ZoneParams({
                genesisBlockHash: keccak256("genesis2"),
                genesisTempoBlockHash: keccak256("tempoGenesis2"),
                genesisTempoBlockNumber: uint64(block.number)
            }),
            zoneRpcUrl: "HTTPS://rpc2.zone.example:443"
        });

        (uint32 zoneId2, address portal2) = zoneFactory.createZone(params2);

        assertEq(zoneId1, 1);
        assertEq(zoneId2, 2);
        assertTrue(portal1 != portal2);
        assertEq(zoneFactory.zoneCount(), 2);
        assertTrue(zoneFactory.isZonePortal(portal1));
        assertTrue(zoneFactory.isZonePortal(portal2));

        // Each zone should have its own messenger
        ZoneInfo memory info1 = zoneFactory.zones(zoneId1);
        ZoneInfo memory info2 = zoneFactory.zones(zoneId2);
        assertTrue(info1.messenger != info2.messenger);
    }

    function test_createZone_emitsEvent() public {
        IZoneFactory.CreateZoneParams memory params = IZoneFactory.CreateZoneParams({
            initialToken: address(pathUSD),
            sequencer: admin,
            verifier: zoneFactory.verifier(),
            zoneParams: ZoneParams({
                genesisBlockHash: GENESIS_BLOCK_HASH,
                genesisTempoBlockHash: GENESIS_TEMPO_BLOCK_HASH,
                genesisTempoBlockNumber: uint64(block.number)
            }),
            zoneRpcUrl: ZONE_RPC_URL
        });

        // Record logs and verify ZoneCreated event was emitted
        vm.recordLogs();
        (uint32 zoneId, address portal) = zoneFactory.createZone(params);

        // Verify logs contain ZoneCreated event with correct data
        Vm.Log[] memory logs = vm.getRecordedLogs();
        bool found = false;
        for (uint256 i = 0; i < logs.length; i++) {
            if (
                logs[i].topics[0]
                    == keccak256(
                        "ZoneCreated(uint32,address,address,address,address,address,bytes32,bytes32,uint64,string)"
                    )
            ) {
                found = true;
                // Verify the indexed zoneId (topic[1])
                assertEq(uint256(logs[i].topics[1]), uint256(zoneId));
                // Verify indexed portal (topic[2])
                assertEq(address(uint160(uint256(logs[i].topics[2]))), portal);
                break;
            }
        }
        assertTrue(found, "ZoneCreated event not found");

        // Verify the portal address is valid
        assertTrue(portal != address(0));
    }

    /*//////////////////////////////////////////////////////////////
                          INVALID TOKEN TESTS
    //////////////////////////////////////////////////////////////*/

    function test_createZone_revertsOnInvalidToken_zeroAddress() public {
        IZoneFactory.CreateZoneParams memory params = IZoneFactory.CreateZoneParams({
            initialToken: address(0),
            sequencer: admin,
            verifier: zoneFactory.verifier(),
            zoneParams: ZoneParams({
                genesisBlockHash: GENESIS_BLOCK_HASH,
                genesisTempoBlockHash: GENESIS_TEMPO_BLOCK_HASH,
                genesisTempoBlockNumber: uint64(block.number)
            }),
            zoneRpcUrl: ZONE_RPC_URL
        });

        vm.expectRevert(IZoneFactory.InvalidToken.selector);
        zoneFactory.createZone(params);
    }

    function test_createZone_revertsOnInvalidToken_nonTIP20() public {
        // Deploy a non-TIP20 contract (just an empty contract)
        address notTip20 = address(new NotATIP20());

        IZoneFactory.CreateZoneParams memory params = IZoneFactory.CreateZoneParams({
            initialToken: notTip20,
            sequencer: admin,
            verifier: zoneFactory.verifier(),
            zoneParams: ZoneParams({
                genesisBlockHash: GENESIS_BLOCK_HASH,
                genesisTempoBlockHash: GENESIS_TEMPO_BLOCK_HASH,
                genesisTempoBlockNumber: uint64(block.number)
            }),
            zoneRpcUrl: ZONE_RPC_URL
        });

        vm.expectRevert(IZoneFactory.InvalidToken.selector);
        zoneFactory.createZone(params);
    }

    function test_createZone_revertsOnInvalidToken_eoa() public {
        IZoneFactory.CreateZoneParams memory params = IZoneFactory.CreateZoneParams({
            initialToken: alice, // EOA, not a contract
            sequencer: admin,
            verifier: zoneFactory.verifier(),
            zoneParams: ZoneParams({
                genesisBlockHash: GENESIS_BLOCK_HASH,
                genesisTempoBlockHash: GENESIS_TEMPO_BLOCK_HASH,
                genesisTempoBlockNumber: uint64(block.number)
            }),
            zoneRpcUrl: ZONE_RPC_URL
        });

        vm.expectRevert(IZoneFactory.InvalidToken.selector);
        zoneFactory.createZone(params);
    }

    /*//////////////////////////////////////////////////////////////
                       INVALID SEQUENCER TESTS
    //////////////////////////////////////////////////////////////*/

    function test_createZone_revertsOnInvalidSequencer_zeroAddress() public {
        IZoneFactory.CreateZoneParams memory params = IZoneFactory.CreateZoneParams({
            initialToken: address(pathUSD),
            sequencer: address(0),
            verifier: zoneFactory.verifier(),
            zoneParams: ZoneParams({
                genesisBlockHash: GENESIS_BLOCK_HASH,
                genesisTempoBlockHash: GENESIS_TEMPO_BLOCK_HASH,
                genesisTempoBlockNumber: uint64(block.number)
            }),
            zoneRpcUrl: ZONE_RPC_URL
        });

        vm.expectRevert(IZoneFactory.InvalidSequencer.selector);
        zoneFactory.createZone(params);
    }

    /*//////////////////////////////////////////////////////////////
                       INVALID VERIFIER TESTS
    //////////////////////////////////////////////////////////////*/

    function test_createZone_revertsOnInvalidVerifier() public {
        IZoneFactory.CreateZoneParams memory params = IZoneFactory.CreateZoneParams({
            initialToken: address(pathUSD),
            sequencer: admin,
            verifier: address(0xdead),
            zoneParams: ZoneParams({
                genesisBlockHash: GENESIS_BLOCK_HASH,
                genesisTempoBlockHash: GENESIS_TEMPO_BLOCK_HASH,
                genesisTempoBlockNumber: uint64(block.number)
            }),
            zoneRpcUrl: ZONE_RPC_URL
        });

        vm.expectRevert(IZoneFactory.InvalidVerifier.selector);
        zoneFactory.createZone(params);
    }

    /*//////////////////////////////////////////////////////////////
                       ZONE RPC URL VALIDATION TESTS
    //////////////////////////////////////////////////////////////*/

    function test_createZone_allowsEmptyZoneRpcUrl() public {
        IZoneFactory.CreateZoneParams memory params = _validCreateZoneParams("");

        (uint32 zoneId, address portal) = zoneFactory.createZone(params);

        assertEq(zoneFactory.zones(zoneId).zoneRpcUrl, "");
        assertEq(ZonePortal(portal).zoneRpcUrl(), "");
    }

    function test_createZone_allowsHttpsSchemeOnlyValidation() public {
        IZoneFactory.CreateZoneParams memory params = _validCreateZoneParams("HTTPS:foo:bar");

        (, address portal) = zoneFactory.createZone(params);

        assertEq(ZonePortal(portal).zoneRpcUrl(), "HTTPS:foo:bar");
    }

    function test_createZone_allowsMaxLengthZoneRpcUrl() public {
        IZoneFactory.CreateZoneParams memory params = _validCreateZoneParams(_httpsUrlOfLength(256));

        (, address portal) = zoneFactory.createZone(params);

        assertEq(bytes(ZonePortal(portal).zoneRpcUrl()).length, 256);
    }

    function test_createZone_revertsOnInvalidZoneRpcUrl() public {
        string[6] memory invalidUrls = _invalidZoneRpcUrls();
        for (uint256 i = 0; i < invalidUrls.length; i++) {
            IZoneFactory.CreateZoneParams memory params = _validCreateZoneParams(invalidUrls[i]);

            vm.expectRevert(IZoneFactory.InvalidZoneRpcUrl.selector);
            zoneFactory.createZone(params);
        }
    }

    function test_createZone_revertsOnZoneRpcUrlTooLong() public {
        IZoneFactory.CreateZoneParams memory params = _validCreateZoneParams(_httpsUrlOfLength(257));

        vm.expectRevert(IZoneFactory.ZoneRpcUrlTooLong.selector);
        zoneFactory.createZone(params);
    }

    /*//////////////////////////////////////////////////////////////
                            VIEW TESTS
    //////////////////////////////////////////////////////////////*/

    function test_zoneCount_initiallyZero() public view {
        assertEq(zoneFactory.zoneCount(), 0);
    }

    function test_isZonePortal_returnsFalseForNonPortal() public view {
        assertFalse(zoneFactory.isZonePortal(address(0)));
        assertFalse(zoneFactory.isZonePortal(alice));
        assertFalse(zoneFactory.isZonePortal(address(zoneFactory)));
    }

    function test_zones_returnsEmptyForNonExistentZone() public view {
        ZoneInfo memory info = zoneFactory.zones(999);
        assertEq(info.zoneId, 0);
        assertEq(info.portal, address(0));
        assertEq(info.messenger, address(0));
        assertEq(info.initialToken, address(0));
    }

    function _validCreateZoneParams(string memory zoneRpcUrl)
        internal
        view
        returns (IZoneFactory.CreateZoneParams memory)
    {
        return IZoneFactory.CreateZoneParams({
            initialToken: address(pathUSD),
            sequencer: admin,
            verifier: zoneFactory.verifier(),
            zoneParams: ZoneParams({
                genesisBlockHash: GENESIS_BLOCK_HASH,
                genesisTempoBlockHash: GENESIS_TEMPO_BLOCK_HASH,
                genesisTempoBlockNumber: uint64(block.number)
            }),
            zoneRpcUrl: zoneRpcUrl
        });
    }

    function _invalidZoneRpcUrls() internal pure returns (string[6] memory) {
        return [
            "http://rpc.zone.example",
            "javascript:alert(1)",
            "no-scheme-here",
            "://missing-scheme.example",
            "1https://digit-leading.example",
            " https://leading-space.example"
        ];
    }

    function _httpsUrlOfLength(uint256 len) internal pure returns (string memory) {
        bytes memory prefix = bytes("https://example.com/");
        bytes memory out = new bytes(len);
        for (uint256 i = 0; i < len; i++) {
            out[i] = i < prefix.length ? prefix[i] : bytes1("a");
        }
        return string(out);
    }

}

/// @notice A minimal contract that is NOT a TIP-20
contract NotATIP20 {

    function notATIP20Function() external pure returns (bool) {
        return true;
    }

}
