use super::*;

/// Sequencer encryption private keys indexed by their position in Portal key history.
#[derive(Clone)]
pub struct SequencerKeyring {
    keys: Vec<(U256, k256::SecretKey)>,
}

impl SequencerKeyring {
    /// Create a keyring from `(Portal key index, private key)` entries.
    pub fn new(mut keys: Vec<(U256, k256::SecretKey)>) -> eyre::Result<Self> {
        eyre::ensure!(!keys.is_empty(), "sequencer encryption keyring is empty");
        keys.sort_unstable_by_key(|(index, _)| *index);
        eyre::ensure!(
            keys.windows(2).all(|pair| pair[0].0 != pair[1].0),
            "sequencer encryption keyring contains duplicate indices"
        );
        Ok(Self { keys })
    }

    /// Return the private key registered at `key_index`.
    pub fn key_at(&self, key_index: U256) -> eyre::Result<&k256::SecretKey> {
        self.keys
            .iter()
            .find_map(|(index, key)| (*index == key_index).then_some(key))
            .ok_or_else(|| {
                eyre::eyre!("missing sequencer encryption key for portal key index {key_index}")
            })
    }

    /// Portal indices present in this keyring.
    pub fn indices(&self) -> impl Iterator<Item = U256> + '_ {
        self.keys.iter().map(|(index, _)| *index)
    }
}

impl core::fmt::Debug for SequencerKeyring {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SequencerKeyring")
            .field("indices", &self.indices().collect::<Vec<_>>())
            .finish()
    }
}

/// An L1 block's header paired with the deposits found in that block.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct L1BlockDeposits {
    /// The sealed L1 block header (caches the block hash).
    pub header: SealedHeader<TempoHeader>,
    /// Portal events extracted from this block.
    pub events: L1PortalEvents,
}

impl L1BlockDeposits {
    /// Prepare all deposits for the payload builder.
    ///
    /// Decrypts encrypted deposits and ABI-encodes into the types the `advanceTempo` call expects.
    /// Mint-recipient policy is enforced by upstream TIP-20 after the L1 state is anchored.
    /// The resulting [`PreparedL1Block`] is ready to be passed via payload attributes to the
    /// builder.
    pub async fn prepare(
        self,
        sequencer_keys: &SequencerKeyring,
        portal_address: Address,
    ) -> eyre::Result<PreparedL1Block> {
        use crate::precompiles::ecies;

        let start = std::time::Instant::now();
        let l1_block_number = self.header.inner.number;
        let total_deposits = self.events.deposits.len();
        let mut queued_deposits: Vec<abi::QueuedDeposit> = Vec::new();
        let mut decryptions: Vec<abi::DecryptionData> = Vec::new();

        for deposit in &self.events.deposits {
            match deposit {
                L1Deposit::Regular(_) => queued_deposits.push(deposit.to_abi_queued_deposit()),
                L1Deposit::Encrypted(d) => {
                    let queued = deposit.to_abi_queued_deposit();
                    let sequencer_key = sequencer_keys.key_at(d.key_index)?;

                    // Attempt full ECIES decryption.
                    let dec = ecies::decrypt_deposit(
                        sequencer_key,
                        &d.ephemeral_pubkey_x,
                        d.ephemeral_pubkey_y_parity,
                        &d.ciphertext,
                        &d.nonce,
                        &d.tag,
                        portal_address,
                        d.key_index,
                    );

                    if let Some(dec) = dec {
                        debug!(
                            target: "zone::engine",
                            l1_block = l1_block_number,
                            sender = %d.sender,
                            recipient = %dec.to,
                            token = %d.token,
                            amount = %d.amount,
                            "Decrypted encrypted deposit"
                        );

                        let decryption = abi::DecryptionData {
                            sharedSecret: dec.proof.shared_secret,
                            sharedSecretYParity: dec.proof.shared_secret_y_parity,
                            cpProof: abi::ChaumPedersenProof {
                                s: dec.proof.cp_proof_s,
                                c: dec.proof.cp_proof_c,
                            },
                        };
                        queued_deposits.push(queued);
                        decryptions.push(decryption);
                        continue;
                    }

                    // Full decryption failed — try ECDH proof for on-chain refund.
                    let proof = ecies::compute_ecdh_proof(
                        sequencer_key,
                        &d.ephemeral_pubkey_x,
                        d.ephemeral_pubkey_y_parity,
                    );

                    if let Some(proof) = proof {
                        warn!(
                            target: "zone::payload",
                            sender = %d.sender,
                            amount = %d.amount,
                            "Encrypted deposit decryption failed, providing valid proof for on-chain refund"
                        );
                        let decryption = abi::DecryptionData {
                            sharedSecret: proof.shared_secret,
                            sharedSecretYParity: proof.shared_secret_y_parity,
                            cpProof: abi::ChaumPedersenProof {
                                s: proof.cp_proof_s,
                                c: proof.cp_proof_c,
                            },
                        };
                        queued_deposits.push(queued);
                        decryptions.push(decryption);
                        continue;
                    }

                    warn!(
                        target: "zone::payload",
                        sender = %d.sender,
                        amount = %d.amount,
                        "Encrypted deposit has invalid ephemeral pubkey, using zeroed DecryptionData"
                    );
                    let decryption = abi::DecryptionData {
                        sharedSecret: B256::ZERO,
                        sharedSecretYParity: 0x02,
                        cpProof: abi::ChaumPedersenProof {
                            s: B256::ZERO,
                            c: B256::ZERO,
                        },
                    };
                    queued_deposits.push(queued);
                    decryptions.push(decryption);
                }
            }
        }

        let enabled_tokens: Vec<_> = self
            .events
            .enabled_tokens
            .iter()
            .map(|t| t.to_abi())
            .collect();

        let elapsed = start.elapsed();
        info!(
            target: "zone::engine",
            l1_block = l1_block_number,
            total_deposits,
            encrypted = decryptions.len(),
            enabled_tokens = enabled_tokens.len(),
            ?elapsed,
            "Prepared L1 block deposits"
        );

        Ok(PreparedL1Block {
            header: self.header,
            queued_deposits,
            decryptions,
            enabled_tokens,
        })
    }
}

/// An L1 block with deposits fully prepared for the payload builder.
///
/// All ECIES decryption and ABI encoding have been performed.
/// The builder only needs to RLP-encode the header and assemble the `advanceTempo` calldata.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PreparedL1Block {
    /// The sealed L1 block header.
    pub header: SealedHeader<TempoHeader>,
    /// ABI-encoded queued deposits (regular + encrypted).
    #[serde(skip)]
    pub queued_deposits: Vec<abi::QueuedDeposit>,
    /// Decryption data for every encrypted deposit submitted for on-chain verification, in order.
    #[serde(skip)]
    pub decryptions: Vec<abi::DecryptionData>,
    /// Tokens newly enabled for bridging in this block.
    #[serde(skip)]
    pub enabled_tokens: Vec<abi::EnabledToken>,
}
