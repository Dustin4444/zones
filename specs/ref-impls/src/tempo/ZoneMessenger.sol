// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import {
    IWithdrawalReceiver,
    IZoneFactory,
    IZoneMessenger,
    IZonePortal,
    Role,
    ZONE_FACTORY_ADDRESS,
    ZoneInfo
} from "../interfaces/IZone.sol";
import { ITIP20 } from "tempo-std/interfaces/ITIP20.sol";

/// @title ZoneMessenger
/// @notice Shared withdrawal callback sender for all zones created by one ZoneFactory.
contract ZoneMessenger is IZoneMessenger {

    IZoneFactory public constant zoneFactory = IZoneFactory(ZONE_FACTORY_ADDRESS);

    uint256 internal _relayReentrancyStatus;

    error UnauthorizedPortal();
    error TransferFailed();
    error CallbackRejected();
    error InvalidCallbackTarget();
    error ReentrantRelay();

    modifier nonReentrantRelay() {
        if (_relayReentrancyStatus != 0) revert ReentrantRelay();
        _relayReentrancyStatus = 1;
        _;
        _relayReentrancyStatus = 0;
    }

    function relayMessage(
        uint32 zoneId,
        address token,
        bytes32 senderTag,
        address target,
        uint128 amount,
        bytes calldata data
    )
        external
        nonReentrantRelay
    {
        ZoneInfo memory zone = zoneFactory.zones(zoneId);
        if (zone.portal != msg.sender) revert UnauthorizedPortal();

        if (
            !IZonePortal(msg.sender).isGatewayOpen()
                && IZonePortal(msg.sender).role(target) != Role.CallbackGateway
        ) {
            revert InvalidCallbackTarget();
        }

        if (!ITIP20(token).transfer(target, amount)) {
            revert TransferFailed();
        }

        bytes memory callback = abi.encodeCall(
            IWithdrawalReceiver.onWithdrawalReceived,
            (zoneId, msg.sender, senderTag, token, amount, data)
        );
        bool success;
        bytes4 selector;
        assembly ("memory-safe") {
            success := call(gas(), target, 0, add(callback, 0x20), mload(callback), 0, 0)
            if success {
                if lt(returndatasize(), 4) { success := false }
                if success {
                    returndatacopy(0, 0, 4)
                    selector := mload(0)
                }
            }
        }

        if (!success || selector != IWithdrawalReceiver.onWithdrawalReceived.selector) {
            revert CallbackRejected();
        }
    }

}
