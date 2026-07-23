//! EIP-712 replication ACKs, settlement attestations, and leader-side storage.

use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use alloy_consensus::BlockHeader as _;
use alloy_eips::{BlockNumberOrTag, NumHash};
use alloy_network::primitives::HeaderResponse as _;
use alloy_primitives::{Address, B256, Bytes, Signature, U256};
use alloy_provider::{DynProvider, Provider as _};
use alloy_rpc_types_eth::BlockId;
use alloy_signer::SignerSync as _;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{Eip712Domain, SolStruct as _, SolValue as _, eip712_domain, sol};
use eyre::WrapErr as _;
use tempo_alloy::TempoNetwork;
use tokio::sync::{Notify, watch};

use crate::abi::{BatchSubmissionState, ZonePortal};

type SettlementSignatures =
    BTreeMap<u64, BTreeMap<B256, BTreeMap<Address, SignedSettlementAttestation>>>;

sol! {
    /// Exact settlement statement verified by ZonePortal.
    #[derive(Debug, PartialEq, Eq)]
    struct SettlementAttestation {
        uint32 zoneId;
        uint64 sequencerSetVersion;
        uint256 zoneHeight;
        uint256 withdrawalBatchIndex;
        address verifier;
        uint64 tempoBlockNumber;
        uint64 anchorBlockNumber;
        bytes32 anchorBlockHash;
        bytes32 blockTransitionHash;
        bytes32 depositQueueTransitionHash;
        bytes32 withdrawalQueueHash;
        bytes32 verifierConfigHash;
    }

    /// Settlement signature returned to the leader for quorum collection.
    #[derive(Debug, PartialEq, Eq)]
    struct SignedSettlementAttestation {
        SettlementAttestation attestation;
        bytes signature;
    }

    /// Off-chain proposal context needed to reproduce one immutable plan.
    #[derive(Debug, PartialEq, Eq)]
    struct SettlementProposal {
        uint64 portalBlockNumber;
        bytes32 portalBlockHash;
        SettlementAttestation attestation;
    }
}

/// Immutable EIP-712 domain shared by every settlement plan for one Portal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettlementDomain {
    pub l1_chain_id: u64,
    pub portal_address: Address,
    pub separator: B256,
}

impl SettlementDomain {
    pub fn new(l1_chain_id: u64, portal_address: Address) -> Self {
        let eip712 = eip712_domain! {
            name: "ZonePortal",
            version: "1",
            chain_id: l1_chain_id,
            verifying_contract: portal_address,
        };
        Self {
            l1_chain_id,
            portal_address,
            separator: eip712.separator(),
        }
    }

    fn eip712(self) -> Eip712Domain {
        eip712_domain! {
            name: "ZonePortal",
            version: "1",
            chain_id: self.l1_chain_id,
            verifying_contract: self.portal_address,
        }
    }

    pub fn settlement_digest(self, attestation: &SettlementAttestation) -> B256 {
        attestation.eip712_signing_hash(&self.eip712())
    }
}

/// One internally consistent view of all Portal-owned batch submission inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalSnapshot {
    pub l1_block_number: u64,
    pub l1_block_hash: B256,
    pub state: BatchSubmissionState,
}

/// Read the Portal snapshot at one concrete L1 block and verify its immutable domain identity.
pub async fn fetch_portal_snapshot(
    provider: &DynProvider<TempoNetwork>,
    portal_address: Address,
    candidate: Address,
    domain: SettlementDomain,
) -> eyre::Result<PortalSnapshot> {
    eyre::ensure!(
        portal_address == domain.portal_address,
        "portal address does not match cached settlement domain"
    );
    let header = provider
        .get_header_by_number(BlockNumberOrTag::Latest)
        .await?
        .ok_or_else(|| eyre::eyre!("latest L1 header is unavailable"))?;
    let l1_block_number = header.number();
    let l1_block_hash = header.hash();
    fetch_portal_snapshot_unchecked(
        provider,
        portal_address,
        candidate,
        domain,
        NumHash::new(l1_block_number, l1_block_hash),
    )
    .await
}

