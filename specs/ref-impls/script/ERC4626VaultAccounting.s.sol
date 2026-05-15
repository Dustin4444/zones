// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import { Script } from "forge-std/Script.sol";

interface IERC20 {

    function totalSupply() external view returns (uint256);
    function balanceOf(address account) external view returns (uint256);
    function allowance(address owner, address spender) external view returns (uint256);
    function approve(address spender, uint256 amount) external returns (bool);
    function transfer(address to, uint256 amount) external returns (bool);
    function transferFrom(address from, address to, uint256 amount) external returns (bool);

}

contract WrappedEthAsset is IERC20 {

    string public constant name = "Wrapped ETH";
    string public constant symbol = "WETH";
    uint8 public constant decimals = 18;

    uint256 public override totalSupply;
    mapping(address => uint256) public override balanceOf;
    mapping(address => mapping(address => uint256)) public override allowance;

    function deposit() external payable {
        balanceOf[msg.sender] += msg.value;
        totalSupply += msg.value;
    }

    function approve(address spender, uint256 amount) external override returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function transfer(address to, uint256 amount) external override returns (bool) {
        _transfer(msg.sender, to, amount);
        return true;
    }

    function transferFrom(
        address from,
        address to,
        uint256 amount
    )
        external
        override
        returns (bool)
    {
        uint256 allowed = allowance[from][msg.sender];
        require(allowed >= amount, "ALLOWANCE");
        if (allowed != type(uint256).max) {
            allowance[from][msg.sender] = allowed - amount;
        }
        _transfer(from, to, amount);
        return true;
    }

    function _transfer(address from, address to, uint256 amount) internal {
        require(balanceOf[from] >= amount, "BALANCE");
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
    }

    }

    contract Eth4626Vault is IERC20 {

        string public constant name = "ETH ERC4626 Vault";
        string public constant symbol = "vETH";
        uint8 public constant decimals = 18;

        IERC20 public immutable asset;
        uint256 public override totalSupply;
        mapping(address => uint256) public override balanceOf;
        mapping(address => mapping(address => uint256)) public override allowance;

        constructor(IERC20 asset_) {
            asset = asset_;
        }

        function totalAssets() public view returns (uint256) {
            return asset.balanceOf(address(this));
        }

        function convertToShares(uint256 assets) public view returns (uint256) {
            uint256 supply = totalSupply;
            return supply == 0 ? assets : (assets * supply) / totalAssets();
        }

        function convertToAssets(uint256 shares) public view returns (uint256) {
            uint256 supply = totalSupply;
            return supply == 0 ? shares : (shares * totalAssets()) / supply;
        }

        function previewDeposit(uint256 assets) external view returns (uint256) {
            return convertToShares(assets);
        }

        function deposit(uint256 assets, address receiver) external returns (uint256 shares) {
            shares = convertToShares(assets);
            require(shares != 0, "ZERO_SHARES");
            require(asset.transferFrom(msg.sender, address(this), assets), "TRANSFER_FROM");
            totalSupply += shares;
            balanceOf[receiver] += shares;
        }

        function approve(address spender, uint256 amount) external override returns (bool) {
            allowance[msg.sender][spender] = amount;
            return true;
        }

        function transfer(address to, uint256 amount) external override returns (bool) {
            _transfer(msg.sender, to, amount);
            return true;
        }

        function transferFrom(
            address from,
            address to,
            uint256 amount
        )
            external
            override
            returns (bool)
        {
            uint256 allowed = allowance[from][msg.sender];
            require(allowed >= amount, "ALLOWANCE");
            if (allowed != type(uint256).max) {
                allowance[from][msg.sender] = allowed - amount;
            }
            _transfer(from, to, amount);
            return true;
        }

        function _transfer(address from, address to, uint256 amount) internal {
            require(balanceOf[from] >= amount, "BALANCE");
            balanceOf[from] -= amount;
            balanceOf[to] += amount;
        }

        }

        contract ERC4626VaultAccountingScript is Script {

            function run() external payable {
                uint256 deployerKey = vm.envOr("PRIVATE_KEY", uint256(0));
                address deployer = deployerKey == 0 ? msg.sender : vm.addr(deployerKey);
                uint256 depositAmount = vm.envOr("DEPOSIT_AMOUNT", uint256(10 ether));

                if (deployerKey == 0) {
                    vm.startBroadcast();
                } else {
                    vm.startBroadcast(deployerKey);
                }

                WrappedEthAsset weth = new WrappedEthAsset();
                Eth4626Vault vault = new Eth4626Vault(IERC20(address(weth)));

                weth.deposit{ value: depositAmount }();
                require(weth.balanceOf(deployer) == depositAmount, "ETH_NOT_WRAPPED");

                uint256 expectedShares = vault.previewDeposit(depositAmount);
                require(weth.approve(address(vault), depositAmount), "APPROVE_FAILED");
                uint256 shares = vault.deposit(depositAmount, deployer);

                require(shares == expectedShares, "SHARE_PREVIEW_MISMATCH");
                require(vault.balanceOf(deployer) == shares, "SHARE_BALANCE_MISMATCH");
                require(vault.totalAssets() == depositAmount, "TOTAL_ASSETS_MISMATCH");
                require(vault.convertToAssets(shares) == depositAmount, "ASSET_ACCOUNTING_MISMATCH");

                vm.stopBroadcast();
            }

        }
