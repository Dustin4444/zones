use super::*;

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
        sequencer_key: &k256::SecretKey,
        portal_address: Address,
    ) -> eyre::Result<PreparedL1Block> {
        self.prepare_for_build(sequencer_key, portal_address)
            .await
            .map(|(block, _)| block)
    }

    /// Prepare deposits and retain the event-derived L1 reads needed during their execution.
    ///
    /// Leaders use the returned plan to populate the shared exact-block L1 cache before triggering
    /// payload construction. Keeping the plan separate leaves [`PreparedL1Block`] and its Engine
    /// API encoding unchanged.
    pub async fn prepare_for_build(
        self,
        sequencer_key: &k256::SecretKey,
        portal_address: Address,
    ) -> eyre::Result<(PreparedL1Block, crate::state::DepositPrefetchPlan)> {
        use crate::precompiles::ecies;

        let start = std::time::Instant::now();
        let l1_block_number = self.header.inner.number;
        let total_deposits = self.events.deposits.len();
        let mut queued_deposits: Vec<abi::QueuedDeposit> = Vec::new();
        let mut decryptions: Vec<abi::DecryptionData> = Vec::new();
        let mut prefetch = crate::state::DepositPrefetchPlan::new(l1_block_number, portal_address);

        for l1_deposit in &self.events.deposits {
            match l1_deposit {
                L1Deposit::Regular(deposit) => {
                    if deposit.tempo_refund_recipient.is_zero() {
                        // The effective withdrawal bounce-back recipient comes from Zone-local
                        // Outbox state, but its token policy remains event-derived.
                        prefetch.add_token(deposit.token);
                    } else {
                        prefetch.add_mint(deposit.token, deposit.to);
                    }
                    queued_deposits.push(l1_deposit.to_abi_queued_deposit());
                }
                L1Deposit::Encrypted(d) => {
                    let queued = l1_deposit.to_abi_queued_deposit();
                    prefetch.add_encryption_key(d.key_index);

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
                        prefetch.add_mint(d.token, dec.to);
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
            .map(|token| {
                prefetch.add_token(token.token);
                token.to_abi()
            })
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

        Ok((
            PreparedL1Block {
                header: self.header,
                queued_deposits,
                decryptions,
                enabled_tokens,
            },
            prefetch,
        ))
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
