use super::*;
use zone_primitives::constants::MAX_UNPROCESSED_DEPOSITS;

/// An L1 block's header paired with the deposits found in that block.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct L1BlockDeposits {
    /// The sealed L1 block header (caches the block hash).
    pub header: SealedHeader<TempoHeader>,
    /// Portal events extracted from this block.
    pub events: L1PortalEvents,
}

/// A contiguous L1 range with portal events fully prepared for the payload builder.
///
/// All ECIES decryption and ABI encoding have been performed. The builder only needs to
/// RLP-encode the headers and assemble the `advanceTempo` calldata.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PreparedL1BlockRange {
    /// Consecutive sealed L1 headers imported by this Zone advance. The final entry is the
    /// checkpoint persisted by TempoState and the root used for all L1 reads in the call.
    pub headers: Vec<SealedHeader<TempoHeader>>,
    /// ABI-encoded user deposits and internal withdrawal bounce-backs.
    #[serde(skip)]
    pub queued_deposits: Vec<abi::QueuedDeposit>,
    /// Decryption data for every user deposit submitted for on-chain verification, in order.
    #[serde(skip)]
    pub decryptions: Vec<abi::DecryptionData>,
    /// Tokens newly enabled for bridging across this range.
    #[serde(skip)]
    pub enabled_tokens: Vec<abi::EnabledToken>,
}

impl PreparedL1BlockRange {
    /// Prepare a contiguous range of finalized L1 blocks for one atomic Zone advance.
    ///
    /// Deposits, token enables, and decryption data retain their canonical cross-block order;
    /// only the final header is used as the Tempo state anchor by the Zone precompile.
    pub fn new(
        blocks: &[Arc<L1BlockDeposits>],
        encryption_keys: &EncryptionKeyRing,
        portal_address: Address,
    ) -> eyre::Result<Self> {
        use crate::precompiles::ecies;

        let start = std::time::Instant::now();
        let l1_block_range = blocks
            .first()
            .ok_or_else(|| eyre::eyre!("cannot prepare an empty L1 header range"))?
            ..=blocks.last().unwrap();
        let mut total_deposits = 0;
        let mut enabled_tokens = Vec::new();
        let mut headers = Vec::with_capacity(blocks.len());
        for block in blocks {
            headers.push(block.header.clone());
            total_deposits += block.events.deposits.len();
            enabled_tokens.extend(
                block
                    .events
                    .enabled_tokens
                    .iter()
                    .map(|token| token.to_abi()),
            );
        }
        eyre::ensure!(
            total_deposits <= MAX_UNPROCESSED_DEPOSITS,
            "prepared L1 range contains {total_deposits} deposits, exceeding the global outstanding cap {MAX_UNPROCESSED_DEPOSITS}"
        );

        let mut decryptions = Vec::with_capacity(total_deposits);
        let mut queued_deposits = Vec::with_capacity(total_deposits);
        let deposits = blocks.iter().flat_map(|block| {
            let block_number = block.header.inner.number;
            block
                .events
                .deposits
                .iter()
                .map(move |deposit| (block_number, deposit))
        });
        for (l1_block_number, deposit) in deposits {
            match deposit {
                L1Deposit::WithdrawalBounceBack(_) => {
                    queued_deposits.push(deposit.to_abi_queued_deposit())
                }
                L1Deposit::Deposit(d) => {
                    let queued = deposit.to_abi_queued_deposit();
                    let decryption_key = encryption_keys.key(d.key_index)?;

                    // Attempt full ECIES decryption.
                    let dec = ecies::decrypt_deposit(
                        &decryption_key,
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
                            "Decrypted deposit"
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
                        &decryption_key,
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

        let elapsed = start.elapsed();
        info!(
            target: "zone::engine",
            l1_block = l1_block_range.start().header.inner.number,
            l1_block_to = l1_block_range.end().header.inner.number,
            headers = blocks.len(),
            total_deposits,
            encrypted = decryptions.len(),
            enabled_tokens = enabled_tokens.len(),
            ?elapsed,
            "Prepared L1 range portal events"
        );

        Ok(Self {
            headers,
            queued_deposits,
            decryptions,
            enabled_tokens,
        })
    }
}

/// Validate the non-consensus hash-chain continuity of an imported L1 header range.
///
/// Consensus execution performs its own checkpoint validation at the native `TempoState`
/// boundary. This helper is for node-side payload and replication checks that should agree on the
/// basic range shape before execution. Returns the final validated checkpoint.
pub fn validate_l1_headers(
    headers: &[SealedHeader<TempoHeader>],
    checkpoint: NumHash,
) -> eyre::Result<NumHash> {
    eyre::ensure!(
        !headers.is_empty(),
        "advanceTempo contains no Tempo headers"
    );
    headers.iter().try_fold(checkpoint, |previous, header| {
        let block_number = header.number();
        let expected = previous
            .number
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("L1 block overflow"))?;
        eyre::ensure!(
            block_number == expected,
            "L1 header range has a gap at block {block_number}, expected {expected}"
        );
        eyre::ensure!(
            header.parent_hash() == previous.hash,
            "L1 header {block_number} does not extend the previous checkpoint: embedded parent {}, previous hash {}",
            header.parent_hash(),
            previous.hash
        );
        Ok(NumHash::new(block_number, header.hash()))
    })
}
