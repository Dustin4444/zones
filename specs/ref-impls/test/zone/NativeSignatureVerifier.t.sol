// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import { BlockTransition, DepositQueueTransition } from "../../src/zone/IZone.sol";
import { NativeSignatureVerifier } from "../../src/zone/NativeSignatureVerifier.sol";
import { Test } from "forge-std/Test.sol";

contract NativeSignatureVerifierTest is Test {

    uint256 private constant SIGNER_KEY = 0xA11CE;
    uint256 private constant WRONG_SIGNER_KEY = 0xB0B;
    uint64 private constant VERIFIER_VERSION = 1;

    NativeSignatureVerifier private verifier;
    address private signer;
    address private wrongSigner;
    address private portal = address(0x1234);
    address private sequencer = address(0x5678);

    function setUp() public {
        verifier = new NativeSignatureVerifier(address(this));
        signer = vm.addr(SIGNER_KEY);
        wrongSigner = vm.addr(WRONG_SIGNER_KEY);
        verifier.registerPortal(portal, signer, VERIFIER_VERSION);
    }

    function test_verify_acceptsRegisteredSignerForExactBatchInputs() public {
        Batch memory batch = _batch();
        bytes memory config = _config(portal);
        bytes memory proof = _proof(SIGNER_KEY, batch, config);

        vm.prank(portal);
        bool valid = verifier.verify(
            batch.tempoBlockNumber,
            batch.anchorBlockNumber,
            batch.anchorBlockHash,
            batch.expectedWithdrawalBatchIndex,
            sequencer,
            batch.blockTransition,
            batch.depositQueueTransition,
            batch.withdrawalQueueHash,
            config,
            proof
        );

        assertTrue(valid);
    }

    function test_verify_rejectsTamperedBlockTransition() public {
        Batch memory batch = _batch();
        bytes memory config = _config(portal);
        bytes memory proof = _proof(SIGNER_KEY, batch, config);
        batch.blockTransition.nextBlockHash = keccak256("tampered-next-block");

        vm.prank(portal);
        bool valid = verifier.verify(
            batch.tempoBlockNumber,
            batch.anchorBlockNumber,
            batch.anchorBlockHash,
            batch.expectedWithdrawalBatchIndex,
            sequencer,
            batch.blockTransition,
            batch.depositQueueTransition,
            batch.withdrawalQueueHash,
            config,
            proof
        );

        assertFalse(valid);
    }

    function test_verify_rejectsUnregisteredPortal() public {
        Batch memory batch = _batch();
        address unregisteredPortal = address(0x9999);
        bytes memory config = _config(unregisteredPortal);
        bytes memory proof = _proof(SIGNER_KEY, batch, config);

        vm.prank(unregisteredPortal);
        bool valid = verifier.verify(
            batch.tempoBlockNumber,
            batch.anchorBlockNumber,
            batch.anchorBlockHash,
            batch.expectedWithdrawalBatchIndex,
            sequencer,
            batch.blockTransition,
            batch.depositQueueTransition,
            batch.withdrawalQueueHash,
            config,
            proof
        );

        assertFalse(valid);
    }

    function test_verify_rejectsWrongSigner() public {
        Batch memory batch = _batch();
        bytes memory config = _config(portal);
        bytes memory proof = _proof(WRONG_SIGNER_KEY, batch, config);

        vm.prank(portal);
        bool valid = verifier.verify(
            batch.tempoBlockNumber,
            batch.anchorBlockNumber,
            batch.anchorBlockHash,
            batch.expectedWithdrawalBatchIndex,
            sequencer,
            batch.blockTransition,
            batch.depositQueueTransition,
            batch.withdrawalQueueHash,
            config,
            proof
        );

        assertFalse(valid);
        assertNotEq(wrongSigner, signer);
    }

    function test_verify_rejectsEmptyConfigOrProof() public {
        Batch memory batch = _batch();
        bytes memory config = _config(portal);
        bytes memory proof = _proof(SIGNER_KEY, batch, config);

        vm.prank(portal);
        assertFalse(
            verifier.verify(
                batch.tempoBlockNumber,
                batch.anchorBlockNumber,
                batch.anchorBlockHash,
                batch.expectedWithdrawalBatchIndex,
                sequencer,
                batch.blockTransition,
                batch.depositQueueTransition,
                batch.withdrawalQueueHash,
                "",
                proof
            )
        );

        vm.prank(portal);
        assertFalse(
            verifier.verify(
                batch.tempoBlockNumber,
                batch.anchorBlockNumber,
                batch.anchorBlockHash,
                batch.expectedWithdrawalBatchIndex,
                sequencer,
                batch.blockTransition,
                batch.depositQueueTransition,
                batch.withdrawalQueueHash,
                config,
                ""
            )
        );
    }

    struct Batch {
        uint64 tempoBlockNumber;
        uint64 anchorBlockNumber;
        bytes32 anchorBlockHash;
        uint64 expectedWithdrawalBatchIndex;
        BlockTransition blockTransition;
        DepositQueueTransition depositQueueTransition;
        bytes32 withdrawalQueueHash;
    }

    function _batch() private pure returns (Batch memory batch) {
        batch = Batch({
            tempoBlockNumber: 42,
            anchorBlockNumber: 42,
            anchorBlockHash: keccak256("tempo-anchor"),
            expectedWithdrawalBatchIndex: 7,
            blockTransition: BlockTransition({
                prevBlockHash: keccak256("previous-zone-block"),
                nextBlockHash: keccak256("next-zone-block")
            }),
            depositQueueTransition: DepositQueueTransition({
                prevProcessedHash: keccak256("previous-deposit"),
                nextProcessedHash: keccak256("next-deposit"),
                prevDepositNumber: 4,
                nextDepositNumber: 5
            }),
            withdrawalQueueHash: keccak256("withdrawal-queue")
        });
    }

    function _config(address configPortal) private view returns (bytes memory) {
        return abi.encode(
            NativeSignatureVerifier.NativeVerifierConfig({
                version: verifier.PROTOCOL_VERSION(),
                chainId: uint64(block.chainid),
                portal: configPortal,
                verifierVersion: VERIFIER_VERSION
            })
        );
    }

    function _proof(
        uint256 key,
        Batch memory batch,
        bytes memory config
    )
        private
        view
        returns (bytes memory)
    {
        bytes32 digest = verifier.computeDigest(
            batch.tempoBlockNumber,
            batch.anchorBlockNumber,
            batch.anchorBlockHash,
            batch.expectedWithdrawalBatchIndex,
            sequencer,
            batch.blockTransition,
            batch.depositQueueTransition,
            batch.withdrawalQueueHash,
            config
        );
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, digest);
        return abi.encode(
            NativeSignatureVerifier.NativeProof({
                digest: digest, signature: abi.encodePacked(r, s, v)
            })
        );
    }

}
