// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import {
    EncryptedDepositPayload,
    IWithdrawalReceiver,
    IZoneFactory,
    IZoneMessenger,
    IZonePortal
} from "../../src/zone/IZone.sol";
import { SwapAndDepositRouter } from "../../src/zone/SwapAndDepositRouter.sol";
import { BaseTest } from "../BaseTest.t.sol";
import { IStablecoinDEX } from "tempo-std/interfaces/IStablecoinDEX.sol";
import { ITIP20 } from "tempo-std/interfaces/ITIP20.sol";

contract MockZoneFactoryForRouter {

    mapping(address => bool) public portalMap;
    mapping(address => bool) public messengerMap;

    function setPortal(address portal, bool registered) external {
        portalMap[portal] = registered;
    }

    function setMessenger(address messenger, bool registered) external {
        messengerMap[messenger] = registered;
    }

    function isZonePortal(address portal) external view returns (bool) {
        return portalMap[portal];
    }

    function isZoneMessenger(address messenger) external view returns (bool) {
        return messengerMap[messenger];
    }

}

contract MockZoneMessengerForRouter { }

contract MockZonePortalForRouter {

    mapping(address => bool) public enabledTokens;

    address public lastDepositRecipient;
    address public lastDepositBouncebackRecipient;
    uint128 public lastDepositAmount;
    bytes32 public lastDepositMemo;
    bool public depositCalled;

    uint128 public lastEncryptedAmount;
    uint256 public lastEncryptedKeyIndex;
    address public lastEncryptedBouncebackRecipient;
    bool public encryptedDepositCalled;

    function enableToken(address _token) external {
        enabledTokens[_token] = true;
    }

    function isTokenEnabled(address _token) external view returns (bool) {
        return enabledTokens[_token];
    }

    function deposit(
        address _token,
        address to,
        uint128 amount,
        bytes32 memo,
        address bouncebackRecipient
    )
        external
        returns (bytes32)
    {
        ITIP20(_token).transferFrom(msg.sender, address(this), amount);
        lastDepositRecipient = to;
        lastDepositBouncebackRecipient = bouncebackRecipient;
        lastDepositAmount = amount;
        lastDepositMemo = memo;
        depositCalled = true;
        return bytes32(0);
    }

    function depositEncrypted(
        address _token,
        uint128 amount,
        uint256 keyIndex,
        EncryptedDepositPayload calldata,
        address bouncebackRecipient
    )
        external
        returns (bytes32)
    {
        ITIP20(_token).transferFrom(msg.sender, address(this), amount);
        lastEncryptedAmount = amount;
        lastEncryptedKeyIndex = keyIndex;
        lastEncryptedBouncebackRecipient = bouncebackRecipient;
        encryptedDepositCalled = true;
        return bytes32(0);
    }

}

