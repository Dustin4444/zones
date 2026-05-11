// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

/// @title ZoneRpcUrlLib
/// @notice Validation helpers for zone RPC URL metadata.
library ZoneRpcUrlLib {

    /// @notice Maximum allowed byte length of a zone RPC URL.
    uint256 internal constant MAX_ZONE_RPC_URL_BYTES = 256;

    bytes1 private constant COLON = 0x3a;

    error ZoneRpcUrlTooLong();
    error InvalidZoneRpcUrl();

    /// @notice Validate a zone RPC URL.
    /// @dev Empty strings are valid and clear the URL. Non-empty URLs must have an
    ///      `https` scheme, compared ASCII-case-insensitively. The URI body after
    ///      the first `:` is intentionally not validated.
    function validate(string memory url) internal pure {
        bytes memory uri = bytes(url);
        if (uri.length > MAX_ZONE_RPC_URL_BYTES) revert ZoneRpcUrlTooLong();
        if (uri.length == 0) return;
        if (!_hasHttpsScheme(uri)) revert InvalidZoneRpcUrl();
    }

    function _hasHttpsScheme(bytes memory uri) private pure returns (bool) {
        for (uint256 i = 0; i < uri.length; i++) {
            if (uri[i] == COLON) {
                return i == 5 && _eqAsciiCaseInsensitive(uri[0], 0x68)
                    && _eqAsciiCaseInsensitive(uri[1], 0x74)
                    && _eqAsciiCaseInsensitive(uri[2], 0x74)
                    && _eqAsciiCaseInsensitive(uri[3], 0x70)
                    && _eqAsciiCaseInsensitive(uri[4], 0x73);
            }
        }
        return false;
    }

    function _eqAsciiCaseInsensitive(bytes1 value, uint8 lower) private pure returns (bool) {
        uint8 c = uint8(value);
        return c == lower || c == lower - 32;
    }

}
