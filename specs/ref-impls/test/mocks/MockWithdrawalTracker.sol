// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import { IWithdrawalTracker } from "../../src/interfaces/IZone.sol";

/// @notice Unrestricted test double for the native WithdrawalTracker precompile.
contract MockWithdrawalTracker is IWithdrawalTracker {

    mapping(address user => mapping(address token => uint256 amount)) public zoneBalance;
    mapping(address token => uint256 amount) public zoneTotalSupply;

    function deposit(address user, address token, uint256 amount) external {
        zoneBalance[user][token] += amount;
        zoneTotalSupply[token] += amount;
    }

    function withdraw(address user, address token, uint256 amount, uint256 fee) external {
        uint256 requested = amount + fee;
        uint256 available = zoneBalance[user][token];
        if (available < requested) {
            revert InsufficientZoneBalance(user, token, requested, available);
        }
        zoneBalance[user][token] = available - requested;
        zoneTotalSupply[token] -= requested;
    }

}
