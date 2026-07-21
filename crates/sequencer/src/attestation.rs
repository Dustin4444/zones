//! EIP-712 replication ACKs, settlement attestations, and leader-side storage.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256, Bytes, Signature, U256};
use alloy_signer::SignerSync as _;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{Eip712Domain, SolStruct as _, SolValue as _, eip712_domain, sol};
use tokio::sync::Notify;
use tracing::{info, warn};

const TEMPORARY_ATTESTATION_RETENTION_HEIGHTS: usize = 120;
const QUORUM_WAIT_LOG_INTERVAL: Duration = Duration::from_secs(60);

type Settlements = BTreeMap<u64, SettlementTarget>;

#[derive(Debug)]
struct SettlementTarget {
    block_hash: B256,
    refusals: BTreeSet<Address>,
    digest: B256,
    attestation: SettlementAttestation,
    signatures: BTreeMap<Address, Bytes>,
}

#[derive(Debug, Default)]
struct AttestationState {
    members: BTreeSet<Address>,
    settlements: Settlements,
}

sol! {
    /// Off-chain acknowledgement signed after importing and persisting one zone block.
    #[derive(Debug, PartialEq, Eq)]
    struct BlockAck {
        uint32 zoneId;
        uint64 sequencerSetVersion;
        uint256 zoneHeight;
        bytes32 zoneBlockHash;
    }

    /// Exact settlement statement verified by ZonePortal.
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

    /// Terminal refusal to sign one exact zone target.
    #[derive(Debug, PartialEq, Eq)]
    struct SigningRefusal {
        uint32 zoneId;
        uint64 sequencerSetVersion;
        uint256 zoneHeight;
        bytes32 zoneBlockHash;
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

    /// Refusal authenticated by the manifest member's secp256k1 key.
    #[derive(Debug, PartialEq, Eq)]
    struct SignedSigningRefusal {
        SigningRefusal refusal;
        bytes signature;
    }
}

/// Immutable values that domain-separate one zone's attestations.
#[derive(Debug, Clone, Copy)]
pub struct AttestationDomain {
    pub l1_chain_id: u64,
    pub portal_address: Address,
    pub zone_id: u32,
    pub sequencer_set_version: u64,
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

    pub fn block_ack_digest(self, ack: &BlockAck) -> B256 {
        ack.eip712_signing_hash(&self.eip712())
    }

    pub fn settlement_digest(self, attestation: &SettlementAttestation) -> B256 {
        attestation.eip712_signing_hash(&self.eip712())
    }

    pub fn signing_refusal_digest(self, refusal: &SigningRefusal) -> B256 {
        refusal.eip712_signing_hash(&self.eip712())
    }
}

impl BlockAck {
    pub fn new(domain: AttestationDomain, zone_height: u64, zone_block_hash: B256) -> Self {
        Self {
            zoneId: domain.zone_id,
            sequencerSetVersion: domain.sequencer_set_version,
            zoneHeight: U256::from(zone_height),
            zoneBlockHash: zone_block_hash,
        }
    }
}

impl SettlementAttestation {
    pub fn encode(&self) -> Vec<u8> {
        self.abi_encode()
    }

    pub fn decode(encoded: &[u8]) -> eyre::Result<Self> {
        Self::abi_decode(encoded)
            .map_err(|err| eyre::eyre!("invalid settlement proposal encoding: {err}"))
    }
}

impl SigningRefusal {
    pub fn new(domain: AttestationDomain, target: BlockNumHash) -> Self {
        Self {
            zoneId: domain.zone_id,
            sequencerSetVersion: domain.sequencer_set_version,
            zoneHeight: U256::from(target.number),
            zoneBlockHash: target.hash,
        }
    }

    pub fn target(&self, domain: AttestationDomain) -> eyre::Result<BlockNumHash> {
        eyre::ensure!(
            self.zoneId == domain.zone_id
                && self.sequencerSetVersion == domain.sequencer_set_version,
            "signing refusal does not match the active zone signer set"
        );
        Ok(BlockNumHash {
            number: self
                .zoneHeight
                .try_into()
                .map_err(|_| eyre::eyre!("refusal zone height does not fit in u64"))?,
            hash: self.zoneBlockHash,
        })
    }
}

impl SignedBlockAck {
    pub fn sign(
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

    pub fn encode(&self) -> Vec<u8> {
        self.abi_encode()
    }

    pub fn decode(encoded: &[u8]) -> eyre::Result<Self> {
        Self::abi_decode(encoded).map_err(|err| eyre::eyre!("invalid block ACK encoding: {err}"))
    }

    pub fn recover_signer(&self, domain: AttestationDomain) -> eyre::Result<Address> {
        let signature = Signature::try_from(self.signature.as_ref())
            .map_err(|err| eyre::eyre!("invalid block ACK signature: {err}"))?;
        eyre::ensure!(
            signature.normalize_s().is_none(),
            "block ACK signature has a non-canonical high-s value"
        );
        signature
            .recover_address_from_prehash(&domain.block_ack_digest(&self.ack))
            .map_err(|err| eyre::eyre!("failed recovering block ACK signer: {err}"))
    }
}

impl SignedSettlementAttestation {
    pub fn sign(
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

    pub fn encode(&self) -> Vec<u8> {
        self.abi_encode()
    }

    pub fn decode(encoded: &[u8]) -> eyre::Result<Self> {
        Self::abi_decode(encoded)
            .map_err(|err| eyre::eyre!("invalid settlement signature encoding: {err}"))
    }

    pub fn recover_signer(&self, domain: AttestationDomain) -> eyre::Result<Address> {
        let signature = Signature::try_from(self.signature.as_ref())
            .map_err(|err| eyre::eyre!("invalid settlement signature: {err}"))?;
        eyre::ensure!(
            signature.normalize_s().is_none(),
            "settlement signature has a non-canonical high-s value"
        );
        signature
            .recover_address_from_prehash(&domain.settlement_digest(&self.attestation))
            .map_err(|err| eyre::eyre!("failed recovering settlement signer: {err}"))
    }
}

impl SignedSigningRefusal {
    pub fn sign(
        refusal: SigningRefusal,
        domain: AttestationDomain,
        signer: &PrivateKeySigner,
    ) -> eyre::Result<Self> {
        let signature = signer.sign_hash_sync(&domain.signing_refusal_digest(&refusal))?;
        Ok(Self {
            refusal,
            signature: Bytes::copy_from_slice(&signature.as_bytes()),
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        self.abi_encode()
    }

    pub fn decode(encoded: &[u8]) -> eyre::Result<Self> {
        Self::abi_decode(encoded)
            .map_err(|err| eyre::eyre!("invalid signing refusal encoding: {err}"))
    }

    pub fn recover_signer(&self, domain: AttestationDomain) -> eyre::Result<Address> {
        let signature = Signature::try_from(self.signature.as_ref())
            .map_err(|err| eyre::eyre!("invalid signing refusal signature: {err}"))?;
        eyre::ensure!(
            signature.normalize_s().is_none(),
            "signing refusal has a non-canonical high-s signature"
        );
        signature
            .recover_address_from_prehash(&domain.signing_refusal_digest(&self.refusal))
            .map_err(|err| eyre::eyre!("failed recovering signing refusal signer: {err}"))
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
#[derive(Debug, Default)]
struct AttestationStoreInner {
    state: RwLock<AttestationState>,
    settlement_changed: Notify,
}

/// Leader-side settlement quorum state.
#[derive(Debug, Clone)]
pub struct AttestationStore(Arc<AttestationStoreInner>);

/// Why collection stopped before producing a settlement certificate.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SettlementWaitError {
    /// Authenticated terminal refusals made the configured quorum impossible.
    #[error(
        "settlement quorum refused at zone height {height}: quorum={quorum} possible_signers={possible_signers}"
    )]
    QuorumRefused {
        height: u64,
        quorum: usize,
        possible_signers: usize,
    },
    /// The leader registered a different canonical hash at the same height.
    #[error(
        "settlement target replaced at zone height {height}: expected={expected} actual={actual}"
    )]
    TargetReplaced {
        height: u64,
        expected: B256,
        actual: B256,
    },
}

impl AttestationStore {
    /// Create a store for the manifest's complete secp256k1 signer set.
    pub fn new(members: impl IntoIterator<Item = Address>) -> Self {
        Self(Arc::new(AttestationStoreInner {
            state: RwLock::new(AttestationState {
                members: members.into_iter().collect(),
                ..Default::default()
            }),
            settlement_changed: Notify::new(),
        }))
    }

    /// Register the leader's exact proposal before broadcasting it to followers.
    pub fn register_settlement(
        &self,
        domain: AttestationDomain,
        signer: Address,
        target: BlockNumHash,
        signed: SignedSettlementAttestation,
    ) -> eyre::Result<()> {
        eyre::ensure!(
            signed.recover_signer(domain)? == signer,
            "settlement signature does not match its claimed signer"
        );
        let height = signed
            .attestation
            .zoneHeight
            .try_into()
            .expect("validated settlement zone height must fit in u64");
        eyre::ensure!(
            height == target.number,
            "settlement height {height} does not match target height {}",
            target.number
        );
        let digest = domain.settlement_digest(&signed.attestation);
        let mut state = self
            .0
            .state
            .write()
            .expect("attestation store lock poisoned");
        eyre::ensure!(
            state.members.contains(&signer),
            "settlement signer {signer} is not a manifest member"
        );
        if let Some(existing) = state.settlements.get_mut(&height)
            && existing.block_hash == target.hash
        {
            eyre::ensure!(
                existing.digest == digest && existing.attestation == signed.attestation,
                "settlement target already has a different registered proposal"
            );
            existing.signatures.insert(signer, signed.signature);
            drop(state);
            self.0.settlement_changed.notify_one();
            return Ok(());
        }

        // Only the leader registers proposals. Inserting by height atomically supersedes a stale
        // branch target and wakes its waiter with `TargetReplaced`.
        state.settlements.insert(
            height,
            SettlementTarget {
                block_hash: target.hash,
                refusals: BTreeSet::new(),
                digest,
                attestation: signed.attestation,
                signatures: BTreeMap::from([(signer, signed.signature)]),
            },
        );

        // Temporary memory-safety bound until successful submitBatch calls consume certificates.
        while state.settlements.len() > TEMPORARY_ATTESTATION_RETENTION_HEIGHTS {
            state.settlements.pop_first();
        }
        drop(state);
        // There is one in-order batch submission waiter; notify_one retains a permit if insertion
        // races between its store check and awaiting the notification.
        self.0.settlement_changed.notify_one();

        Ok(())
    }

    /// Add a follower signature to the exact proposal previously registered by the leader.
    pub fn insert_settlement_signature(
        &self,
        domain: AttestationDomain,
        signer: Address,
        signed: SignedSettlementAttestation,
    ) -> eyre::Result<(BlockNumHash, usize)> {
        eyre::ensure!(
            signed.recover_signer(domain)? == signer,
            "settlement signature does not match its claimed signer"
        );
        let height: u64 = signed
            .attestation
            .zoneHeight
            .try_into()
            .map_err(|_| eyre::eyre!("settlement height does not fit in u64"))?;
        let digest = domain.settlement_digest(&signed.attestation);
        let mut state = self
            .0
            .state
            .write()
            .expect("attestation store lock poisoned");
        eyre::ensure!(
            state.members.contains(&signer),
            "settlement signer {signer} is not a manifest member"
        );
        let proposal = state
            .settlements
            .get_mut(&height)
            .ok_or_else(|| eyre::eyre!("settlement height was not registered by the leader"))?;
        eyre::ensure!(
            proposal.digest == digest && proposal.attestation == signed.attestation,
            "settlement signature does not match the registered leader proposal"
        );
        proposal.signatures.insert(signer, signed.signature);
        let signature_count = proposal.signatures.len();
        let block_hash = proposal.block_hash;
        drop(state);
        self.0.settlement_changed.notify_one();
        Ok((
            BlockNumHash {
                number: height,
                hash: block_hash,
            },
            signature_count,
        ))
    }

    /// Return the exact proposal already registered for a target.
    pub fn settlement_proposal(&self, target: BlockNumHash) -> Option<SettlementAttestation> {
        let state = self
            .0
            .state
            .read()
            .expect("attestation store lock poisoned");
        let proposal = state.settlements.get(&target.number)?;
        (proposal.block_hash == target.hash).then(|| proposal.attestation.clone())
    }

    /// Refuse an exact proposal previously registered by the leader.
    pub fn refuse_to_sign(&self, signer: Address, target: BlockNumHash) -> bool {
        let changed = {
            let mut state = self
                .0
                .state
                .write()
                .expect("attestation store lock poisoned");
            if !state.members.contains(&signer) {
                return false;
            }
            let Some(proposal) = state.settlements.get_mut(&target.number) else {
                return false;
            };
            if proposal.block_hash != target.hash {
                return false;
            }
            proposal.refusals.insert(signer)
        };
        if changed {
            self.0.settlement_changed.notify_one();
        }
        changed
    }

    /// Wait until a statement reaches quorum or terminal refusals make quorum impossible.
    pub async fn wait_for_settlement(
        &self,
        target: BlockNumHash,
        quorum: usize,
    ) -> Result<SettlementCertificate, SettlementWaitError> {
        let started = Instant::now();
        let mut log_interval = tokio::time::interval(QUORUM_WAIT_LOG_INTERVAL);
        log_interval.tick().await;
        info!(
            zone_height = target.number,
            zone_block_hash = %target.hash,
            quorum,
            "Waiting for settlement quorum"
        );
        loop {
            let notified = self.0.settlement_changed.notified();
            if let Some(certificate) = self.settlement_at(target, quorum)? {
                return Ok(certificate);
            }
            tokio::select! {
                () = notified => {}
                _ = log_interval.tick() => warn!(
                    zone_height = target.number,
                    zone_block_hash = %target.hash,
                    quorum,
                    elapsed_seconds = started.elapsed().as_secs(),
                    "Still waiting for settlement quorum"
                ),
            }
        }
    }

    fn settlement_at(
        &self,
        target: BlockNumHash,
        quorum: usize,
    ) -> Result<Option<SettlementCertificate>, SettlementWaitError> {
        let state = self
            .0
            .state
            .read()
            .expect("attestation store lock poisoned");
        let Some(settlement_target) = state.settlements.get(&target.number) else {
            return Ok(None);
        };
        if settlement_target.block_hash != target.hash {
            return Err(SettlementWaitError::TargetReplaced {
                height: target.number,
                expected: target.hash,
                actual: settlement_target.block_hash,
            });
        }
        if settlement_target.signatures.len() >= quorum {
            return Ok(Some(SettlementCertificate {
                height: target.number,
                digest: settlement_target.digest,
                attestation: settlement_target.attestation.clone(),
                // Signer-address ordering makes transaction calldata deterministic.
                signatures: settlement_target
                    .signatures
                    .values()
                    .take(quorum)
                    .cloned()
                    .collect(),
            }));
        }

        let possible_signers = state
            .members
            .iter()
            .filter(|signer| {
                !settlement_target.refusals.contains(*signer)
                    || settlement_target.signatures.contains_key(*signer)
            })
            .count();
        if possible_signers < quorum {
            Err(SettlementWaitError::QuorumRefused {
                height: target.number,
                quorum,
                possible_signers,
            })
        } else {
            Ok(None)
        }
    }

    /// Remove one unusable exact-target proposal.
    pub fn remove_settlement(&self, target: BlockNumHash) {
        let mut state = self
            .0
            .state
            .write()
            .expect("attestation store lock poisoned");
        if state
            .settlements
            .get(&target.number)
            .is_some_and(|stored| stored.block_hash == target.hash)
        {
            state.settlements.remove(&target.number);
        }
    }

    /// Remove all settlement attestations covered by a confirmed batch submission.
    pub fn remove_submitted(&self, height: u64) {
        let mut state = self
            .0
            .state
            .write()
            .expect("attestation store lock poisoned");
        state
            .settlements
            .retain(|settlement_height, _| *settlement_height > height);
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, B256, U256, keccak256};
    use alloy_signer_local::PrivateKeySigner;
    use alloy_sol_types::{SolStruct as _, SolValue as _};

    use super::*;

    fn domain() -> AttestationDomain {
        AttestationDomain {
            l1_chain_id: 1337,
            portal_address: Address::repeat_byte(0x11),
            zone_id: 7,
            sequencer_set_version: 3,
        }
    }

    fn target(height: u64) -> BlockNumHash {
        BlockNumHash {
            number: height,
            hash: B256::repeat_byte(height as u8),
        }
    }

    fn settlement(height: u64) -> SettlementAttestation {
        SettlementAttestation {
            zoneId: 7,
            sequencerSetVersion: 3,
            zoneHeight: U256::from(height),
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
    fn signed_refusal_round_trips_and_recovers() {
        let signer = PrivateKeySigner::random();
        let target = target(42);
        let signed =
            SignedSigningRefusal::sign(SigningRefusal::new(domain(), target), domain(), &signer)
                .unwrap();
        let decoded = SignedSigningRefusal::decode(&signed.encode()).unwrap();
        assert_eq!(decoded, signed);
        assert_eq!(decoded.refusal.target(domain()).unwrap(), target);
        assert_eq!(decoded.recover_signer(domain()).unwrap(), signer.address());
    }

    #[test]
    fn rejects_noncanonical_high_s_signatures() {
        let signer = PrivateKeySigner::random();
        let attestation = settlement(42);
        let mut signed = SignedSettlementAttestation::sign(attestation, domain(), &signer).unwrap();
        let signature = Signature::try_from(signed.signature.as_ref()).unwrap();
        let curve_order = U256::from_str_radix(
            "fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141",
            16,
        )
        .unwrap();
        let high_s = Signature::new(signature.r(), curve_order - signature.s(), !signature.v());
        signed.signature = Bytes::copy_from_slice(&high_s.as_bytes());

        assert!(signed.recover_signer(domain()).is_err());
        assert!(
            AttestationStore::new([signer.address()])
                .register_settlement(domain(), signer.address(), target(42), signed)
                .is_err()
        );
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

        let store = AttestationStore::new([signer.address()]);
        store
            .register_settlement(domain, signer.address(), target(120), signed)
            .unwrap();
    }

    #[tokio::test]
    async fn waits_for_quorum_and_removes_confirmed_attestations() {
        let signer_a = PrivateKeySigner::random();
        let signer_b = PrivateKeySigner::random();
        let store = AttestationStore::new([signer_a.address(), signer_b.address()]);
        let attestation = SettlementAttestation {
            zoneId: 7,
            sequencerSetVersion: 3,
            zoneHeight: U256::from(10),
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
        store
            .register_settlement(
                domain(),
                signer_a.address(),
                target(10),
                SignedSettlementAttestation::sign(attestation.clone(), domain(), &signer_a)
                    .unwrap(),
            )
            .unwrap();

        let waiting = {
            let store = store.clone();
            tokio::spawn(async move { store.wait_for_settlement(target(10), 2).await })
        };
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());

        store
            .insert_settlement_signature(
                domain(),
                signer_b.address(),
                SignedSettlementAttestation::sign(attestation, domain(), &signer_b).unwrap(),
            )
            .unwrap();
        let certificate = waiting.await.unwrap().unwrap();
        assert_eq!(certificate.signatures.len(), 2);

        store.remove_submitted(10);
        assert!(matches!(store.settlement_at(target(10), 1), Ok(None)));
    }

    #[tokio::test]
    async fn terminal_refusals_wake_an_impossible_quorum() {
        let leader = PrivateKeySigner::random();
        let follower_a = PrivateKeySigner::random();
        let follower_b = PrivateKeySigner::random();
        let store =
            AttestationStore::new([leader.address(), follower_a.address(), follower_b.address()]);
        let attestation = SettlementAttestation {
            zoneId: 7,
            sequencerSetVersion: 3,
            zoneHeight: U256::from(10),
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
        store
            .register_settlement(
                domain(),
                leader.address(),
                target(10),
                SignedSettlementAttestation::sign(attestation, domain(), &leader).unwrap(),
            )
            .unwrap();

        assert!(store.refuse_to_sign(follower_a.address(), target(10)));
        let waiting = {
            let store = store.clone();
            tokio::spawn(async move { store.wait_for_settlement(target(10), 2).await })
        };
        tokio::task::yield_now().await;
        assert!(
            !waiting.is_finished(),
            "one refusal still leaves quorum possible"
        );

        assert!(store.refuse_to_sign(follower_b.address(), target(10)));
        assert_eq!(
            waiting.await.unwrap().unwrap_err(),
            SettlementWaitError::QuorumRefused {
                height: 10,
                quorum: 2,
                possible_signers: 1,
            }
        );
    }

    #[test]
    fn refusal_requires_a_registered_exact_target() {
        let leader = PrivateKeySigner::random();
        let follower = PrivateKeySigner::random();
        let store = AttestationStore::new([leader.address(), follower.address()]);
        let target = target(10);

        assert!(!store.refuse_to_sign(follower.address(), target));
        assert!(matches!(store.settlement_at(target, 2), Ok(None)));
    }

    #[test]
    fn follower_cannot_create_an_alternate_candidate() {
        let leader = PrivateKeySigner::random();
        let follower = PrivateKeySigner::random();
        let store = AttestationStore::new([leader.address(), follower.address()]);
        let target = target(10);
        store
            .register_settlement(
                domain(),
                leader.address(),
                target,
                SignedSettlementAttestation::sign(settlement(10), domain(), &leader).unwrap(),
            )
            .unwrap();

        let mut alternate = settlement(10);
        alternate.anchorBlockHash = B256::repeat_byte(4);
        assert!(
            store
                .insert_settlement_signature(
                    domain(),
                    follower.address(),
                    SignedSettlementAttestation::sign(alternate, domain(), &follower).unwrap(),
                )
                .is_err()
        );
        assert!(matches!(store.settlement_at(target, 2), Ok(None)));
    }

    #[test]
    fn refusals_are_exact_target_and_signature_order_independent() {
        for refusal_before_signature in [false, true] {
            let leader = PrivateKeySigner::random();
            let follower = PrivateKeySigner::random();
            let store = AttestationStore::new([leader.address(), follower.address()]);
            let attestation = SettlementAttestation {
                zoneId: 7,
                sequencerSetVersion: 3,
                zoneHeight: U256::from(10),
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
            let old_target = target(10);
            let follower_signature =
                SignedSettlementAttestation::sign(attestation.clone(), domain(), &follower)
                    .unwrap();
            store
                .register_settlement(
                    domain(),
                    leader.address(),
                    old_target,
                    SignedSettlementAttestation::sign(attestation.clone(), domain(), &leader)
                        .unwrap(),
                )
                .unwrap();
            if !refusal_before_signature {
                store
                    .insert_settlement_signature(
                        domain(),
                        follower.address(),
                        follower_signature.clone(),
                    )
                    .unwrap();
            }
            assert!(store.refuse_to_sign(follower.address(), old_target));
            if refusal_before_signature {
                store
                    .insert_settlement_signature(domain(), follower.address(), follower_signature)
                    .unwrap();
            }
            assert!(matches!(store.settlement_at(old_target, 2), Ok(Some(_))));

            let new_target = BlockNumHash {
                number: 10,
                hash: B256::repeat_byte(0xff),
            };
            let mut replacement = attestation;
            replacement.blockTransitionHash = B256::repeat_byte(0xff);
            store
                .register_settlement(
                    domain(),
                    leader.address(),
                    new_target,
                    SignedSettlementAttestation::sign(replacement, domain(), &leader).unwrap(),
                )
                .unwrap();
            assert!(matches!(store.settlement_at(new_target, 2), Ok(None)));
            assert_eq!(
                store.settlement_at(old_target, 2).unwrap_err(),
                SettlementWaitError::TargetReplaced {
                    height: 10,
                    expected: old_target.hash,
                    actual: new_target.hash,
                }
            );
        }
    }

    #[tokio::test]
    async fn replacing_a_target_cancels_its_waiter() {
        let leader = PrivateKeySigner::random();
        let follower = PrivateKeySigner::random();
        let store = AttestationStore::new([leader.address(), follower.address()]);
        let old_target = target(10);
        store
            .register_settlement(
                domain(),
                leader.address(),
                old_target,
                SignedSettlementAttestation::sign(settlement(10), domain(), &leader).unwrap(),
            )
            .unwrap();

        let waiting = {
            let store = store.clone();
            tokio::spawn(async move { store.wait_for_settlement(old_target, 2).await })
        };
        tokio::task::yield_now().await;

        let new_target = BlockNumHash {
            number: 10,
            hash: B256::repeat_byte(0xff),
        };
        let mut replacement = settlement(10);
        replacement.blockTransitionHash = B256::repeat_byte(0xff);
        store
            .register_settlement(
                domain(),
                leader.address(),
                new_target,
                SignedSettlementAttestation::sign(replacement, domain(), &leader).unwrap(),
            )
            .unwrap();

        assert_eq!(
            waiting.await.unwrap().unwrap_err(),
            SettlementWaitError::TargetReplaced {
                height: 10,
                expected: old_target.hash,
                actual: new_target.hash,
            }
        );
    }
}