pub async fn fetch_portal_snapshot_at(
    provider: &DynProvider<TempoNetwork>,
    portal_address: Address,
    candidate: Address,
    domain: SettlementDomain,
    block: NumHash,
) -> eyre::Result<PortalSnapshot> {
    eyre::ensure!(
        portal_address == domain.portal_address,
        "portal address does not match cached settlement domain"
    );
    let canonical = provider
        .get_header_by_number(block.number.into())
        .await?
        .ok_or_else(|| eyre::eyre!("L1 snapshot header {} is unavailable", block.number))?;
    eyre::ensure!(
        canonical.hash() == block.hash,
        "L1 snapshot block hash is not canonical"
    );
    fetch_portal_snapshot_unchecked(provider, portal_address, candidate, domain, block).await
}

async fn fetch_portal_snapshot_unchecked(
    provider: &DynProvider<TempoNetwork>,
    portal_address: Address,
    candidate: Address,
    domain: SettlementDomain,
    block: NumHash,
) -> eyre::Result<PortalSnapshot> {
    let state = ZonePortal::new(portal_address, provider.clone())
        .batchSubmissionState(candidate)
        .block(BlockId::hash(block.hash))
        .call()
        .await?;

    eyre::ensure!(
        state.l1BlockNumber == U256::from(block.number),
        "portal snapshot block number does not match pinned L1 block"
    );
    eyre::ensure!(
        state.chainId == U256::from(domain.l1_chain_id),
        "portal snapshot chain ID does not match cached settlement domain"
    );
    eyre::ensure!(
        state.domainSeparator == domain.separator,
        "portal snapshot domain separator does not match cached settlement domain"
    );

    Ok(PortalSnapshot {
        l1_block_number: block.number,
        l1_block_hash: block.hash,
        state,
    })
}

impl SettlementAttestation {
    pub fn encode(&self) -> Vec<u8> {
        self.abi_encode()
    }

    pub fn decode(encoded: &[u8]) -> eyre::Result<Self> {
        Self::abi_decode(encoded).wrap_err("invalid settlement proposal encoding")
    }
}

impl SettlementProposal {
    pub fn encode(&self) -> Vec<u8> {
        self.abi_encode()
    }

    pub fn decode(encoded: &[u8]) -> eyre::Result<Self> {
        Self::abi_decode(encoded).wrap_err("invalid settlement proposal encoding")
    }
}

impl SignedSettlementAttestation {
    pub fn sign(
        attestation: SettlementAttestation,
        domain: SettlementDomain,
        signer: &PrivateKeySigner,
    ) -> eyre::Result<Self> {
        let signature = signer.sign_hash_sync(&domain.settlement_digest(&attestation))?;
        Ok(Self {
            attestation,
            signature: Bytes::copy_from_slice(&signature.as_bytes()),
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        self.abi_encode()
    }

    pub fn decode(encoded: &[u8]) -> eyre::Result<Self> {
        Self::abi_decode(encoded).wrap_err("invalid settlement signature encoding")
    }

    pub fn recover_signer(&self, domain: SettlementDomain) -> eyre::Result<Address> {
        let signature = Signature::try_from(self.signature.as_ref())
            .wrap_err("invalid settlement signature")?;
        alloy_consensus::crypto::secp256k1::recover_signer(
            &signature,
            domain.settlement_digest(&self.attestation),
        )
        .wrap_err("failed recovering settlement signer")
    }
}

/// A settlement statement and its distinct signer signatures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementCertificate {
    pub height: u64,
    pub digest: B256,
    pub attestation: SettlementAttestation,
    pub signatures: Vec<Bytes>,
}

/// Settlement certificates shared by P2P and batch submission.
#[derive(Debug, Clone)]
pub struct AttestationStore {
    settlements: Arc<RwLock<SettlementSignatures>>,
    settlement_changed: Arc<Notify>,
    submitted_height: watch::Sender<u64>,
}

impl Default for AttestationStore {
    fn default() -> Self {
        let (submitted_height, _) = watch::channel(0);
        Self {
            settlements: Arc::default(),
            settlement_changed: Arc::default(),
            submitted_height,
        }
    }
}

impl AttestationStore {
    /// Insert one settlement signature per recovered signer and statement digest.
    pub fn insert_settlement(
        &self,
        domain: SettlementDomain,
        signer: Address,
        signed: SignedSettlementAttestation,
    ) -> (bool, usize) {
        let height = signed
            .attestation
            .zoneHeight
            .try_into()
            .expect("validated settlement zone height must fit in u64");
        let digest = domain.settlement_digest(&signed.attestation);

        let (inserted, signature_count) = {
            let mut all = self
                .settlements
                .write()
                .expect("attestation store lock poisoned");

            let signatures = all.entry(height).or_default().entry(digest).or_default();
            let inserted = signatures.insert(signer, signed).is_none();
            (inserted, signatures.len())
        };

        // There is one in-order batch submission waiter; notify_one retains a permit if insertion
        // races between its store check and awaiting the notification.
        self.settlement_changed.notify_one();

        (inserted, signature_count)
    }

