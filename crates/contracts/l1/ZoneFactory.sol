// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import { IZoneFactory, ZoneInfo } from "../interfaces/IZone.sol";
import { Verifier } from "./Verifier.sol";
import { ZonePortal } from "./ZonePortal.sol";
import { StdPrecompiles } from "tempo-std/StdPrecompiles.sol";
import { ITIP20Factory } from "tempo-std/interfaces/ITIP20Factory.sol";

/// @title ZoneFactory
/// @notice Creates zones and registers parameters
contract ZoneFactory is IZoneFactory {

    /*//////////////////////////////////////////////////////////////
                                STORAGE
    //////////////////////////////////////////////////////////////*/

    /// @notice Minimum gas required for zone creation.
    /// @dev Prevents low-cost zone spam. The caller must supply at least this much gas.
    uint256 public constant ZONE_CREATION_GAS = 15_000_000;

    /// @notice Next zone ID to be assigned
    /// @dev Starts at 1, reserving zone ID 0 for potential future use (e.g., mainnet as zone 0)
    uint32 internal _nextZoneId = 1;

    mapping(uint32 => ZoneInfo) internal _zones;
    mapping(address => bool) internal _isZonePortal;
    mapping(address => bool) internal _validVerifiers;
    address internal _verifier;

    /*//////////////////////////////////////////////////////////////
                              CONSTRUCTOR
    //////////////////////////////////////////////////////////////*/

    constructor() {
        address v = address(new Verifier());
        _validVerifiers[v] = true;
        _verifier = v;
    }

    /*//////////////////////////////////////////////////////////////
                            ZONE CREATION
    //////////////////////////////////////////////////////////////*/

    function createZone(CreateZoneParams calldata params)
        external
        returns (uint32 zoneId, address portal)
    {
        // Validate initial token is a TIP-20
        if (!ITIP20Factory(StdPrecompiles.TIP20_FACTORY_ADDRESS).isTIP20(params.initialToken)) {
            revert InvalidToken();
        }
        if (params.admin == address(0)) revert InvalidAdmin();
        if (params.sequencer == address(0)) revert InvalidSequencer();
        if (!_validVerifiers[params.verifier]) revert InvalidVerifier();
        if (gasleft() < ZONE_CREATION_GAS) revert InsufficientGas();

        zoneId = _nextZoneId;
        if (zoneId == type(uint32).max) revert ZoneIdOverflow();
        _nextZoneId = zoneId + 1;

        // Deploy portal. The portal constructor enables the initial token automatically.
        ZonePortal portalContract = new ZonePortal(
            zoneId,
            params.initialToken,
            params.admin,
            params.sequencer,
            params.verifier,
            params.zoneParams.genesisBlockHash,
            params.zoneParams.genesisTempoBlockNumber,
            params.rpcUrl
        );
        portal = address(portalContract);

        // Store zone info
        _zones[zoneId] = ZoneInfo({
            zoneId: zoneId,
            portal: portal,
            initialToken: params.initialToken,
            admin: params.admin,
            sequencer: params.sequencer,
            verifier: params.verifier,
            genesisBlockHash: params.zoneParams.genesisBlockHash,
            genesisTempoBlockHash: params.zoneParams.genesisTempoBlockHash,
            genesisTempoBlockNumber: params.zoneParams.genesisTempoBlockNumber,
            rpcUrl: params.rpcUrl
        });

        _isZonePortal[portal] = true;

        emit ZoneCreated(
            zoneId,
            portal,
            params.initialToken,
            params.admin,
            params.sequencer,
            params.verifier,
            params.zoneParams.genesisBlockHash,
            params.zoneParams.genesisTempoBlockHash,
            params.zoneParams.genesisTempoBlockNumber
        );
    }

    /*//////////////////////////////////////////////////////////////
                                 VIEWS
    //////////////////////////////////////////////////////////////*/

    /// @notice Returns the number of zones created (not including reserved zone 0)
    function zoneCount() external view returns (uint32) {
        return _nextZoneId - 1;
    }

    function zones(uint32 zoneId) external view returns (ZoneInfo memory) {
        return _zones[zoneId];
    }

    function isZonePortal(address portal) external view returns (bool) {
        return _isZonePortal[portal];
    }

    function isValidVerifier(address v) external view returns (bool) {
        return _validVerifiers[v];
    }

    function verifier() external view returns (address) {
        return _verifier;
    }

}