contract SwapAndDepositRouterTest is BaseTest {

    SwapAndDepositRouter public router;
    MockZoneFactoryForRouter public mockFactory;
    MockZoneMessengerForRouter public mockMessenger;
    MockZonePortalForRouter public mockPortal;
    MockZonePortalForRouter public mockPortal2;

    bytes32 public senderTag = keccak256(abi.encodePacked(address(0x500)));
    uint128 public constant AMOUNT = 1000e6;

    // Shared fragmented order-book config (seeded in setUp).
    uint256 internal constant FRAG_NUM_ORDERS = 2;
    uint128 internal fragPerOrder;

    function setUp() public override {
        super.setUp();

        mockFactory = new MockZoneFactoryForRouter();
        mockMessenger = new MockZoneMessengerForRouter();
        mockPortal = new MockZonePortalForRouter();
        mockPortal2 = new MockZonePortalForRouter();

        // Router runs against the real StablecoinDEX precompile (no mock).
        router = new SwapAndDepositRouter(address(exchange), address(mockFactory));

        mockFactory.setMessenger(address(mockMessenger), true);
        mockFactory.setPortal(address(mockPortal), true);
        mockFactory.setPortal(address(mockPortal2), true);

        mockPortal.enableToken(address(pathUSD));
        mockPortal2.enableToken(address(token1));

        // Fund the router with the incoming withdrawal token (pathUSD). The
        // tokenOut (token1) is obtained from the real swap, not pre-minted.
        vm.startPrank(pathUSDAdmin);
        pathUSD.grantRole(_ISSUER_ROLE, pathUSDAdmin);
        pathUSD.mint(address(router), AMOUNT * 10);
        vm.stopPrank();

        // Make admin a token1 issuer so the liquidity-seeding helper can mint.
        vm.prank(admin);
        token1.grantRole(_ISSUER_ROLE, admin);

        // Seed a fragmented book so every swap test runs against real per-order
        // rounding (executed output can land below the quote).
        fragPerOrder = exchange.MIN_ORDER_AMOUNT() * 5 + 3;
        _seedFragmentedLiquidity(FRAG_NUM_ORDERS, fragPerOrder);
    }

    /// @dev Seed fragmented ask liquidity for token1/pathUSD. Two orders per tick
    ///      force per-order splitting, so quote and execution can diverge (executed
    ///      output lands strictly below the quote).
    function _seedFragmentedLiquidity(uint256 numOrders, uint128 perOrder) internal {
        exchange.createPair(address(token1));

        vm.prank(admin);
        token1.mint(bob, uint256(perOrder) * numOrders * 2 + perOrder);

        vm.prank(bob);
        token1.approve(address(exchange), type(uint256).max);

        int16 spacing = exchange.TICK_SPACING();
        for (uint256 i = 0; i < numOrders; i++) {
            int16 tick = int16(int256(i + 1)) * spacing;
            vm.prank(bob);
            exchange.place(address(token1), perOrder, false, tick);
            vm.prank(bob);
            exchange.place(address(token1), perOrder, false, tick);
        }
    }

    function _buildPlaintextData(
        address tokenOut,
        address targetPortal,
        address recipient,
        bytes32 memo,
        uint128 minAmountOut
    )
        internal
        pure
        returns (bytes memory)
    {
        return abi.encode(false, tokenOut, targetPortal, recipient, memo, minAmountOut);
    }

    function _buildEncryptedData(
        address tokenOut,
        address targetPortal,
        uint256 keyIndex,
        EncryptedDepositPayload memory encrypted,
        uint128 minAmountOut
    )
        internal
        pure
        returns (bytes memory)
    {
        return abi.encode(true, tokenOut, targetPortal, keyIndex, encrypted, minAmountOut);
    }

    function _defaultEncryptedPayload() internal pure returns (EncryptedDepositPayload memory) {
        return EncryptedDepositPayload({
            ephemeralPubkeyX: bytes32(uint256(0x1234)),
            ephemeralPubkeyYParity: 0x02,
            ciphertext: hex"deadbeef",
            nonce: bytes12(uint96(42)),
            tag: bytes16(uint128(99))
        });
    }

    function test_revertUnauthorizedMessenger() public {
        bytes memory data =
            _buildPlaintextData(address(pathUSD), address(mockPortal), alice, bytes32("memo"), 0);

        vm.prank(alice);
        vm.expectRevert(SwapAndDepositRouter.UnauthorizedMessenger.selector);
        router.onWithdrawalReceived(senderTag, address(pathUSD), AMOUNT, data);
    }

    function test_revertInvalidTargetPortal() public {
        address fakePortal = address(0xFAFAFA);
        bytes memory data =
            _buildPlaintextData(address(pathUSD), fakePortal, alice, bytes32("memo"), 0);

        vm.prank(address(mockMessenger));
        vm.expectRevert(SwapAndDepositRouter.InvalidTargetPortal.selector);
        router.onWithdrawalReceived(senderTag, address(pathUSD), AMOUNT, data);
    }

    function test_revertInvalidToken() public {
        bytes memory data =
            _buildPlaintextData(address(token1), address(mockPortal), alice, bytes32("memo"), 0);

        vm.prank(address(mockMessenger));
        vm.expectRevert(SwapAndDepositRouter.InvalidToken.selector);
        router.onWithdrawalReceived(senderTag, address(pathUSD), AMOUNT, data);
    }

    function test_plaintextDeposit_sameToken() public {
        bytes memory data =
            _buildPlaintextData(address(pathUSD), address(mockPortal), alice, bytes32("hello"), 0);

        vm.prank(address(mockMessenger));
        bytes4 ret = router.onWithdrawalReceived(senderTag, address(pathUSD), AMOUNT, data);

        assertEq(ret, IWithdrawalReceiver.onWithdrawalReceived.selector);
        assertTrue(mockPortal.depositCalled());
        assertEq(mockPortal.lastDepositRecipient(), alice);
        assertEq(mockPortal.lastDepositBouncebackRecipient(), alice);
        assertEq(mockPortal.lastDepositAmount(), AMOUNT);
        assertEq(mockPortal.lastDepositMemo(), bytes32("hello"));
    }

    function test_plaintextDeposit_withSwap() public {
        // Against the fragmented book, minAmountOut == quote can revert because
        // per-order rounding makes the executed output fall below the quote.
        uint128 swapOut = exchange.quoteSwapExactAmountIn(address(pathUSD), address(token1), AMOUNT);
        assertLt(swapOut, AMOUNT, "expected price impact");

        bytes memory data = _buildPlaintextData(
            address(token1), address(mockPortal2), alice, bytes32("swap"), swapOut
        );

        vm.prank(address(mockMessenger));
        bytes4 ret = router.onWithdrawalReceived(senderTag, address(pathUSD), AMOUNT, data);

        assertEq(ret, IWithdrawalReceiver.onWithdrawalReceived.selector);
        assertTrue(mockPortal2.depositCalled());
        assertEq(mockPortal2.lastDepositRecipient(), alice);
        assertEq(mockPortal2.lastDepositBouncebackRecipient(), alice);
        assertEq(mockPortal2.lastDepositAmount(), swapOut);
        assertEq(mockPortal2.lastDepositMemo(), bytes32("swap"));
    }

    function test_encryptedDeposit_sameToken() public {
        EncryptedDepositPayload memory payload = _defaultEncryptedPayload();
        bytes memory data =
            _buildEncryptedData(address(pathUSD), address(mockPortal), 0, payload, 0);

        vm.prank(address(mockMessenger));
        bytes4 ret = router.onWithdrawalReceived(senderTag, address(pathUSD), AMOUNT, data);

        assertEq(ret, IWithdrawalReceiver.onWithdrawalReceived.selector);
        assertTrue(mockPortal.encryptedDepositCalled());
        assertEq(mockPortal.lastEncryptedAmount(), AMOUNT);
        assertEq(mockPortal.lastEncryptedKeyIndex(), 0);
        assertEq(mockPortal.lastEncryptedBouncebackRecipient(), address(router));
    }

    function test_encryptedDeposit_withSwap() public {
        // Same caveat as the plaintext variant: minAmountOut == quote can revert.
        uint128 swapOut = exchange.quoteSwapExactAmountIn(address(pathUSD), address(token1), AMOUNT);
        assertLt(swapOut, AMOUNT, "expected price impact");

        EncryptedDepositPayload memory payload = _defaultEncryptedPayload();
        bytes memory data =
            _buildEncryptedData(address(token1), address(mockPortal2), 1, payload, swapOut);

        vm.prank(address(mockMessenger));
        bytes4 ret = router.onWithdrawalReceived(senderTag, address(pathUSD), AMOUNT, data);

        assertEq(ret, IWithdrawalReceiver.onWithdrawalReceived.selector);
        assertTrue(mockPortal2.encryptedDepositCalled());
        assertEq(mockPortal2.lastEncryptedAmount(), swapOut);
        assertEq(mockPortal2.lastEncryptedKeyIndex(), 1);
        assertEq(mockPortal2.lastEncryptedBouncebackRecipient(), address(router));
    }

    function test_swapSlippageReverts() public {
        // Output is below AMOUNT (price impact), so requiring AMOUNT must revert.
        bytes memory data = _buildPlaintextData(
            address(token1), address(mockPortal2), alice, bytes32("slip"), AMOUNT
        );

        vm.prank(address(mockMessenger));
        vm.expectRevert(IStablecoinDEX.InsufficientOutput.selector);
        router.onWithdrawalReceived(senderTag, address(pathUSD), AMOUNT, data);
    }

    /// @notice The router must deposit exactly the DEX's executed output (never a
    ///         quote), strand no token dust, and consume the full input, even when
    ///         per-order rounding makes the execution diverge from the quote.
    function testFuzz_depositsExecutedOutput(uint128 amountIn) public {
        // Bound amountIn under the available liquidity (full fill) and above a
        // min order (so it executes).
        amountIn =
            uint128(bound(amountIn, exchange.MIN_ORDER_AMOUNT(), fragPerOrder * FRAG_NUM_ORDERS));

        uint128 quoted =
            exchange.quoteSwapExactAmountIn(address(pathUSD), address(token1), amountIn);

        bytes memory data =
            _buildPlaintextData(address(token1), address(mockPortal2), alice, bytes32("swap"), 0);

        uint256 routerPathBefore = pathUSD.balanceOf(address(router));
        uint256 portalToken1Before = token1.balanceOf(address(mockPortal2));

        vm.prank(address(mockMessenger));
        bytes4 ret = router.onWithdrawalReceived(senderTag, address(pathUSD), amountIn, data);
        assertEq(ret, IWithdrawalReceiver.onWithdrawalReceived.selector);

        uint128 deposited = mockPortal2.lastDepositAmount();

        // The portal received exactly the recorded deposit amount...
        assertEq(
            token1.balanceOf(address(mockPortal2)) - portalToken1Before,
            deposited,
            "portal received exactly the deposited amount"
        );
        // ...which is the real executed output: no tokenOut dust left in router.
        assertEq(token1.balanceOf(address(router)), 0, "no stranded token1 dust");
        // The DEX consumes the full input up front (overpayment favors protocol).
        assertEq(
            routerPathBefore - pathUSD.balanceOf(address(router)), amountIn, "full input consumed"
        );
        // Execution never exceeds the quote.
        assertLe(deposited, quoted, "executed output <= quote");
        assertGt(deposited, 0, "swap produced nonzero output");
    }

    /// @notice Deterministic case where the executed output is strictly below the
    ///         quote, so depositing with `minAmountOut == quote` reverts.
    /// @dev amountIn = 900000012 quotes to 899910020 but executes 899910019.
    function test_minEqualsQuoteReverts() public {
        uint128 amountIn = 900_000_012;

        uint128 quoted =
            exchange.quoteSwapExactAmountIn(address(pathUSD), address(token1), amountIn);

        // Execute the swap once (minAmountOut = 0) to observe the real output.
        uint256 snap = vm.snapshotState();
        bytes memory openData =
            _buildPlaintextData(address(token1), address(mockPortal2), alice, bytes32("swap"), 0);
        vm.prank(address(mockMessenger));
        router.onWithdrawalReceived(senderTag, address(pathUSD), amountIn, openData);
        uint128 executed = mockPortal2.lastDepositAmount();

        // Per-order rounding makes execution fall short of the quote.
        assertLt(executed, quoted, "executed must be strictly below quote here");

        // Replay from the same state, this time using the quote as minAmountOut.
        vm.revertToState(snap);
        bytes memory tightData = _buildPlaintextData(
            address(token1), address(mockPortal2), alice, bytes32("swap"), quoted
        );
        vm.prank(address(mockMessenger));
        vm.expectRevert(IStablecoinDEX.InsufficientOutput.selector);
        router.onWithdrawalReceived(senderTag, address(pathUSD), amountIn, tightData);
    }

}
