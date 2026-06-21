// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import { ITIP20 } from "tempo-std/interfaces/ITIP20.sol";
import { ITIP403Registry } from "tempo-std/interfaces/ITIP403Registry.sol";

contract MockTempoTIP20 {

    string public name;
    string public symbol;
    string public currency;
    uint8 public constant decimals = 6;

    uint256 public totalSupply;
    uint64 public transferPolicyId = 1;
    bool public paused;

    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    mapping(bytes32 => mapping(address => bool)) public roles;

    bool internal _initialized;

    event Transfer(address indexed from, address indexed to, uint256 amount);
    event Approval(address indexed owner, address indexed spender, uint256 amount);

    error ContractPaused();
    error InsufficientAllowance();
    error InsufficientBalance(uint256 currentBalance, uint256 expectedBalance, address account);
    error InvalidRecipient();

    function initialize(
        string memory _name,
        string memory _symbol,
        string memory _currency,
        ITIP20,
        address admin
    )
        external
    {
        if (_initialized) return;

        name = _name;
        symbol = _symbol;
        currency = _currency;
        roles[bytes32(0)][admin] = true;
        _initialized = true;
    }

    function grantRole(bytes32 role, address account) external {
        roles[role][account] = true;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        emit Approval(msg.sender, spender, amount);
        return true;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        _transfer(msg.sender, to, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        uint256 allowed = allowance[from][msg.sender];
        if (allowed < amount) revert InsufficientAllowance();
        if (allowed != type(uint256).max) {
            allowance[from][msg.sender] = allowed - amount;
            emit Approval(from, msg.sender, allowed - amount);
        }
        _transfer(from, to, amount);
        return true;
    }

    function mint(address to, uint256 amount) external {
        if (paused) revert ContractPaused();
        if (to == address(0)) revert InvalidRecipient();
        totalSupply += amount;
        balanceOf[to] += amount;
        emit Transfer(address(0), to, amount);
    }

    function pause() external {
        paused = true;
    }

    function unpause() external {
        paused = false;
    }

    function changeTransferPolicyId(uint64 newPolicyId) external {
        transferPolicyId = newPolicyId;
    }

    function _transfer(address from, address to, uint256 amount) internal {
        if (paused) revert ContractPaused();
        if (to == address(0)) revert InvalidRecipient();
        uint256 fromBalance = balanceOf[from];
        if (fromBalance < amount) revert InsufficientBalance(fromBalance, amount, from);
        balanceOf[from] = fromBalance - amount;
        balanceOf[to] += amount;
        emit Transfer(from, to, amount);
    }

}

contract MockTempoTIP20Factory {

    mapping(address => bool) public isTIP20;

    function initialize(address pathUSD) external {
        isTIP20[pathUSD] = true;
    }

    function createToken(
        string memory name,
        string memory symbol,
        string memory currency,
        ITIP20 quoteToken,
        address admin,
        bytes32 salt
    )
        external
        returns (address token)
    {
        token = address(new MockTempoTIP20{ salt: salt }());
        MockTempoTIP20(token).initialize(name, symbol, currency, quoteToken, admin);
        isTIP20[token] = true;
    }

}

contract MockTempoTIP403Registry {

    uint64 public policyIdCounter;

    mapping(uint64 => ITIP403Registry.PolicyData) internal _policyData;
    mapping(uint64 => bool) internal _policyExists;
    mapping(uint64 => mapping(address => bool)) internal _policyAccounts;
    mapping(uint64 => CompoundPolicy) internal _compoundPolicies;

    struct CompoundPolicy {
        uint64 senderPolicyId;
        uint64 recipientPolicyId;
        uint64 mintRecipientPolicyId;
    }

    error InvalidPolicyType();
    error PolicyNotFound();
    error PolicyNotSimple();

    function policyExists(uint64 policyId) external view returns (bool) {
        return _policyExists[policyId];
    }

    function policyData(uint64 policyId)
        external
        view
        returns (ITIP403Registry.PolicyType policyType, address admin)
    {
        ITIP403Registry.PolicyData storage data = _policyData[policyId];
        return (data.policyType, data.admin);
    }

    function createPolicy(
        address admin,
        ITIP403Registry.PolicyType policyType
    )
        external
        returns (uint64 newPolicyId)
    {
        newPolicyId = _createPolicy(admin, policyType);
    }

    function createPolicyWithAccounts(
        address admin,
        ITIP403Registry.PolicyType policyType,
        address[] calldata accounts
    )
        external
        returns (uint64 newPolicyId)
    {
        newPolicyId = _createPolicy(admin, policyType);
        for (uint256 i = 0; i < accounts.length; i++) {
            _policyAccounts[newPolicyId][accounts[i]] = true;
        }
    }

    function createCompoundPolicy(
        uint64 senderPolicyId,
        uint64 recipientPolicyId,
        uint64 mintRecipientPolicyId
    )
        external
        returns (uint64 newPolicyId)
    {
        if (
            _isCompound(senderPolicyId) || _isCompound(recipientPolicyId)
                || _isCompound(mintRecipientPolicyId)
        ) {
            revert PolicyNotSimple();
        }

        newPolicyId = ++policyIdCounter;
        _policyExists[newPolicyId] = true;
        _policyData[newPolicyId] = ITIP403Registry.PolicyData({
            policyType: ITIP403Registry.PolicyType.COMPOUND, admin: msg.sender
        });
        _compoundPolicies[newPolicyId] = CompoundPolicy({
            senderPolicyId: senderPolicyId,
            recipientPolicyId: recipientPolicyId,
            mintRecipientPolicyId: mintRecipientPolicyId
        });
    }

    function isAuthorized(uint64 policyId, address user) public view returns (bool) {
        if (!_policyExists[policyId]) return true;

        ITIP403Registry.PolicyType policyType = _policyData[policyId].policyType;
        if (policyType == ITIP403Registry.PolicyType.WHITELIST) {
            return _policyAccounts[policyId][user];
        }
        if (policyType == ITIP403Registry.PolicyType.BLACKLIST) {
            return !_policyAccounts[policyId][user];
        }

        return isAuthorizedSender(policyId, user) && isAuthorizedRecipient(policyId, user);
    }

    function isAuthorizedSender(uint64 policyId, address user) public view returns (bool) {
        if (!_isCompound(policyId)) return isAuthorized(policyId, user);
        return isAuthorized(_compoundPolicies[policyId].senderPolicyId, user);
    }

    function isAuthorizedRecipient(uint64 policyId, address user) public view returns (bool) {
        if (!_isCompound(policyId)) return isAuthorized(policyId, user);
        return isAuthorized(_compoundPolicies[policyId].recipientPolicyId, user);
    }

    function isAuthorizedMintRecipient(uint64 policyId, address user) public view returns (bool) {
        if (!_isCompound(policyId)) return isAuthorized(policyId, user);
        return isAuthorized(_compoundPolicies[policyId].mintRecipientPolicyId, user);
    }

    function compoundPolicyData(uint64 policyId)
        external
        view
        returns (uint64 senderPolicyId, uint64 recipientPolicyId, uint64 mintRecipientPolicyId)
    {
        CompoundPolicy storage data = _compoundPolicies[policyId];
        return (data.senderPolicyId, data.recipientPolicyId, data.mintRecipientPolicyId);
    }

    function modifyPolicyWhitelist(uint64 policyId, address account, bool allowed) external {
        if (!_policyExists[policyId]) revert PolicyNotFound();
        _policyAccounts[policyId][account] = allowed;
    }

    function modifyPolicyBlacklist(uint64 policyId, address account, bool restricted) external {
        if (!_policyExists[policyId]) revert PolicyNotFound();
        _policyAccounts[policyId][account] = restricted;
    }

    function setPolicyAdmin(uint64 policyId, address admin) external {
        if (!_policyExists[policyId]) revert PolicyNotFound();
        _policyData[policyId].admin = admin;
    }

    function _createPolicy(
        address admin,
        ITIP403Registry.PolicyType policyType
    )
        internal
        returns (uint64 newPolicyId)
    {
        if (policyType == ITIP403Registry.PolicyType.COMPOUND) revert InvalidPolicyType();

        newPolicyId = ++policyIdCounter;
        _policyExists[newPolicyId] = true;
        _policyData[newPolicyId] =
            ITIP403Registry.PolicyData({ policyType: policyType, admin: admin });
    }

    function _isCompound(uint64 policyId) internal view returns (bool) {
        return _policyExists[policyId]
            && _policyData[policyId].policyType == ITIP403Registry.PolicyType.COMPOUND;
    }

}

contract MockTempoNoopPrecompile { }
