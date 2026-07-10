// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import { IWithdrawalReceiver } from "../../src/interfaces/IZone.sol";
import { ZoneMessenger } from "../../src/tempo/ZoneMessenger.sol";
import { BaseTest } from "../BaseTest.t.sol";
import { MockZoneToken } from "../mocks/MockZoneToken.sol";
import { ITIP20 } from "tempo-std/interfaces/ITIP20.sol";

contract AcceptingWithdrawalReceiver is IWithdrawalReceiver {

    function onWithdrawalReceived(
        bytes32,
        address,
        uint128,
        bytes calldata
    )
        external
        pure
        returns (bytes4)
    {
        return IWithdrawalReceiver.onWithdrawalReceived.selector;
    }

}

contract RejectingWithdrawalReceiver is IWithdrawalReceiver {

    function onWithdrawalReceived(
        bytes32,
        address,
        uint128,
        bytes calldata
    )
        external
        pure
        returns (bytes4)
    {
        return bytes4(0xdeadbeef);
    }

}

contract RestrictedPortalMock {

    bytes32 public currentDepositQueueHash;

    function approveToken(address token, address spender, uint256 amount) external {
        require(ITIP20(token).approve(spender, amount));
    }

    function relay(
        ZoneMessenger messenger,
        address token,
        address target,
        uint128 amount,
        bytes calldata data
    )
        external
    {
        messenger.relayMessage(token, bytes32("sender"), target, amount, 5_000_000, data);
    }

    function depositEncrypted(address token, uint128 amount) external returns (bytes32) {
        require(ITIP20(token).transferFrom(msg.sender, address(this), amount));
        currentDepositQueueHash =
            keccak256(abi.encode(currentDepositQueueHash, token, amount, msg.sender));
        return currentDepositQueueHash;
    }

}

contract SwappingWithdrawalReceiver is IWithdrawalReceiver {

    RestrictedPortalMock public immutable portal;
    address public immutable outputToken;
    address public immutable sink;
    bool public immutable returnOutputToZone;

    constructor(
        RestrictedPortalMock _portal,
        address _outputToken,
        address _sink,
        bool _returnOutputToZone
    ) {
        portal = _portal;
        outputToken = _outputToken;
        sink = _sink;
        returnOutputToZone = _returnOutputToZone;
    }

    function onWithdrawalReceived(
        bytes32,
        address token,
        uint128 amount,
        bytes calldata
    )
        external
        returns (bytes4)
    {
        require(ITIP20(token).transfer(sink, amount));
        if (returnOutputToZone) {
            require(ITIP20(outputToken).approve(address(portal), amount));
            portal.depositEncrypted(outputToken, amount);
        }
        return IWithdrawalReceiver.onWithdrawalReceived.selector;
    }

}

contract DirectReturnWithdrawalReceiver is IWithdrawalReceiver {

    address public immutable portal;
    address public immutable outputToken;
    address public immutable sink;

    constructor(address _portal, address _outputToken, address _sink) {
        portal = _portal;
        outputToken = _outputToken;
        sink = _sink;
    }

    function onWithdrawalReceived(
        bytes32,
        address token,
        uint128 amount,
        bytes calldata
    )
        external
        returns (bytes4)
    {
        require(ITIP20(token).transfer(sink, amount));
        require(ITIP20(outputToken).transfer(portal, amount));
        return IWithdrawalReceiver.onWithdrawalReceived.selector;
    }

}

