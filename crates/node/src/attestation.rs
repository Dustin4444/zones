//! EIP-712 replication ACKs, settlement attestations, and leader-side storage.

use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

const TEMPORARY_ATTESTATION_RETENTION_HEIGHTS: usize = 120;

type SettlementSignatures =
    BTreeMap<u64, BTreeMap<B256, BTreeMap<Address, SignedSettlementAttestation>>>;

use alloy_primitives::{Address, B256, Bytes, Signature, U256};
use alloy_signer::SignerSync as _;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{Eip712Domain, SolStruct as _, SolValue as _, eip712_domain, sol};

sol! {
    /// Off-chain acknowledgement signed after importing and persisting one zone block.
    #[derive(Debug, PartialEq, Eq)]
    struct BlockAck {
        uint32 zoneId;
        uint64 sequencerSetVersion;
        uint256 zoneHeight;
        bytes32 zoneBlockHash;
    }

    /// Exact settlement statement verified by ZonePortal in PR #669.
    #[derive(Debug, PartialEq, Eq)]
    struct SettlementAttestation {
        uint32 zoneId;
        uint64 sequencerSetVersion;
        uint256 zoneHeight;
        uint256 withdrawalBatchIndex;
        address sequencer;
        address verifier;
        uint64 tempoBlockNumber;
        uint64 anchorBlockNumber;
        bytes32 anchorBlockHash;
        bytes32 blockTransitionHash;
        bytes32 depositQueueTransitionHash;
        bytes32 withdrawalQueueHash;
        bytes32 verifierConfigHash;
    }

    /// Wire envelope returned over the authenticated ACK channel.
    #[derive(Debug, PartialEq, Eq)]
    struct SignedBlockAck {
        BlockAck ack;
        bytes signature;
    }

    /// Settlement signature returned to the leader for quorum collection.
    #[derive(Debug, PartialEq, Eq)]
    struct SignedSettlementAttestation {
        SettlementAttestation attestation;
        bytes signature;
    }
}

/// Immutable values that domain-separate one zone's attestations.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AttestationDomain {
    pub(crate) l1_chain_id: u64,
    pub(crate) portal_address: Address,
    pub(crate) zone_id: u32,
    pub(crate) sequencer_set_version: u64,
}

impl AttestationDomain {
    fn eip712(self) -> Eip712Domain {
        eip712_domain! {
            name: "ZonePortal",
            version: "1",
            chain_id: self.l1_chain_id,
            verifying_contract: self.portal_address,
        }
    }

    pub(crate) fn block_ack_digest(self, ack: &BlockAck) -> B256 {
        ack.eip712_signing_hash(&self.eip712())
    }

    pub(crate) fn settlement_digest(self, attestation: &SettlementAttestation) -> B256 {
        attestation.eip712_signing_hash(&self.eip712())
    }
}

impl BlockAck {
    pub(crate) fn new(domain: AttestationDomain, zone_height: u64, zone_block_hash: B256) -> Self {
        Self {
            zoneId: domain.zone_id,
            sequencerSetVersion: domain.sequencer_set_version,
            zoneHeight: U256::from(zone_height),
            zoneBlockHash: zone_block_hash,
        }
    }
}

impl SettlementAttestation {
    pub(crate) fn encode(&self) -> Vec<u8> {
        self.abi_encode()
    }

    pub(crate) fn decode(encoded: &[u8]) -> eyre::Result<Self> {
        Self::abi_decode(encoded)
            .map_err(|err| eyre::eyre!("invalid settlement proposal encoding: {err}"))
    }
}

impl SignedBlockAck {
    pub(crate) fn sign(
        ack: BlockAck,
        domain: AttestationDomain,
        signer: &PrivateKeySigner,
    ) -> eyre::Result<Self> {
        let signature = signer.sign_hash_sync(&domain.block_ack_digest(&ack))?;
        Ok(Self {
            ack,
            signature: Bytes::copy_from_slice(&signature.as_bytes()),
        })
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        self.abi_encode()
    }

    pub(crate) fn decode(encoded: &[u8]) -> eyre::Result<Self> {
        Self::abi_decode(encoded).map_err(|err| eyre::eyre!("invalid block ACK encoding: {err}"))
    }

    pub(crate) fn recover_signer(&self, domain: AttestationDomain) -> eyre::Result<Address> {
        let signature = Signature::try_from(self.signature.as_ref())
            .map_err(|err| eyre::eyre!("invalid block ACK signature: {err}"))?;
        signature
            .recover_address_from_prehash(&domain.block_ack_digest(&self.ack))
            .map_err(|err| eyre::eyre!("failed recovering block ACK signer: {err}"))
    }
}