    /// Wait until any statement at `height` has at least `quorum` distinct signatures. If there aren't
    /// enough to meet quorum, the zone blocks will stall, this is intentional.
    pub async fn wait_for_settlement(&self, height: u64, quorum: usize) -> SettlementCertificate {
        loop {
            let notified = self.settlement_changed.notified();
            if let Some(certificate) = self.settlement_at(height, quorum) {
                return certificate;
            }
            notified.await;
        }
    }

    /// Return whether the leader already recorded this exact immutable proposal.
    pub fn contains_settlement(
        &self,
        domain: SettlementDomain,
        attestation: &SettlementAttestation,
    ) -> bool {
        let Ok(height) = u64::try_from(attestation.zoneHeight) else {
            return false;
        };
        let digest = domain.settlement_digest(attestation);
        self.settlements
            .read()
            .expect("attestation store lock poisoned")
            .get(&height)
            .and_then(|by_digest| by_digest.get(&digest))
            .is_some()
    }

    /// Get the settlement certificate at the zone block height
    fn settlement_at(&self, height: u64, quorum: usize) -> Option<SettlementCertificate> {
        let all = self
            .settlements
            .read()
            .expect("attestation store lock poisoned");
        let (digest, signatures) = all
            .get(&height)?
            .iter()
            .find(|(_, signatures)| signatures.len() >= quorum)?;
        let attestation = signatures.values().next()?.attestation.clone();

        Some(SettlementCertificate {
            height,
            digest: *digest,
            attestation,
            // Signer-address ordering makes transaction calldata deterministic.
            signatures: signatures
                .values()
                .map(|signed| signed.signature.clone())
                .collect(),
        })
    }

    /// Remove one unusable certificate without discarding other anchor candidates.
    pub fn remove_settlement(&self, height: u64, digest: B256) {
        let mut settlements = self
            .settlements
            .write()
            .expect("attestation store lock poisoned");
        if let Some(by_digest) = settlements.get_mut(&height) {
            by_digest.remove(&digest);
            if by_digest.is_empty() {
                settlements.remove(&height);
            }
        }
    }

    /// Remove all attestations covered by a confirmed batch submission.
    pub fn remove_submitted(&self, height: u64) {
        self.settlements
            .write()
            .expect("attestation store lock poisoned")
            .retain(|settlement_height, _| *settlement_height > height);
        self.submitted_height.send_if_modified(|submitted| {
            if height > *submitted {
                *submitted = height;
                true
            } else {
                false
            }
        });
    }

    /// Subscribe to the latest zone height confirmed by a batch submission or portal resync.
    pub fn subscribe_submitted_height(&self) -> watch::Receiver<u64> {
        self.submitted_height.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, B256, U256, keccak256, uint};
    use alloy_signer_local::PrivateKeySigner;
    use alloy_sol_types::{SolStruct as _, SolValue as _};

    use super::*;

    fn domain() -> SettlementDomain {
        SettlementDomain::new(1337, Address::repeat_byte(0x11))
    }

