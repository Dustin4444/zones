// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

import { ITIP20, ITIP20Token } from "tempo-std/interfaces/ITIP20.sol";
import { ITIP20Factory } from "tempo-std/interfaces/ITIP20Factory.sol";
import { ITIP403Registry } from "tempo-std/interfaces/ITIP403Registry.sol";

contract MockTempoTIP20 is ITIP20Token {

    bytes32 public constant DEFAULT_ADMIN_ROLE = bytes32(0);
    bytes32 public constant BURN_BLOCKED_ROLE = keccak256("BURN_BLOCKED_ROLE");
    bytes32 public constant ISSUER_ROLE = keccak256("ISSUER_ROLE");
    bytes32 public constant PAUSE_ROLE = keccak256("PAUSE_ROLE");
    bytes32 public constant UNPAUSE_ROLE = keccak256("UNPAUSE_ROLE");

    string public name;
    string public symbol;
    string public currency;
    uint8 public constant decimals = 6;

    ITIP20 public quoteToken;
    ITIP20 public nextQuoteToken;
    uint256 public totalSupply;
    uint256 public supplyCap;
    uint64 public transferPolicyId;
    bool public paused;

    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    mapping(address => uint256) public nonces;
    mapping(address => RewardInfo) internal _rewardInfo;
    mapping(bytes32 => bytes32) internal _roleAdmins;
    mapping(bytes32 => mapping(address => bool)) internal _roles;

    bool internal _initialized;

    struct RewardInfo {
        address rewardRecipient;
        uint256 rewardPerToken;
        uint256 rewardBalance;
    }

    function initialize(
        string memory _name,
        string memory _symbol,
        string memory _currency,
        ITIP20 _quoteToken,
        address admin
    )
        external
    {
        if (_initialized) return;

        name = _name;
        symbol = _symbol;
        currency = _currency;
        quoteToken = _quoteToken;
        transferPolicyId = 1;
        _roles[DEFAULT_ADMIN_ROLE][admin] = true;
        _roleAdmins[DEFAULT_ADMIN_ROLE] = DEFAULT_ADMIN_ROLE;
        _roleAdmins[BURN_BLOCKED_ROLE] = DEFAULT_ADMIN_ROLE;
        _roleAdmins[ISSUER_ROLE] = DEFAULT_ADMIN_ROLE;
        _roleAdmins[PAUSE_ROLE] = DEFAULT_ADMIN_ROLE;
        _roleAdmins[UNPAUSE_ROLE] = DEFAULT_ADMIN_ROLE;
        _initialized = true;
    }

    function hasRole(address account, bytes32 role) external view returns (bool) {
        return _roles[role][account];
    }

    function getRoleAdmin(bytes32 role) external view returns (bytes32) {
        return _roleAdmins[role];
    }

    function grantRole(bytes32 role, address account) external {
        _roles[role][account] = true;
        emit RoleMembershipUpdated(role, account, msg.sender, true);
    }

    function revokeRole(bytes32 role, address account) external {
        _roles[role][account] = false;
        emit RoleMembershipUpdated(role, account, msg.sender, false);
    }

    function renounceRole(bytes32 role) external {
        _roles[role][msg.sender] = false;
        emit RoleMembershipUpdated(role, msg.sender, msg.sender, false);
    }

    function setRoleAdmin(bytes32 role, bytes32 adminRole) external {
        _roleAdmins[role] = adminRole;
        emit RoleAdminUpdated(role, adminRole, msg.sender);
    }

    function approve(address spender, uint256 amount) public returns (bool) {
        allowance[msg.sender][spender] = amount;
        emit Approval(msg.sender, spender, amount);
        return true;
    }

    function transfer(address to, uint256 amount) public returns (bool) {
        _transfer(msg.sender, to, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) public returns (bool) {
        uint256 allowed = allowance[from][msg.sender];
        if (allowed < amount) revert InsufficientAllowance();
        if (allowed != type(uint256).max) {
            allowance[from][msg.sender] = allowed - amount;
            emit Approval(from, msg.sender, allowed - amount);
        }
        _transfer(from, to, amount);
        return true;
    }

    function transferWithMemo(address to, uint256 amount, bytes32 memo) external {
        _transfer(msg.sender, to, amount);
        emit TransferWithMemo(msg.sender, to, amount, memo);
    }

    function transferFromWithMemo(
        address from,
        address to,
        uint256 amount,
        bytes32 memo
    )
        external
        returns (bool)
    {
        transferFrom(from, to, amount);
        emit TransferWithMemo(from, to, amount, memo);
        return true;
    }

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }

    function mintWithMemo(address to, uint256 amount, bytes32 memo) external {
        _mint(to, amount);
        emit TransferWithMemo(address(0), to, amount, memo);
    }

    function burn(uint256 amount) external {
        _burn(msg.sender, amount);
    }

    function burnWithMemo(uint256 amount, bytes32) external {
        _burn(msg.sender, amount);
    }

    function burnBlocked(address from, uint256 amount) external {
        _burn(from, amount);
        emit BurnBlocked(from, amount);
    }

    function pause() external {
        paused = true;
        emit PauseStateUpdate(msg.sender, true);
    }

    function unpause() external {
        paused = false;
        emit PauseStateUpdate(msg.sender, false);
    }

    function changeTransferPolicyId(uint64 newPolicyId) external {
        transferPolicyId = newPolicyId;
        emit TransferPolicyUpdate(msg.sender, newPolicyId);
    }

    function setSupplyCap(uint256 newSupplyCap) external {
        supplyCap = newSupplyCap;
        emit SupplyCapUpdate(msg.sender, newSupplyCap);
    }

    function setNextQuoteToken(ITIP20 newQuoteToken) external {
        nextQuoteToken = newQuoteToken;
        emit NextQuoteTokenSet(msg.sender, newQuoteToken);
    }

    function completeQuoteTokenUpdate() external {
        quoteToken = nextQuoteToken;
        delete nextQuoteToken;
        emit QuoteTokenUpdate(msg.sender, quoteToken);
    }

    function setRewardRecipient(address newRewardRecipient) external {
        _rewardInfo[msg.sender].rewardRecipient = newRewardRecipient;
        emit RewardRecipientSet(msg.sender, newRewardRecipient);
    }

    function distributeReward(uint256 amount) external {
        transferFrom(msg.sender, address(this), amount);
        emit RewardDistributed(msg.sender, amount);
    }

    function claimRewards() external pure returns (uint256 maxAmount) {
        return 0;
    }

    function globalRewardPerToken() external pure returns (uint256) {
        return 0;
    }

    function optedInSupply() external view returns (uint128) {
        uint256 supply = totalSupply;
        return supply > type(uint128).max ? type(uint128).max : uint128(supply);
    }

    function userRewardInfo(address account)
        external
        view
        returns (address rewardRecipient, uint256 rewardPerToken, uint256 rewardBalance)
    {
        RewardInfo storage info = _rewardInfo[account];
        return (info.rewardRecipient, info.rewardPerToken, info.rewardBalance);
    }

    function getPendingRewards(address) external pure returns (uint128) {
        return 0;
    }

    function permit(
        address owner,
        address spender,
        uint256 value,
        uint256,
        uint8,
        bytes32,
        bytes32
    )
        external
    {
        allowance[owner][spender] = value;
        nonces[owner]++;
        emit Approval(owner, spender, value);
    }

    function DOMAIN_SEPARATOR() external view returns (bytes32) {
        return keccak256(abi.encode(block.chainid, address(this)));
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

    function _mint(address to, uint256 amount) internal {
        if (paused) revert ContractPaused();
        if (to == address(0)) revert InvalidRecipient();
        if (supplyCap != 0 && totalSupply + amount > supplyCap) revert SupplyCapExceeded();
        totalSupply += amount;
        balanceOf[to] += amount;
        emit Transfer(address(0), to, amount);
        emit Mint(to, amount);
    }

    function _burn(address from, uint256 amount) internal {
        if (paused) revert ContractPaused();
        uint256 fromBalance = balanceOf[from];
        if (fromBalance < amount) revert InsufficientBalance(fromBalance, amount, from);
        balanceOf[from] = fromBalance - amount;
        totalSupply -= amount;
        emit Transfer(from, address(0), amount);
        emit Burn(from, amount);
    }

}

contract MockTempoTIP20Factory is ITIP20Factory {

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
        emit TokenCreated(token, name, symbol, currency, quoteToken, admin, salt);
    }

    function getTokenAddress(address sender, bytes32 salt) external pure returns (address) {
        bytes32 digest = keccak256(
            abi.encodePacked(
                bytes1(0xff), sender, salt, keccak256(type(MockTempoTIP20).creationCode)
            )
        );
        return address(uint160(uint256(digest)));
    }

}