impl SignedSettlementAttestation {
    pub(crate) fn sign(
        attestation: SettlementAttestation,
        domain: AttestationDomain,
        signer: &PrivateKeySigner,
    ) -> eyre::Result<Self> {
        let signature = signer.sign_hash_sync(&domain.settlement_digest(&attestation))?;
        Ok(Self {
            attestation,
            signature: Bytes::copy_from_slice(&signature.as_bytes()),
        })
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        self.abi_encode()
    }

    pub(crate) fn decode(encoded: &[u8]) -> eyre::Result<Self> {
        Self::abi_decode(encoded)
            .map_err(|err| eyre::eyre!("invalid settlement signature encoding: {err}"))
    }

    pub(crate) fn recover_signer(&self, domain: AttestationDomain) -> eyre::Result<Address> {
        let signature = Signature::try_from(self.signature.as_ref())
            .map_err(|err| eyre::eyre!("invalid settlement signature: {err}"))?;
        signature
            .recover_address_from_prehash(&domain.settlement_digest(&self.attestation))
            .map_err(|err| eyre::eyre!("failed recovering settlement signer: {err}"))
    }
}

/// Replication ACKs and settlement certificates retained by the leader.
#[derive(Debug, Clone, Default)]
pub(crate) struct AttestationStore {
    // TODO(multi-sequencer): Replace the temporary bounded retention with a
    // consume-and-remove API when the leader starts attaching quorum certificates to submitBatch.
    block_acks: Arc<RwLock<BTreeMap<u64, BTreeMap<Address, SignedBlockAck>>>>,
    settlements: Arc<RwLock<SettlementSignatures>>,
}

impl AttestationStore {
    /// Inserts one replication ACK per recovered signer and block height.
    pub(crate) fn insert_block_ack(
        &self,
        signer: Address,
        signed: SignedBlockAck,
    ) -> eyre::Result<(bool, usize)> {
        let height: u64 = signed
            .ack
            .zoneHeight
            .try_into()
            .map_err(|_| eyre::eyre!("zone height does not fit in u64"))?;
        let mut all = self
            .block_acks
            .write()
            .expect("attestation store lock poisoned");
        let (inserted, signature_count) = {
            let signatures = all.entry(height).or_default();
            let inserted = signatures.insert(signer, signed).is_none();
            (inserted, signatures.len())
        };

        // Temporary memory-safety bound: retain only the newest 120 attested block heights.
        // Once the leader submits certificates with submitBatch, that path should consume and
        // remove every attestation covered by the submitted batch instead.
        while all.len() > TEMPORARY_ATTESTATION_RETENTION_HEIGHTS {
            all.pop_first();
        }

        Ok((inserted, signature_count))
    }

    /// Inserts one settlement signature per recovered signer and statement digest.
    pub(crate) fn insert_settlement(
        &self,
        domain: AttestationDomain,
        signer: Address,
        signed: SignedSettlementAttestation,
    ) -> (bool, usize) {
        let height = signed
            .attestation
            .zoneHeight
            .try_into()
            .expect("validated settlement zone height must fit in u64");
        let digest = domain.settlement_digest(&signed.attestation);
        let mut all = self
            .settlements
            .write()
            .expect("attestation store lock poisoned");
        let (inserted, signature_count) = {
            let signatures = all.entry(height).or_default().entry(digest).or_default();
            let inserted = signatures.insert(signer, signed).is_none();
            (inserted, signatures.len())
        };

        // Temporary memory-safety bound until submitBatch consumes and removes the signatures for
        // each successfully settled batch.
        while all.len() > TEMPORARY_ATTESTATION_RETENTION_HEIGHTS {
            all.pop_first();
        }

        (inserted, signature_count)
    }

    #[cfg(test)]
    fn len_at(&self, height: u64) -> usize {
        self.block_acks
            .read()
            .expect("attestation store lock poisoned")
            .get(&height)
            .map_or(0, BTreeMap::len)
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, B256, U256, keccak256};
    use alloy_signer_local::PrivateKeySigner;
    use alloy_sol_types::{SolStruct as _, SolValue as _};

    use super::{
        AttestationDomain, AttestationStore, BlockAck, SettlementAttestation, SignedBlockAck,
        SignedSettlementAttestation,
    };