    #[test]
    fn settlement_type_and_signature_match_zone_portal() {
        const PORTAL_TYPE: &str = "SettlementAttestation(uint32 zoneId,uint64 sequencerSetVersion,uint256 zoneHeight,uint256 withdrawalBatchIndex,address verifier,uint64 tempoBlockNumber,uint64 anchorBlockNumber,bytes32 anchorBlockHash,bytes32 blockTransitionHash,bytes32 depositQueueTransitionHash,bytes32 withdrawalQueueHash,bytes32 verifierConfigHash)";
        assert_eq!(SettlementAttestation::eip712_encode_type(), PORTAL_TYPE);

        let attestation = SettlementAttestation {
            zoneId: 7,
            sequencerSetVersion: 3,
            zoneHeight: U256::from(120),
            withdrawalBatchIndex: U256::from(1),
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
        assert_eq!(domain.separator, domain_separator);
        let mut encoded_digest = Vec::with_capacity(66);
        encoded_digest.extend_from_slice(&[0x19, 0x01]);
        encoded_digest.extend_from_slice(domain_separator.as_slice());
        encoded_digest.extend_from_slice(struct_hash.as_slice());
        assert_eq!(
            domain.settlement_digest(&attestation),
            keccak256(encoded_digest)
        );

        let proposal = SettlementProposal {
            portalBlockNumber: 123,
            portalBlockHash: B256::repeat_byte(0x44),
            attestation: attestation.clone(),
        };
        assert_eq!(
            SettlementProposal::decode(&proposal.encode()).unwrap(),
            proposal
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

    #[test]
    fn rejects_high_s_settlement_signature() {
        const SECP256K1_ORDER: U256 =
            uint!(0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141_U256);

        let signer = PrivateKeySigner::random();
        let domain = domain();
        let attestation = SettlementAttestation {
            zoneId: 7,
            sequencerSetVersion: 3,
            zoneHeight: U256::from(10),
            withdrawalBatchIndex: U256::from(1),
            verifier: Address::repeat_byte(2),
            tempoBlockNumber: 100,
            anchorBlockNumber: 100,
            anchorBlockHash: B256::repeat_byte(3),
            blockTransitionHash: B256::repeat_byte(4),
            depositQueueTransitionHash: B256::repeat_byte(5),
            withdrawalQueueHash: B256::repeat_byte(6),
            verifierConfigHash: B256::repeat_byte(7),
        };
        let mut signed = SignedSettlementAttestation::sign(attestation, domain, &signer).unwrap();
        let signature = Signature::try_from(signed.signature.as_ref()).unwrap();
        let high_s_signature = Signature::new(
            signature.r(),
            SECP256K1_ORDER - signature.s(),
            !signature.v(),
        );
        signed.signature = Bytes::copy_from_slice(&high_s_signature.as_bytes());

        assert!(signed.recover_signer(domain).is_err());
    }

    #[tokio::test]
    async fn waits_for_quorum_and_removes_confirmed_attestations() {
        let store = AttestationStore::default();
        let signer_a = PrivateKeySigner::random();
        let signer_b = PrivateKeySigner::random();
        let attestation = SettlementAttestation {
            zoneId: 7,
            sequencerSetVersion: 3,
            zoneHeight: U256::from(10),
            withdrawalBatchIndex: U256::from(1),
            verifier: Address::repeat_byte(2),
            tempoBlockNumber: 100,
            anchorBlockNumber: 100,
            anchorBlockHash: B256::repeat_byte(3),
            blockTransitionHash: B256::repeat_byte(4),
            depositQueueTransitionHash: B256::repeat_byte(5),
            withdrawalQueueHash: B256::repeat_byte(6),
            verifierConfigHash: B256::repeat_byte(7),
        };
        store.insert_settlement(
            domain(),
            signer_a.address(),
            SignedSettlementAttestation::sign(attestation.clone(), domain(), &signer_a).unwrap(),
        );

        let waiting = {
            let store = store.clone();
            tokio::spawn(async move { store.wait_for_settlement(10, 2).await })
        };
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());

        store.insert_settlement(
            domain(),
            signer_b.address(),
            SignedSettlementAttestation::sign(attestation, domain(), &signer_b).unwrap(),
        );
        let certificate = waiting.await.unwrap();
        assert_eq!(certificate.signatures.len(), 2);

        store.remove_submitted(10);
        assert!(store.settlement_at(10, 1).is_none());
    }
}
