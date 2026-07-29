// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import {
    EncryptedDepositPayload,
    IWithdrawalReceiver,
    IZoneFactory,
    IZonePortal,
    ZONE_MESSENGER_ADDRESS,
    ZoneInfo
} from "../../src/interfaces/IZone.sol";
import { SwapAndDepositRouter } from "../../src/tempo/SwapAndDepositRouter.sol";
import { BaseTest } from "../BaseTest.t.sol";
import { IStablecoinDEX } from "tempo-std/interfaces/IStablecoinDEX.sol";
import { ITIP20 } from "tempo-std/interfaces/ITIP20.sol";

contract MockZoneFactoryForRouter {

    mapping(address => bool) public portalMap;
    mapping(uint32 => ZoneInfo) internal _zones;

    function setPortal(address portal, bool registered) external {
        portalMap[portal] = registered;
    }

    function setSourcePortal(uint32 zoneId, address portal) external {
        _zones[zoneId].zoneId = zoneId;
        _zones[zoneId].portal = portal;
    }

    function isZonePortal(address portal) external view returns (bool) {
        return portalMap[portal];
    }

    function zones(uint32 id) external view returns (ZoneInfo memory) {
        return _zones[id];
    }

}

contract MockZonePortalForRouter {

    error DepositsRejected();

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
    uint256 public depositCount;
    bytes32 public currentDepositQueueHash;
    bool public rejectDeposits;

    function setRejectDeposits(bool reject) external {
        rejectDeposits = reject;
    }

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
        address tempoRefundRecipient
    )
        external
        returns (bytes32)
    {
        if (rejectDeposits) revert DepositsRejected();
        ITIP20(_token).transferFrom(msg.sender, address(this), amount);
        lastDepositRecipient = to;
        lastDepositBouncebackRecipient = tempoRefundRecipient;
        lastDepositAmount = amount;
        lastDepositMemo = memo;
        depositCalled = true;
        ++depositCount;
        currentDepositQueueHash = keccak256(
            abi.encode(
                currentDepositQueueHash, false, _token, to, amount, memo, tempoRefundRecipient
            )
        );
        return currentDepositQueueHash;
    }

    function depositEncrypted(
        address _token,
        uint128 amount,
        uint256 keyIndex,
        EncryptedDepositPayload calldata,
        address tempoRefundRecipient
    )
        external
        returns (bytes32)
    {
        if (rejectDeposits) revert DepositsRejected();
        ITIP20(_token).transferFrom(msg.sender, address(this), amount);
        lastEncryptedAmount = amount;
        lastEncryptedKeyIndex = keyIndex;
        lastEncryptedBouncebackRecipient = tempoRefundRecipient;
        encryptedDepositCalled = true;
        ++depositCount;
        currentDepositQueueHash = keccak256(
            abi.encode(
                currentDepositQueueHash, true, _token, amount, keyIndex, tempoRefundRecipient
            )
        );
        return currentDepositQueueHash;
    }

}