    fn domain() -> AttestationDomain {
        AttestationDomain {
            l1_chain_id: 1337,
            portal_address: Address::repeat_byte(0x11),
            zone_id: 7,
            sequencer_set_version: 3,
        }
    }

    #[test]
    fn signed_attestation_round_trips_and_recovers() {
        let signer = PrivateKeySigner::random();
        let ack = BlockAck::new(domain(), 42, B256::repeat_byte(1));
        let signed = SignedBlockAck::sign(ack, domain(), &signer).unwrap();
        let decoded = SignedBlockAck::decode(&signed.encode()).unwrap();
        assert_eq!(decoded, signed);
        assert_eq!(decoded.recover_signer(domain()).unwrap(), signer.address());
    }

    #[test]
    fn store_deduplicates_by_recovered_signer() {
        let signer = PrivateKeySigner::random();
        let ack = BlockAck::new(domain(), 42, B256::repeat_byte(2));
        let signed = SignedBlockAck::sign(ack, domain(), &signer).unwrap();
        let store = AttestationStore::default();
        assert_eq!(
            store
                .insert_block_ack(signer.address(), signed.clone())
                .unwrap(),
            (true, 1)
        );
        assert_eq!(
            store.insert_block_ack(signer.address(), signed).unwrap(),
            (false, 1)
        );
        assert_eq!(store.len_at(42), 1);
    }

    #[test]
    fn settlement_type_and_signature_match_zone_portal() {
        const PORTAL_TYPE: &str = "SettlementAttestation(uint32 zoneId,uint64 sequencerSetVersion,uint256 zoneHeight,uint256 withdrawalBatchIndex,address sequencer,address verifier,uint64 tempoBlockNumber,uint64 anchorBlockNumber,bytes32 anchorBlockHash,bytes32 blockTransitionHash,bytes32 depositQueueTransitionHash,bytes32 withdrawalQueueHash,bytes32 verifierConfigHash)";
        assert_eq!(SettlementAttestation::eip712_encode_type(), PORTAL_TYPE);

        let attestation = SettlementAttestation {
            zoneId: 7,
            sequencerSetVersion: 3,
            zoneHeight: U256::from(120),
            withdrawalBatchIndex: U256::from(1),
            sequencer: Address::repeat_byte(1),
            verifier: Address::repeat_byte(2),
            tempoBlockNumber: 100,
            anchorBlockNumber: 100,
            anchorBlockHash: B256::repeat_byte(3),
            blockTransitionHash: B256::repeat_byte(4),
            depositQueueTransitionHash: B256::repeat_byte(5),
            withdrawalQueueHash: B256::repeat_byte(6),
            verifierConfigHash: B256::repeat_byte(7),
        };
        let struct_hash = keccak256(
            (
                keccak256(PORTAL_TYPE),
                attestation.zoneId,
                attestation.sequencerSetVersion,
                attestation.zoneHeight,
                attestation.withdrawalBatchIndex,
                attestation.sequencer,
                attestation.verifier,
                attestation.tempoBlockNumber,
                attestation.anchorBlockNumber,
                attestation.anchorBlockHash,
                attestation.blockTransitionHash,
                attestation.depositQueueTransitionHash,
                attestation.withdrawalQueueHash,
                attestation.verifierConfigHash,
            )
                .abi_encode(),
        );
        let domain = domain();
        let domain_separator = keccak256(
            (
                keccak256(
                    "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
                ),
                keccak256("ZonePortal"),
                keccak256("1"),
                U256::from(domain.l1_chain_id),
                domain.portal_address,
            )
                .abi_encode(),
        );
        let mut encoded_digest = Vec::with_capacity(66);
        encoded_digest.extend_from_slice(&[0x19, 0x01]);
        encoded_digest.extend_from_slice(domain_separator.as_slice());
        encoded_digest.extend_from_slice(struct_hash.as_slice());
        assert_eq!(
            domain.settlement_digest(&attestation),
            keccak256(encoded_digest)
        );

        let signer = PrivateKeySigner::random();
        let signed = SignedSettlementAttestation::sign(attestation, domain, &signer).unwrap();
        let decoded = SignedSettlementAttestation::decode(&signed.encode()).unwrap();
        assert_eq!(decoded, signed);
        assert_eq!(decoded.recover_signer(domain).unwrap(), signer.address());

        let store = AttestationStore::default();
        assert_eq!(
            store.insert_settlement(domain, signer.address(), signed),
            (true, 1)
        );
    }
}
