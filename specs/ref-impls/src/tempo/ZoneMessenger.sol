// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import { IWithdrawalReceiver, IZoneMessenger, IZonePortal } from "../interfaces/IZone.sol";
import { ITIP20 } from "tempo-std/interfaces/ITIP20.sol";

/// @title ZoneMessenger
/// @notice Per-zone messenger that handles withdrawal callbacks
/// @dev Deployed by ZoneFactory for each zone. The portal gives the messenger max approval
///      for each enabled token. Withdrawal callbacks originate from this contract, not the portal.
contract ZoneMessenger is IZoneMessenger {

    /*//////////////////////////////////////////////////////////////
                                STORAGE
    //////////////////////////////////////////////////////////////*/

    /// @notice The zone's portal address
    address public immutable portal;
    address public immutable restrictedTarget;
    address public immutable vaultAsset;
    address public immutable vaultReceipt;

    /*//////////////////////////////////////////////////////////////
                                ERRORS
    //////////////////////////////////////////////////////////////*/

    error OnlyPortal();
    error CallbackRejected();
    error TransferFailed();
    error InvalidRestrictedConfig();
    error InvalidRestrictedTarget();
    error InvalidVaultToken();
    error VaultSwapInvariantViolated();

    /*//////////////////////////////////////////////////////////////
                              CONSTRUCTOR
    //////////////////////////////////////////////////////////////*/

    constructor(
        address _portal,
        address _restrictedTarget,
        address _vaultAsset,
        address _vaultReceipt
    ) {
        bool unrestricted = _restrictedTarget == address(0) && _vaultAsset == address(0)
            && _vaultReceipt == address(0);
        bool restricted = _restrictedTarget != address(0) && _vaultAsset != address(0)
            && _vaultReceipt != address(0) && _vaultAsset != _vaultReceipt;
        if (!unrestricted && !restricted) revert InvalidRestrictedConfig();

        portal = _portal;
        restrictedTarget = _restrictedTarget;
        vaultAsset = _vaultAsset;
        vaultReceipt = _vaultReceipt;
    }

    /*//////////////////////////////////////////////////////////////
                               MODIFIERS
    //////////////////////////////////////////////////////////////*/

    modifier onlyPortal() {
        if (msg.sender != portal) revert OnlyPortal();
        _;
    }

    /*//////////////////////////////////////////////////////////////
                           MESSAGE RELAY
    //////////////////////////////////////////////////////////////*/

    /// @notice Relay a withdrawal message. Only callable by the portal.
    /// @dev Transfers tokens from portal to target via transferFrom, then executes callback.
    /// @param token The TIP-20 token to transfer
    /// @param senderTag The authenticated sender commitment from the zone
    /// @param target The Tempo recipient
    /// @param amount Tokens to transfer from portal to target
    /// @param gasLimit Max gas for the callback
    /// @param data Calldata for the target
    function relayMessage(
        address token,
        bytes32 senderTag,
        address target,
        uint128 amount,
        uint64 gasLimit,
        bytes calldata data
    )
        external
        onlyPortal
    {
        bool restricted = restrictedTarget != address(0);
        if (restricted && target != restrictedTarget) revert InvalidRestrictedTarget();
        if (restricted && token != vaultAsset && token != vaultReceipt) {
            revert InvalidVaultToken();
        }

        address outputToken;
        uint256 outputPortalBalanceBefore;
        bytes32 depositQueueHashBefore;
        if (restricted) {
            outputToken = token == vaultAsset ? vaultReceipt : vaultAsset;
            outputPortalBalanceBefore = ITIP20(outputToken).balanceOf(portal);
            depositQueueHashBefore = IZonePortal(portal).currentDepositQueueHash();
        }

        // Transfer tokens from portal to target
        if (!ITIP20(token).transferFrom(portal, target, amount)) {
            revert TransferFailed();
        }

        uint256 balanceBeforeCallback;
        if (restricted) balanceBeforeCallback = ITIP20(token).balanceOf(target);

        // Call only the standardized withdrawal receiver entrypoint.
        (bool callbackSuccess, bytes memory returnData) = target.call{ gas: gasLimit }(
            abi.encodeCall(
                IWithdrawalReceiver.onWithdrawalReceived, (senderTag, token, amount, data)
            )
        );

        // Verify the callback returned the correct selector
        if (
            !callbackSuccess || returnData.length != 32
                || abi.decode(returnData, (bytes4))
                    != IWithdrawalReceiver.onWithdrawalReceived.selector
        ) {
            revert CallbackRejected();
        }
        if (
            restricted
                && (ITIP20(token).balanceOf(target) >= balanceBeforeCallback
                    || ITIP20(outputToken).balanceOf(portal) <= outputPortalBalanceBefore
                    || IZonePortal(portal).currentDepositQueueHash() == depositQueueHashBefore)
        ) {
            revert VaultSwapInvariantViolated();
        }
    }

}
