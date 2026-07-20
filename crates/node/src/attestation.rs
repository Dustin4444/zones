//! EIP-712 zone-block attestations and the leader's in-memory certificate store.

use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

const TEMPORARY_ATTESTATION_RETENTION_HEIGHTS: usize = 120;

use alloy_primitives::{Address, B256, Bytes, Signature, U256};
use alloy_signer::SignerSync as _;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{Eip712Domain, SolStruct as _, SolValue as _, eip712_domain, sol};

/// Version of the settlement statement signed by zone nodes.
pub(crate) const SETTLEMENT_STATEMENT_VERSION: u64 = 0;

sol! {
    /// The statement a follower signs only after importing the advertised block.
    #[derive(Debug, PartialEq, Eq)]
    struct ZoneBlockAttestation {
        uint32 zoneId;
        uint64 sequencerSetVersion;
        uint64 settlementStatementVersion;
        uint256 zoneHeight;
        bytes32 parentZoneBlockHash;
        bytes32 zoneBlockHash;
        bytes32 withdrawalQueueHash;
        bytes32 depositQueueTransitionHash;
        bytes32 anchorBlockHash;
    }

    /// Wire envelope returned to the leader over the authenticated ACK channel.
    #[derive(Debug, PartialEq, Eq)]
    struct SignedZoneBlockAttestation {
        ZoneBlockAttestation attestation;
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

    pub(crate) fn digest(self, attestation: &ZoneBlockAttestation) -> B256 {
        attestation.eip712_signing_hash(&self.eip712())
    }
}

impl ZoneBlockAttestation {
    pub(crate) fn new(
        domain: AttestationDomain,
        zone_height: u64,
        parent_zone_block_hash: B256,
        zone_block_hash: B256,
        withdrawal_queue_hash: B256,
        prev_processed_deposit_hash: B256,
        next_processed_deposit_hash: B256,
        anchor_block_hash: B256,
    ) -> Self {
        Self {
            zoneId: domain.zone_id,
            sequencerSetVersion: domain.sequencer_set_version,
            settlementStatementVersion: SETTLEMENT_STATEMENT_VERSION,
            zoneHeight: U256::from(zone_height),
            parentZoneBlockHash: parent_zone_block_hash,
            zoneBlockHash: zone_block_hash,
            withdrawalQueueHash: withdrawal_queue_hash,
            depositQueueTransitionHash: alloy_primitives::keccak256(
                (prev_processed_deposit_hash, next_processed_deposit_hash).abi_encode(),
            ),
            anchorBlockHash: anchor_block_hash,
        }
    }
}

impl SignedZoneBlockAttestation {
    pub(crate) fn sign(
        attestation: ZoneBlockAttestation,
        domain: AttestationDomain,
        signer: &PrivateKeySigner,
    ) -> eyre::Result<Self> {
        let signature = signer.sign_hash_sync(&domain.digest(&attestation))?;
        Ok(Self {
            attestation,
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
            .recover_address_from_prehash(&domain.digest(&self.attestation))
            .map_err(|err| eyre::eyre!("failed recovering block ACK signer: {err}"))
    }
}

/// Signatures retained by block height and recovered secp256k1 identity.
#[derive(Debug, Clone, Default)]
pub(crate) struct AttestationStore {
    // TODO(multi-sequencer): Replace the temporary bounded retention in `insert` with a
    // consume-and-remove API when the leader starts attaching quorum certificates to submitBatch.
    inner: Arc<RwLock<BTreeMap<u64, BTreeMap<Address, SignedZoneBlockAttestation>>>>,
}

impl AttestationStore {
    /// Inserts once per recovered signer. Returns the number of distinct signatures at the height.
    pub(crate) fn insert(
        &self,
        signer: Address,
        signed: SignedZoneBlockAttestation,
    ) -> eyre::Result<(bool, usize)> {
        let height: u64 = signed
            .attestation
            .zoneHeight
            .try_into()
            .map_err(|_| eyre::eyre!("zone height does not fit in u64"))?;
        let mut all = self.inner.write().expect("attestation store lock poisoned");
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

    #[cfg(test)]
    fn len_at(&self, height: u64) -> usize {
        self.inner
            .read()
            .expect("attestation store lock poisoned")
            .get(&height)
            .map_or(0, BTreeMap::len)
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, B256};
    use alloy_signer_local::PrivateKeySigner;

    use super::{
        AttestationDomain, AttestationStore, SignedZoneBlockAttestation, ZoneBlockAttestation,
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
        let statement = ZoneBlockAttestation::new(
            domain(),
            42,
            B256::repeat_byte(1),
            B256::repeat_byte(2),
            B256::repeat_byte(3),
            B256::repeat_byte(4),
            B256::repeat_byte(5),
            B256::repeat_byte(6),
        );
        let signed = SignedZoneBlockAttestation::sign(statement, domain(), &signer).unwrap();
        let decoded = SignedZoneBlockAttestation::decode(&signed.encode()).unwrap();
        assert_eq!(decoded, signed);
        assert_eq!(decoded.recover_signer(domain()).unwrap(), signer.address());
    }

    #[test]
    fn store_deduplicates_by_recovered_signer() {
        let signer = PrivateKeySigner::random();
        let statement = ZoneBlockAttestation::new(
            domain(),
            42,
            B256::ZERO,
            B256::repeat_byte(2),
            B256::ZERO,
            B256::ZERO,
            B256::ZERO,
            B256::ZERO,
        );
        let signed = SignedZoneBlockAttestation::sign(statement, domain(), &signer).unwrap();
        let store = AttestationStore::default();
        assert_eq!(
            store.insert(signer.address(), signed.clone()).unwrap(),
            (true, 1)
        );
        assert_eq!(store.insert(signer.address(), signed).unwrap(), (false, 1));
        assert_eq!(store.len_at(42), 1);
    }
}