contract ZoneMessengerTest is BaseTest {

    ZoneMessenger public messenger;
    MockZoneToken public zoneToken;
    address public portal = address(0x700);
    address public token = address(0x701);

    function setUp() public override {
        messenger = new ZoneMessenger(portal, address(0), address(0), address(0));
        zoneToken = new MockZoneToken("Zone USD", "zUSD");
        zoneToken.setMinter(address(this), true);
    }

    function _mockTransferFrom(address target, uint128 amount, bool result) internal {
        vm.mockCall(
            token,
            abi.encodeWithSelector(ITIP20.transferFrom.selector, portal, target, amount),
            abi.encode(result)
        );
    }

    /// @notice Verifies the messenger stores the portal address immutably.
    function test_portalImmutable() public view {
        assertEq(messenger.portal(), portal);
    }

    /// @notice Verifies only the portal can relay withdrawal messages.
    function test_relayMessage_revertsOnlyPortalForNonPortalCaller() public {
        vm.expectRevert(ZoneMessenger.OnlyPortal.selector);
        messenger.relayMessage(token, bytes32("sender"), alice, 1, 50_000, "");
    }

    /// @notice Verifies relay reverts when token transferFrom returns false.
    function test_relayMessage_revertsTransferFailedWhenTransferFromReturnsFalse() public {
        AcceptingWithdrawalReceiver receiver = new AcceptingWithdrawalReceiver();
        _mockTransferFrom(address(receiver), 1, false);

        vm.prank(portal);
        vm.expectRevert(ZoneMessenger.TransferFailed.selector);
        messenger.relayMessage(token, bytes32("sender"), address(receiver), 1, 50_000, "");
    }

    /// @notice Verifies relay reverts when the receiver returns the wrong selector.
    function test_relayMessage_revertsCallbackRejectedForWrongSelector() public {
        RejectingWithdrawalReceiver receiver = new RejectingWithdrawalReceiver();
        _mockTransferFrom(address(receiver), 1, true);

        vm.prank(portal);
        vm.expectRevert(ZoneMessenger.CallbackRejected.selector);
        messenger.relayMessage(token, bytes32("sender"), address(receiver), 1, 50_000, "");
    }

    /// @notice Verifies relay to an EOA target reverts after transfer succeeds.
    function test_relayMessage_revertsForEoaTarget() public {
        _mockTransferFrom(alice, 1, true);

        vm.prank(portal);
        vm.expectRevert();
        messenger.relayMessage(token, bytes32("sender"), alice, 1, 50_000, "");
    }

    /// @notice Verifies a valid relay transfers tokens to an accepting receiver.
    function test_relayMessage_success() public {
        AcceptingWithdrawalReceiver receiver = new AcceptingWithdrawalReceiver();
        bytes32 senderTag = keccak256("sender");
        bytes memory data = hex"1234";
        zoneToken.mint(portal, 123);
        vm.prank(portal);
        zoneToken.approve(address(messenger), 123);

        vm.prank(portal);
        messenger.relayMessage(
            address(zoneToken), senderTag, address(receiver), 123, 1_000_000, data
        );

        assertEq(zoneToken.balanceOf(address(receiver)), 123);
    }

    function test_restrictedRelay_revertsForWrongTarget() public {
        AcceptingWithdrawalReceiver receiver = new AcceptingWithdrawalReceiver();
        ZoneMessenger restricted =
            new ZoneMessenger(portal, address(receiver), token, address(zoneToken));

        vm.prank(portal);
        vm.expectRevert(ZoneMessenger.InvalidRestrictedTarget.selector);
        restricted.relayMessage(token, bytes32("sender"), alice, 1, 50_000, "");
    }

    function test_restrictedRelay_revertsForNonVaultToken() public {
        AcceptingWithdrawalReceiver receiver = new AcceptingWithdrawalReceiver();
        ZoneMessenger restricted =
            new ZoneMessenger(portal, address(receiver), address(zoneToken), address(0x702));

        vm.prank(portal);
        vm.expectRevert(ZoneMessenger.InvalidVaultToken.selector);
        restricted.relayMessage(token, bytes32("sender"), address(receiver), 1, 50_000, "");
    }

    function test_restrictedRelay_revertsWhenOutputIsNotDepositedBackToZone() public {
        MockZoneToken receiptToken = new MockZoneToken("Vault Receipt", "vUSD");
        RestrictedPortalMock restrictedPortal = new RestrictedPortalMock();
        SwappingWithdrawalReceiver receiver =
            new SwappingWithdrawalReceiver(restrictedPortal, address(receiptToken), alice, false);
        ZoneMessenger restricted = new ZoneMessenger(
            address(restrictedPortal), address(receiver), address(zoneToken), address(receiptToken)
        );
        zoneToken.mint(address(restrictedPortal), 123);
        restrictedPortal.approveToken(address(zoneToken), address(restricted), 123);

        vm.expectRevert(ZoneMessenger.VaultSwapInvariantViolated.selector);
        restrictedPortal.relay(restricted, address(zoneToken), address(receiver), 123, "");
    }

    function test_restrictedRelay_revertsWhenOutputTransferDoesNotEnqueueZoneDeposit() public {
        MockZoneToken receiptToken = new MockZoneToken("Vault Receipt", "vUSD");
        receiptToken.setMinter(address(this), true);
        RestrictedPortalMock restrictedPortal = new RestrictedPortalMock();
        DirectReturnWithdrawalReceiver receiver = new DirectReturnWithdrawalReceiver(
            address(restrictedPortal), address(receiptToken), alice
        );
        ZoneMessenger restricted = new ZoneMessenger(
            address(restrictedPortal), address(receiver), address(zoneToken), address(receiptToken)
        );
        zoneToken.mint(address(restrictedPortal), 123);
        receiptToken.mint(address(receiver), 123);
        restrictedPortal.approveToken(address(zoneToken), address(restricted), 123);

        vm.expectRevert(ZoneMessenger.VaultSwapInvariantViolated.selector);
        restrictedPortal.relay(restricted, address(zoneToken), address(receiver), 123, "");
    }

    function test_restrictedRelay_assetToReceiptDepositsOutputBackToZone() public {
        _assertRestrictedSwap(true);
    }

    function test_restrictedRelay_receiptToAssetDepositsOutputBackToZone() public {
        _assertRestrictedSwap(false);
    }

    /// @notice Verifies valid relays transfer any bounded amount to the receiver.
    function testFuzz_relayMessage_success(
        uint128 amount,
        uint64 gasLimit,
        bytes calldata data
    )
        public
    {
        vm.assume(gasLimit >= 500_000);
        amount = uint128(bound(amount, 0, 1_000_000_000e6));
        AcceptingWithdrawalReceiver receiver = new AcceptingWithdrawalReceiver();
        bytes32 senderTag = keccak256(abi.encode(amount, gasLimit, data));
        zoneToken.mint(portal, amount);
        vm.prank(portal);
        zoneToken.approve(address(messenger), amount);

        vm.prank(portal);
        messenger.relayMessage(
            address(zoneToken), senderTag, address(receiver), amount, gasLimit, data
        );

        assertEq(zoneToken.balanceOf(address(receiver)), amount);
    }

    function _assertRestrictedSwap(bool assetToReceipt) internal {
        MockZoneToken receiptToken = new MockZoneToken("Vault Receipt", "vUSD");
        receiptToken.setMinter(address(this), true);
        RestrictedPortalMock restrictedPortal = new RestrictedPortalMock();
        address inputToken = assetToReceipt ? address(zoneToken) : address(receiptToken);
        address outputToken = assetToReceipt ? address(receiptToken) : address(zoneToken);
        SwappingWithdrawalReceiver receiver =
            new SwappingWithdrawalReceiver(restrictedPortal, outputToken, alice, true);
        ZoneMessenger restricted = new ZoneMessenger(
            address(restrictedPortal), address(receiver), address(zoneToken), address(receiptToken)
        );

        MockZoneToken(inputToken).mint(address(restrictedPortal), 123);
        MockZoneToken(outputToken).mint(address(receiver), 123);
        restrictedPortal.approveToken(inputToken, address(restricted), 123);
        restrictedPortal.relay(restricted, inputToken, address(receiver), 123, "");

        assertEq(MockZoneToken(inputToken).balanceOf(address(receiver)), 0);
        assertEq(MockZoneToken(outputToken).balanceOf(address(restrictedPortal)), 123);
        assertTrue(restrictedPortal.currentDepositQueueHash() != bytes32(0));
    }

}