contract SwapAndDepositRouterTest is BaseTest {

    SwapAndDepositRouter public router;
    MockZoneFactoryForRouter public mockFactory;
    MockZonePortalForRouter public mockPortal;
    MockZonePortalForRouter public mockPortal2;

    uint32 public constant SOURCE_ZONE_ID = 7;
    bytes32 public senderTag = keccak256(abi.encodePacked(address(0x500)));
    address public sourcePortal = address(0x501);
    address public refundBurner = address(0xb000000000000000000000000000000000000123);
    uint128 public constant AMOUNT = 1000e6;
    uint256 internal constant FRAGMENTED_ORDERS = 2;
    uint128[] internal liquidityOrderIds;

    function setUp() public override {
        super.setUp();

        mockFactory = new MockZoneFactoryForRouter();
        mockPortal = new MockZonePortalForRouter();
        mockPortal2 = new MockZonePortalForRouter();

        router = new SwapAndDepositRouter(address(exchange), address(mockFactory));

        mockFactory.setSourcePortal(SOURCE_ZONE_ID, sourcePortal);
        mockFactory.setPortal(address(mockPortal), true);
        mockFactory.setPortal(address(mockPortal2), true);

        mockPortal.enableToken(address(pathUSD));
        mockPortal2.enableToken(address(token1));

        vm.startPrank(pathUSDAdmin);
        pathUSD.grantRole(_ISSUER_ROLE, pathUSDAdmin);
        pathUSD.mint(address(router), AMOUNT * 10);
        vm.stopPrank();

        vm.prank(sequencer);
        token1.grantRole(_ISSUER_ROLE, admin);
        _seedFragmentedLiquidity();
    }

    function _seedFragmentedLiquidity() internal {
        uint128 perOrder = exchange.MIN_ORDER_AMOUNT() * 5 + 3;
        exchange.createPair(address(token1));
        vm.prank(admin);
        token1.mint(bob, uint256(perOrder) * FRAGMENTED_ORDERS * 2);
        vm.prank(bob);
        token1.approve(address(exchange), type(uint256).max);
        int16 spacing = exchange.TICK_SPACING();
        for (uint256 i; i < FRAGMENTED_ORDERS; ++i) {
            int16 tick = int16(int256(i + 1)) * spacing;
            vm.prank(bob);
            liquidityOrderIds.push(exchange.place(address(token1), perOrder, false, tick));
            vm.prank(bob);
            liquidityOrderIds.push(exchange.place(address(token1), perOrder, false, tick));
        }
    }

    function _dexStateHash() internal view returns (bytes32 stateHash) {
        bytes32 key = exchange.pairKey(address(token1), address(pathUSD));
        (address base, address quote, int16 bestBid, int16 bestAsk) = exchange.books(key);
        stateHash = keccak256(
            abi.encode(
                exchange.nextOrderId(),
                exchange.balanceOf(bob, address(pathUSD)),
                exchange.balanceOf(bob, address(token1)),
                base,
                quote,
                bestBid,
                bestAsk
            )
        );
        int16 spacing = exchange.TICK_SPACING();
        for (uint256 i; i < liquidityOrderIds.length; ++i) {
            int16 tick = int16(int256(i / 2 + 1)) * spacing;
            (uint128 head, uint128 tail, uint128 liquidity) =
                exchange.getTickLevel(address(token1), tick, false);
            stateHash = keccak256(
                abi.encode(
                    stateHash, exchange.getOrder(liquidityOrderIds[i]), head, tail, liquidity
                )
            );
        }
    }

    function _buildPlaintextData(
        address tokenOut,
        address targetPortal,
        address recipient,
        address tempoRefundRecipient,
        bytes32 memo,
        uint128 minAmountOut
    )
        internal
        pure
        returns (bytes memory)
    {
        return abi.encode(
            false, tokenOut, targetPortal, recipient, tempoRefundRecipient, memo, minAmountOut
        );
    }

    function _buildEncryptedData(
        address tokenOut,
        address targetPortal,
        uint256 keyIndex,
        EncryptedDepositPayload memory encrypted,
        address tempoRefundRecipient,
        uint128 minAmountOut
    )
        internal
        pure
        returns (bytes memory)
    {
        return abi.encode(
            true, tokenOut, targetPortal, keyIndex, encrypted, tempoRefundRecipient, minAmountOut
        );
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
        bytes memory data = _buildPlaintextData(
            address(pathUSD), address(mockPortal), alice, refundBurner, bytes32("memo"), 0
        );

        vm.prank(alice);
        vm.expectRevert(SwapAndDepositRouter.UnauthorizedMessenger.selector);
        router.onWithdrawalReceived(
            SOURCE_ZONE_ID, sourcePortal, senderTag, address(pathUSD), AMOUNT, data
        );
    }

    function test_revertInvalidSourcePortal() public {
        bytes memory data = _buildPlaintextData(
            address(pathUSD), address(mockPortal), alice, refundBurner, bytes32("memo"), 0
        );

        vm.prank(ZONE_MESSENGER_ADDRESS);
        vm.expectRevert(SwapAndDepositRouter.InvalidSourcePortal.selector);
        router.onWithdrawalReceived(
            SOURCE_ZONE_ID, address(0xBAD), senderTag, address(pathUSD), AMOUNT, data
        );
    }

    function test_revertInvalidTargetPortal() public {
        address fakePortal = address(0xFAFAFA);
        bytes memory data = _buildPlaintextData(
            address(pathUSD), fakePortal, alice, refundBurner, bytes32("memo"), 0
        );

        vm.prank(ZONE_MESSENGER_ADDRESS);
        vm.expectRevert(SwapAndDepositRouter.InvalidTargetPortal.selector);
        router.onWithdrawalReceived(
            SOURCE_ZONE_ID, sourcePortal, senderTag, address(pathUSD), AMOUNT, data
        );
    }

    function test_revertInvalidToken() public {
        bytes memory data = _buildPlaintextData(
            address(token1), address(mockPortal), alice, refundBurner, bytes32("memo"), 0
        );

        vm.prank(ZONE_MESSENGER_ADDRESS);
        vm.expectRevert(SwapAndDepositRouter.InvalidToken.selector);
        router.onWithdrawalReceived(
            SOURCE_ZONE_ID, sourcePortal, senderTag, address(pathUSD), AMOUNT, data
        );
    }

    function test_plaintextDeposit_sameToken() public {
        bytes memory data = _buildPlaintextData(
            address(pathUSD), address(mockPortal), alice, refundBurner, bytes32("hello"), 0
        );

        vm.prank(ZONE_MESSENGER_ADDRESS);
        bytes4 ret = router.onWithdrawalReceived(
            SOURCE_ZONE_ID, sourcePortal, senderTag, address(pathUSD), AMOUNT, data
        );

        assertEq(ret, IWithdrawalReceiver.onWithdrawalReceived.selector);
        assertTrue(mockPortal.depositCalled());
        assertEq(mockPortal.lastDepositRecipient(), alice);
        assertEq(mockPortal.lastDepositBouncebackRecipient(), refundBurner);
        assertEq(mockPortal.lastDepositAmount(), AMOUNT);
        assertEq(mockPortal.lastDepositMemo(), bytes32("hello"));
    }

    function test_plaintextDeposit_withSwap() public {
        bytes memory data = _buildPlaintextData(
            address(token1), address(mockPortal2), alice, refundBurner, bytes32("swap"), 0
        );

        vm.prank(ZONE_MESSENGER_ADDRESS);
        bytes4 ret = router.onWithdrawalReceived(
            SOURCE_ZONE_ID, sourcePortal, senderTag, address(pathUSD), AMOUNT, data
        );

        assertEq(ret, IWithdrawalReceiver.onWithdrawalReceived.selector);
        assertTrue(mockPortal2.depositCalled());
        assertEq(mockPortal2.lastDepositRecipient(), alice);
        assertEq(mockPortal2.lastDepositBouncebackRecipient(), refundBurner);
        assertGt(mockPortal2.lastDepositAmount(), 0);
        assertEq(mockPortal2.lastDepositMemo(), bytes32("swap"));
    }

    function test_encryptedDeposit_sameToken() public {
        EncryptedDepositPayload memory payload = _defaultEncryptedPayload();
        bytes memory data =
            _buildEncryptedData(address(pathUSD), address(mockPortal), 0, payload, refundBurner, 0);

        vm.prank(ZONE_MESSENGER_ADDRESS);
        bytes4 ret = router.onWithdrawalReceived(
            SOURCE_ZONE_ID, sourcePortal, senderTag, address(pathUSD), AMOUNT, data
        );

        assertEq(ret, IWithdrawalReceiver.onWithdrawalReceived.selector);
        assertTrue(mockPortal.encryptedDepositCalled());
        assertEq(mockPortal.lastEncryptedAmount(), AMOUNT);
        assertEq(mockPortal.lastEncryptedKeyIndex(), 0);
        assertEq(mockPortal.lastEncryptedBouncebackRecipient(), refundBurner);
    }

    function test_encryptedDeposit_withSwap() public {
        EncryptedDepositPayload memory payload = _defaultEncryptedPayload();
        bytes memory data =
            _buildEncryptedData(address(token1), address(mockPortal2), 1, payload, refundBurner, 0);

        vm.prank(ZONE_MESSENGER_ADDRESS);
        bytes4 ret = router.onWithdrawalReceived(
            SOURCE_ZONE_ID, sourcePortal, senderTag, address(pathUSD), AMOUNT, data
        );

        assertEq(ret, IWithdrawalReceiver.onWithdrawalReceived.selector);
        assertTrue(mockPortal2.encryptedDepositCalled());
        assertGt(mockPortal2.lastEncryptedAmount(), 0);
        assertEq(mockPortal2.lastEncryptedKeyIndex(), 1);
        assertEq(mockPortal2.lastEncryptedBouncebackRecipient(), refundBurner);
    }

    function test_swapSlippageReverts() public {
        uint128 quote = exchange.quoteSwapExactAmountIn(address(pathUSD), address(token1), AMOUNT);

        bytes memory data = _buildPlaintextData(
            address(token1), address(mockPortal2), alice, refundBurner, bytes32("slip"), quote + 1
        );

        vm.prank(ZONE_MESSENGER_ADDRESS);
        vm.expectRevert(IStablecoinDEX.InsufficientOutput.selector);
        router.onWithdrawalReceived(
            SOURCE_ZONE_ID, sourcePortal, senderTag, address(pathUSD), AMOUNT, data
        );
    }

    /// @dev TODO: Enable once https://github.com/tempoxyz/tempo/pull/6614 is included in Forge.
    function test_swapOutputMatchesQuote() public {
        vm.skip(true, "Tempo #6614: exact-input quotes still round per tick instead of per order");

        uint128 quote = exchange.quoteSwapExactAmountIn(address(pathUSD), address(token1), AMOUNT);
        bytes memory data = _buildPlaintextData(
            address(token1),
            address(mockPortal2),
            alice,
            refundBurner,
            bytes32("quote parity"),
            quote
        );

        vm.prank(ZONE_MESSENGER_ADDRESS);
        router.onWithdrawalReceived(
            SOURCE_ZONE_ID, sourcePortal, senderTag, address(pathUSD), AMOUNT, data
        );

        assertEq(mockPortal2.lastDepositAmount(), quote);
    }

    function testFuzz_swapDeposit_conservesBalancesAndQueuesDeposit(
        bool encrypted,
        bytes32 metadata,
        uint256 keyIndex
    )
        public
    {
        uint256 routerInBefore = pathUSD.balanceOf(address(router));
        uint256 dexInBefore = pathUSD.balanceOf(address(exchange));
        uint256 dexOutBefore = token1.balanceOf(address(exchange));
        uint256 portalOutBefore = token1.balanceOf(address(mockPortal2));
        bytes32 hashBefore = mockPortal2.currentDepositQueueHash();

        bytes memory data = encrypted
            ? _buildEncryptedData(
                address(token1),
                address(mockPortal2),
                keyIndex,
                _defaultEncryptedPayload(),
                refundBurner,
                0
            )
            : _buildPlaintextData(
                address(token1), address(mockPortal2), alice, refundBurner, metadata, 0
            );
        vm.prank(ZONE_MESSENGER_ADDRESS);
        bytes4 result = router.onWithdrawalReceived(
            SOURCE_ZONE_ID, sourcePortal, senderTag, address(pathUSD), AMOUNT, data
        );
        uint128 amountOut =
            encrypted ? mockPortal2.lastEncryptedAmount() : mockPortal2.lastDepositAmount();

        assertEq(result, IWithdrawalReceiver.onWithdrawalReceived.selector);
        assertGt(amountOut, 0);
        assertEq(pathUSD.balanceOf(address(router)), routerInBefore - AMOUNT);
        assertEq(pathUSD.balanceOf(address(exchange)), dexInBefore + AMOUNT);
        assertEq(token1.balanceOf(address(exchange)), dexOutBefore - amountOut);
        assertEq(token1.balanceOf(address(mockPortal2)), portalOutBefore + amountOut);
        assertEq(token1.balanceOf(address(router)), 0);
        assertEq(mockPortal2.depositCount(), 1);
        assertNotEq(mockPortal2.currentDepositQueueHash(), hashBefore);
    }

    function testFuzz_swapDeposit_failureIsAtomic(bool portalRejects, bool encrypted) public {
        uint128 quote = exchange.quoteSwapExactAmountIn(address(pathUSD), address(token1), AMOUNT);
        mockPortal2.setRejectDeposits(portalRejects);
        uint128 minAmountOut = portalRejects ? 0 : quote + 1;
        bytes memory data = encrypted
            ? _buildEncryptedData(
                address(token1),
                address(mockPortal2),
                17,
                _defaultEncryptedPayload(),
                refundBurner,
                minAmountOut
            )
            : _buildPlaintextData(
                address(token1),
                address(mockPortal2),
                alice,
                refundBurner,
                bytes32("rollback"),
                minAmountOut
            );
        uint256[4] memory balances = [
            pathUSD.balanceOf(address(router)),
            pathUSD.balanceOf(address(exchange)),
            token1.balanceOf(address(exchange)),
            token1.balanceOf(address(mockPortal2))
        ];
        bytes32 hashBefore = mockPortal2.currentDepositQueueHash();
        bytes32 dexStateBefore = _dexStateHash();

        vm.prank(ZONE_MESSENGER_ADDRESS);
        vm.expectRevert(
            portalRejects
                ? MockZonePortalForRouter.DepositsRejected.selector
                : IStablecoinDEX.InsufficientOutput.selector
        );
        router.onWithdrawalReceived(
            SOURCE_ZONE_ID, sourcePortal, senderTag, address(pathUSD), AMOUNT, data
        );

        assertEq(pathUSD.balanceOf(address(router)), balances[0]);
        assertEq(pathUSD.balanceOf(address(exchange)), balances[1]);
        assertEq(token1.balanceOf(address(exchange)), balances[2]);
        assertEq(token1.balanceOf(address(mockPortal2)), balances[3]);
        assertEq(token1.balanceOf(address(router)), 0);
        assertEq(mockPortal2.depositCount(), 0);
        assertEq(mockPortal2.currentDepositQueueHash(), hashBefore);
        assertEq(_dexStateHash(), dexStateBefore);
    }

}