contract MockTempoTIP403Registry is ITIP403Registry {

    uint64 public policyIdCounter;

    mapping(uint64 => PolicyData) internal _policyData;
    mapping(uint64 => bool) internal _policyExists;
    mapping(uint64 => mapping(address => bool)) internal _policyAccounts;
    mapping(uint64 => CompoundPolicy) internal _compoundPolicies;

    struct CompoundPolicy {
        uint64 senderPolicyId;
        uint64 recipientPolicyId;
        uint64 mintRecipientPolicyId;
    }

    function policyExists(uint64 policyId) external view returns (bool) {
        return _policyExists[policyId];
    }

    function policyData(uint64 policyId)
        external
        view
        returns (PolicyType policyType, address admin)
    {
        PolicyData storage data = _policyData[policyId];
        return (data.policyType, data.admin);
    }

    function createPolicy(
        address admin,
        PolicyType policyType
    )
        external
        returns (uint64 newPolicyId)
    {
        newPolicyId = _createPolicy(admin, policyType);
    }

    function createPolicyWithAccounts(
        address admin,
        PolicyType policyType,
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

    function setPolicyAdmin(uint64 policyId, address admin) external {
        if (!_policyExists[policyId]) revert PolicyNotFound();
        _policyData[policyId].admin = admin;
        emit PolicyAdminUpdated(policyId, msg.sender, admin);
    }

    function modifyPolicyWhitelist(uint64 policyId, address account, bool allowed) external {
        if (!_policyExists[policyId]) revert PolicyNotFound();
        if (_policyData[policyId].policyType != PolicyType.WHITELIST) {
            revert IncompatiblePolicyType();
        }
        _policyAccounts[policyId][account] = allowed;
        emit WhitelistUpdated(policyId, msg.sender, account, allowed);
    }

    function modifyPolicyBlacklist(uint64 policyId, address account, bool restricted) external {
        if (!_policyExists[policyId]) revert PolicyNotFound();
        if (_policyData[policyId].policyType != PolicyType.BLACKLIST) {
            revert IncompatiblePolicyType();
        }
        _policyAccounts[policyId][account] = restricted;
        emit BlacklistUpdated(policyId, msg.sender, account, restricted);
    }

    function isAuthorized(uint64 policyId, address user) public view returns (bool) {
        if (!_policyExists[policyId]) return true;

        PolicyType policyType = _policyData[policyId].policyType;
        if (policyType == PolicyType.WHITELIST) {
            return _policyAccounts[policyId][user];
        }
        if (policyType == PolicyType.BLACKLIST) {
            return !_policyAccounts[policyId][user];
        }

        return isAuthorizedSender(policyId, user) && isAuthorizedRecipient(policyId, user);
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
        _policyData[newPolicyId] =
            PolicyData({ policyType: PolicyType.COMPOUND, admin: msg.sender });
        _compoundPolicies[newPolicyId] = CompoundPolicy({
            senderPolicyId: senderPolicyId,
            recipientPolicyId: recipientPolicyId,
            mintRecipientPolicyId: mintRecipientPolicyId
        });

        emit PolicyCreated(newPolicyId, msg.sender, PolicyType.COMPOUND);
        emit CompoundPolicyCreated(
            newPolicyId, msg.sender, senderPolicyId, recipientPolicyId, mintRecipientPolicyId
        );
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

    function _createPolicy(
        address admin,
        PolicyType policyType
    )
        internal
        returns (uint64 newPolicyId)
    {
        if (policyType == PolicyType.COMPOUND) revert InvalidPolicyType();

        newPolicyId = ++policyIdCounter;
        _policyExists[newPolicyId] = true;
        _policyData[newPolicyId] = PolicyData({ policyType: policyType, admin: admin });

        emit PolicyCreated(newPolicyId, msg.sender, policyType);
    }

    function _isCompound(uint64 policyId) internal view returns (bool) {
        return _policyExists[policyId] && _policyData[policyId].policyType == PolicyType.COMPOUND;
    }

}

contract MockTempoNoopPrecompile { }
