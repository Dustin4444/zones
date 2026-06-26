// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import { BlockTransition, DepositQueueTransition, IVerifier } from "./IZone.sol";

/// @notice Local/non-Nitro verifier for end-to-end prover testing.
/// @dev This verifier is intentionally not an MPT verifier. It checks that a
/// prover-controlled native backend signed the exact public inputs and claimed
/// outputs that `ZonePortal.submitBatch` is about to apply. Nitro attestation
/// verification should be implemented by a separate verifier with the same
/// `IVerifier` interface.
contract NativeSignatureVerifier is IVerifier {

    uint16 public constant PROTOCOL_VERSION = 1;
    bytes32 public constant DIGEST_DOMAIN = keccak256("tempo.zone.native.verifier.batch.v1");
    bytes32 public constant CONFIG_DOMAIN = keccak256("tempo.zone.native.verifier.config.v1");
    uint256 private constant SECP256K1_HALF_N =
        0x7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a0;

    struct Policy {
        address signer;
        uint64 verifierVersion;
        bool enabled;
    }

    struct NativeVerifierConfig {
        uint16 version;
        uint64 chainId;
        address portal;
        uint64 verifierVersion;
    }

    struct NativeProof {
        bytes32 digest;
        bytes signature;
    }

    address public immutable verifierAdmin;
    mapping(address portal => Policy) public policies;

    event PortalPolicyRegistered(
        address indexed portal, address indexed signer, uint64 verifierVersion
    );
    event PortalPolicyDisabled(address indexed portal);

    error OnlyVerifierAdmin();
    error InvalidVerifierAdmin();
    error InvalidPortal();
    error InvalidSigner();
    error InvalidVerifierVersion();

    constructor(address admin) {
        if (admin == address(0)) revert InvalidVerifierAdmin();
        verifierAdmin = admin;
    }

    modifier onlyVerifierAdmin() {
        if (msg.sender != verifierAdmin) revert OnlyVerifierAdmin();
        _;
    }

    function registerPortal(
        address portal,
        address signer,
        uint64 verifierVersion
    )
        external
        onlyVerifierAdmin
    {
        if (portal == address(0)) revert InvalidPortal();
        if (signer == address(0)) revert InvalidSigner();
        if (verifierVersion == 0) revert InvalidVerifierVersion();
        policies[portal] =
            Policy({ signer: signer, verifierVersion: verifierVersion, enabled: true });
        emit PortalPolicyRegistered(portal, signer, verifierVersion);
    }

    function disablePortal(address portal) external onlyVerifierAdmin {
        if (portal == address(0)) revert InvalidPortal();
        delete policies[portal];
        emit PortalPolicyDisabled(portal);
    }

    function verify(
        uint64 tempoBlockNumber,
        uint64 anchorBlockNumber,
        bytes32 anchorBlockHash,
        uint64 expectedWithdrawalBatchIndex,
        address sequencer,
        BlockTransition calldata blockTransition,
        DepositQueueTransition calldata depositQueueTransition,
        bytes32 withdrawalQueueHash,
        bytes calldata verifierConfig,
        bytes calldata proof
    )
        external
        view
        override
        returns (bool)
    {
        if (verifierConfig.length == 0 || proof.length == 0) {
            return false;
        }

        NativeVerifierConfig memory config = abi.decode(verifierConfig, (NativeVerifierConfig));
        Policy memory policy = policies[msg.sender];
        if (!_policyMatches(config, policy, msg.sender)) {
            return false;
        }

        NativeProof memory nativeProof = abi.decode(proof, (NativeProof));
        bytes32 digest = _computeDigest(
            tempoBlockNumber,
            anchorBlockNumber,
            anchorBlockHash,
            expectedWithdrawalBatchIndex,
            sequencer,
            blockTransition,
            depositQueueTransition,
            withdrawalQueueHash,
            config,
            policy
        );
        if (nativeProof.digest != digest) {
            return false;
        }

        return _recover(digest, nativeProof.signature) == policy.signer;
    }

    function computeDigest(
        uint64 tempoBlockNumber,
        uint64 anchorBlockNumber,
        bytes32 anchorBlockHash,
        uint64 expectedWithdrawalBatchIndex,
        address sequencer,
        BlockTransition calldata blockTransition,
        DepositQueueTransition calldata depositQueueTransition,
        bytes32 withdrawalQueueHash,
        bytes calldata verifierConfig
    )
        external
        view
        returns (bytes32)
    {
        NativeVerifierConfig memory config = abi.decode(verifierConfig, (NativeVerifierConfig));
        Policy memory policy = policies[config.portal];
        if (!_policyMatches(config, policy, config.portal)) {
            return bytes32(0);
        }
        return _computeDigest(
            tempoBlockNumber,
            anchorBlockNumber,
            anchorBlockHash,
            expectedWithdrawalBatchIndex,
            sequencer,
            blockTransition,
            depositQueueTransition,
            withdrawalQueueHash,
            config,
            policy
        );
    }

    function _policyMatches(
        NativeVerifierConfig memory config,
        Policy memory policy,
        address portal
    )
        private
        view
        returns (bool)
    {
        return config.version == PROTOCOL_VERSION && config.chainId == block.chainid
            && config.portal == portal && policy.enabled && policy.signer != address(0)
            && policy.verifierVersion == config.verifierVersion;
    }

    function _computeDigest(
        uint64 tempoBlockNumber,
        uint64 anchorBlockNumber,
        bytes32 anchorBlockHash,
        uint64 expectedWithdrawalBatchIndex,
        address sequencer,
        BlockTransition calldata blockTransition,
        DepositQueueTransition calldata depositQueueTransition,
        bytes32 withdrawalQueueHash,
        NativeVerifierConfig memory config,
        Policy memory policy
    )
        private
        pure
        returns (bytes32)
    {
        bytes32 configDigest = keccak256(
            abi.encode(
                CONFIG_DOMAIN, config.version, config.chainId, config.portal, config.verifierVersion
            )
        );
        bytes32 outputDigest = keccak256(
            abi.encode(
                blockTransition.prevBlockHash,
                blockTransition.nextBlockHash,
                depositQueueTransition.prevProcessedHash,
                depositQueueTransition.nextProcessedHash,
                depositQueueTransition.prevDepositNumber,
                depositQueueTransition.nextDepositNumber,
                withdrawalQueueHash,
                withdrawalQueueHash,
                expectedWithdrawalBatchIndex
            )
        );
        return keccak256(
            abi.encode(
                DIGEST_DOMAIN,
                configDigest,
                policy.signer,
                tempoBlockNumber,
                anchorBlockNumber,
                anchorBlockHash,
                sequencer,
                outputDigest
            )
        );
    }

    function _recover(bytes32 digest, bytes memory signature) private pure returns (address) {
        if (signature.length != 65) {
            return address(0);
        }

        bytes32 r;
        bytes32 s;
        uint8 v;
        assembly {
            r := mload(add(signature, 0x20))
            s := mload(add(signature, 0x40))
            v := byte(0, mload(add(signature, 0x60)))
        }

        if (v < 27) {
            v += 27;
        }
        if (v != 27 && v != 28) {
            return address(0);
        }
        if (uint256(s) > SECP256K1_HALF_N) {
            return address(0);
        }

        return ecrecover(digest, v, r, s);
    }

}
