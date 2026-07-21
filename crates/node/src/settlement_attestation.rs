//! Batch-boundary settlement attestation construction and leader-side proposal recovery.

use std::time::Duration;

use alloy_consensus::TxReceipt as _;
use alloy_eips::BlockHashOrNumber;
use alloy_primitives::{B256, Bytes, Sealable as _, U256};
use alloy_provider::Provider as _;
use alloy_sol_types::{SolEvent as _, SolValue as _};
use futures::{StreamExt as _, TryStreamExt as _};
use reth_chain_state::PersistedBlockSubscriptions;
use reth_provider::HeaderProvider;
use reth_storage_api::{BlockNumReader, ReceiptProvider};
use tempo_primitives::TempoHeader;
use tempo_zone_contracts::{
    ZONE_INBOX_ADDRESS, ZONE_OUTBOX_ADDRESS, ZoneInbox, ZoneOutbox, ZonePortal,
};
use tokio::sync::mpsc;
use tracing::{debug, info};
use zone_p2p::P2pCommand;

use crate::{replication::AttestationContext, withdrawal_checker::WithdrawalCheckDecision};
use zone_sequencer::{
    BatchAnchorConfig,
    attestation::{SettlementAttestation, SignedSettlementAttestation},
};

const L1_ANCESTRY_FETCH_CONCURRENCY: usize = 16;
const L1_ANCESTRY_VALIDATION_CHUNK_SIZE: u64 = 1_024;
const MAX_L1_ANCESTRY_HEADERS: u64 = 262_144;

fn validate_l1_anchor_bounds(
    config: BatchAnchorConfig,
    tempo_block_number: u64,
    anchor_block_number: u64,
    certified_head: u64,
) -> eyre::Result<()> {
    eyre::ensure!(
        anchor_block_number >= tempo_block_number,
        "proposed L1 anchor predates the zone batch's Tempo block"
    );
    eyre::ensure!(
        anchor_block_number < certified_head,
        "proposed L1 anchor is not below the certified L1 head"
    );
    eyre::ensure!(
        certified_head.saturating_sub(anchor_block_number) < config.history_window(),
        "proposed L1 anchor is outside the EIP-2935 history window"
    );
    let ancestry_len = anchor_block_number
        .checked_sub(tempo_block_number)
        .ok_or_else(|| eyre::eyre!("proposed L1 anchor predates the Tempo block"))?;
    eyre::ensure!(
        ancestry_len <= MAX_L1_ANCESTRY_HEADERS,
        "proposed L1 ancestry has {ancestry_len} headers, exceeding the supported maximum of {MAX_L1_ANCESTRY_HEADERS}"
    );
    Ok(())
}

#[derive(Debug)]
pub(crate) struct BuiltSettlementAttestation {
    pub(crate) attestation: SettlementAttestation,
    pub(crate) target: alloy_eips::BlockNumHash,
}

#[derive(Debug, Clone, Copy)]
struct BlockCommitments {
    tempo_block_hash: B256,
    tempo_block_number: u64,
    processed_deposit_hash: B256,
    processed_deposit_number: u64,
    withdrawal: Option<(B256, u64)>,
}

#[derive(Debug, Clone, Copy)]
enum ProposalStatus {
    Skipped,
    Pending,
    Halted,
}

enum ProposalScan {
    Complete,
    Pending(u64),
    Halted,
}

/// Extract commitments produced by the deterministic system transactions in a zone block.
fn block_commitments<P>(provider: &P, number: u64) -> eyre::Result<BlockCommitments>
where
    P: ReceiptProvider,
{
    let receipts = provider
        .receipts_by_block(BlockHashOrNumber::Number(number))?
        .ok_or_else(|| eyre::eyre!("receipts for canonical block {number} are not persisted"))?;
    let mut anchor_hash = None;
    let mut tempo_block_number = None;
    let mut processed_deposit_hash = None;
    let mut processed_deposit_number = None;
    let mut withdrawal = None;

    for receipt in receipts {
        for log in receipt.logs() {
            if log.address == ZONE_INBOX_ADDRESS
                && log.topics().first() == Some(&ZoneInbox::TempoAdvanced::SIGNATURE_HASH)
            {
                let event = ZoneInbox::TempoAdvanced::decode_log(log).map_err(|err| {
                    eyre::eyre!("invalid TempoAdvanced log in block {number}: {err}")
                })?;
                anchor_hash = Some(event.tempoBlockHash);
                tempo_block_number = Some(event.tempoBlockNumber);
                processed_deposit_hash = Some(event.newProcessedDepositQueueHash);
                processed_deposit_number = Some(event.lastProcessedDepositNumber);
            } else if log.address == ZONE_OUTBOX_ADDRESS
                && log.topics().first() == Some(&ZoneOutbox::BatchFinalized::SIGNATURE_HASH)
            {
                let event = ZoneOutbox::BatchFinalized::decode_log(log).map_err(|err| {
                    eyre::eyre!("invalid BatchFinalized log in block {number}: {err}")
                })?;
                withdrawal = Some((event.withdrawalQueueHash, event.withdrawalBatchIndex));
            }
        }
    }

    Ok(BlockCommitments {
        tempo_block_hash: anchor_hash
            .ok_or_else(|| eyre::eyre!("block {number} is missing TempoAdvanced"))?,
        tempo_block_number: tempo_block_number
            .ok_or_else(|| eyre::eyre!("block {number} is missing its Tempo block number"))?,
        processed_deposit_hash: processed_deposit_hash
            .ok_or_else(|| eyre::eyre!("block {number} is missing its deposit commitment"))?,
        processed_deposit_number: processed_deposit_number
            .ok_or_else(|| eyre::eyre!("block {number} is missing its deposit number"))?,
        withdrawal,
    })
}

/// Get the previous batch's (i.e the last block in the previous batch) block_hash,
/// deposit_hash and processed deposit number. These values
/// are used to identify the previous batch while submitting the current batch.
fn previous_batch<P>(provider: &P, number: u64) -> eyre::Result<Option<(B256, B256, u64)>>
where
    P: HeaderProvider<Header = TempoHeader> + ReceiptProvider,
{
    for candidate in (1..number).rev() {
        let commitments = block_commitments(provider, candidate)?;
        if commitments.withdrawal.is_some() {
            let hash = provider
                .sealed_header(candidate)?
                .map(|header| header.hash())
                .ok_or_else(|| eyre::eyre!("missing prior batch-boundary header {candidate}"))?;
            return Ok(Some((
                hash,
                commitments.processed_deposit_hash,
                commitments.processed_deposit_number,
            )));
        }
    }
    Ok(None)
}

/// Build the settlement attestation at a batch boundary in the exact format ZonePortal expects.
pub(crate) async fn build_settlement_attestation<P>(
    provider: &P,
    number: u64,
    context: &AttestationContext,
    proposed_anchor: Option<(u64, B256)>,
) -> eyre::Result<Option<BuiltSettlementAttestation>>
where
    P: HeaderProvider<Header = TempoHeader> + ReceiptProvider,
{
    let commitments = block_commitments(provider, number)?;
    let Some((withdrawal_queue_hash, withdrawal_batch_index)) = commitments.withdrawal else {
        return Ok(None);
    };
    let next_tip = provider
        .sealed_header(number)?
        .ok_or_else(|| eyre::eyre!("missing batch-tip header {number}"))?
        .hash();
    let previous = previous_batch(provider, number)?;

    let portal = ZonePortal::new(context.domain.portal_address, context.l1_provider.clone());
    let set_version_call = portal.sequencerSetVersion();
    let portal_batch_index_call = portal.withdrawalBatchIndex();
    let sequencer_call = portal.sequencer();
    let verifier_call = portal.verifier();
    let portal_tip_call = portal.blockHash();
    let (set_version, portal_batch_index, sequencer, verifier, portal_tip) = tokio::try_join!(
        set_version_call.call(),
        portal_batch_index_call.call(),
        sequencer_call.call(),
        verifier_call.call(),
        portal_tip_call.call(),
    )?;
    eyre::ensure!(
        set_version == context.domain.sequencer_set_version,
        "portal signer-set version {set_version} does not match manifest version {}",
        context.domain.sequencer_set_version
    );
    // Before the first local batch boundary, extend the Portal's configured genesis tip. Later
    // boundaries must extend the prior local boundary and are checked against the same Portal tip.
    let (previous_tip, previous_deposit_hash, previous_deposit_number) =
        previous.unwrap_or((portal_tip, B256::ZERO, 0));
    eyre::ensure!(
        portal_tip == previous_tip,
        "proposal does not extend the portal batch tip"
    );
    eyre::ensure!(
        withdrawal_batch_index == portal_batch_index.saturating_add(1),
        "zone withdrawal batch index {withdrawal_batch_index} does not follow portal index {portal_batch_index}"
    );

    let (anchor_block_number, anchor_block_hash) = if let Some(anchor) = proposed_anchor {
        anchor
    } else {
        let l1_tip = context.l1_provider.get_block_number().await?;
        let gap = l1_tip.saturating_sub(commitments.tempo_block_number);
        if gap < context.anchor_config.effective_window() {
            (commitments.tempo_block_number, commitments.tempo_block_hash)
        } else {
            let anchor_number = l1_tip.saturating_sub(context.anchor_config.safety_margin());
            let header = context
                .l1_provider
                .get_header_by_number(anchor_number.into())
                .await?
                .ok_or_else(|| eyre::eyre!("missing L1 anchor header {anchor_number}"))?
                .inner
                .inner;
            (anchor_number, header.hash_slow())
        }
    };
    validate_settlement_anchor(
        context,
        commitments.tempo_block_number,
        commitments.tempo_block_hash,
        anchor_block_number,
        anchor_block_hash,
    )
    .await?;

    let attestation = SettlementAttestation {
        zoneId: context.domain.zone_id,
        sequencerSetVersion: set_version,
        zoneHeight: U256::from(number),
        withdrawalBatchIndex: U256::from(withdrawal_batch_index),
        sequencer,
        verifier,
        tempoBlockNumber: commitments.tempo_block_number,
        anchorBlockNumber: anchor_block_number,
        anchorBlockHash: anchor_block_hash,
        blockTransitionHash: alloy_primitives::keccak256((previous_tip, next_tip).abi_encode()),
        depositQueueTransitionHash: alloy_primitives::keccak256(
            (
                previous_deposit_hash,
                commitments.processed_deposit_hash,
                previous_deposit_number,
                commitments.processed_deposit_number,
            )
                .abi_encode(),
        ),
        withdrawalQueueHash: withdrawal_queue_hash,
        verifierConfigHash: alloy_primitives::keccak256(Bytes::new()),
    };
    Ok(Some(BuiltSettlementAttestation {
        attestation,
        target: alloy_eips::BlockNumHash {
            number,
            hash: next_tip,
        },
    }))
}

/// Verify that the proposed anchor is the same finalized Tempo chain observed by this node
/// before signing the attestation.
async fn validate_settlement_anchor(
    context: &AttestationContext,
    tempo_block_number: u64,
    tempo_block_hash: B256,
    anchor_block_number: u64,
    anchor_block_hash: B256,
) -> eyre::Result<()> {
    let certified_head = context.l1_provider.get_block_number().await?;
    validate_l1_anchor_bounds(
        context.anchor_config,
        tempo_block_number,
        anchor_block_number,
        certified_head,
    )?;
    if context.l1_anchor_is_validated(
        tempo_block_number,
        tempo_block_hash,
        anchor_block_number,
        anchor_block_hash,
    ) {
        return Ok(());
    }

    let mut parent_hash = tempo_block_hash;
    let mut chunk_start = tempo_block_number;
    loop {
        let chunk_end = chunk_start
            .saturating_add(L1_ANCESTRY_VALIDATION_CHUNK_SIZE - 1)
            .min(anchor_block_number);
        let mut resolved = context.cached_l1_headers(chunk_start, chunk_end);
        let missing = (chunk_start..=chunk_end)
            .filter(|block_number| !resolved.contains_key(block_number))
            .collect::<Vec<_>>();
        let mut headers = futures::stream::iter(missing)
            .map(|block_number| async move {
                let header = context
                    .l1_provider
                    .get_header_by_number(block_number.into())
                    .await?
                    .ok_or_else(|| eyre::eyre!("missing L1 ancestry header {block_number}"))?
                    .inner
                    .inner;
                Ok::<_, eyre::Report>((
                    block_number,
                    (header.inner.parent_hash, header.hash_slow()),
                ))
            })
            .buffered(L1_ANCESTRY_FETCH_CONCURRENCY);
        while let Some((block_number, header)) = headers.try_next().await? {
            resolved.insert(block_number, header);
        }

        for block_number in chunk_start..=chunk_end {
            let (header_parent, header_hash) = resolved
                .get(&block_number)
                .copied()
                .ok_or_else(|| eyre::eyre!("missing resolved L1 ancestry header {block_number}"))?;
            if block_number == tempo_block_number {
                eyre::ensure!(
                    header_hash == tempo_block_hash,
                    "zone batch's Tempo block hash does not match finalized L1"
                );
            } else {
                eyre::ensure!(
                    header_parent == parent_hash,
                    "L1 ancestry is broken at block {block_number}"
                );
                parent_hash = header_hash;
            }
        }
        context.cache_validated_l1_headers(
            resolved
                .into_iter()
                .map(|(number, (parent_hash, hash))| (number, parent_hash, hash)),
        )?;
        if chunk_end == anchor_block_number {
            break;
        }
        chunk_start = chunk_end
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("L1 ancestry chunk boundary overflow"))?;
    }
    eyre::ensure!(
        parent_hash == anchor_block_hash,
        "proposed L1 anchor is not descended from the zone batch's Tempo block"
    );
    context.cache_validated_l1_anchor(
        tempo_block_number,
        tempo_block_hash,
        anchor_block_number,
        anchor_block_hash,
    );
    Ok(())
}

async fn scan_settlement_boundaries<P>(
    provider: &P,
    commands: &mpsc::Sender<P2pCommand>,
    context: &AttestationContext,
    start: u64,
    end: u64,
    settled_through: Option<U256>,
) -> ProposalScan
where
    P: HeaderProvider<Header = TempoHeader> + ReceiptProvider,
{
    for number in start..=end {
        match propose_settlement(provider, number, commands, context).await {
            Ok(ProposalStatus::Pending) => return ProposalScan::Pending(number),
            Ok(ProposalStatus::Skipped) => {}
            Ok(ProposalStatus::Halted) => return ProposalScan::Halted,
            Err(error) if settled_through.is_some_and(|height| height >= U256::from(number)) => {
                debug!(target: "zone::p2p", %error, height = number, "Skipped already-settled boundary while advancing");
            }
            Err(error) => {
                tracing::warn!(target: "zone::p2p", %error, height = number, "Settlement boundary is temporarily unavailable; retaining it for retry");
                return ProposalScan::Pending(number);
            }
        }
    }
    ProposalScan::Complete
}

/// Long-running async task that detects persisted batch boundaries and broadcasts settlement
/// proposals to followers. At each boundary, it signs the proposal locally and initiates follower
/// attestation collection; follower responses are received by the P2P sync task and inserted into
/// the shared attestation store.
pub(crate) async fn collect_leader_settlements<P>(
    provider: P,
    commands: mpsc::Sender<P2pCommand>,
    context: AttestationContext,
) where
    P: PersistedBlockSubscriptions
        + BlockNumReader
        + HeaderProvider<Header = TempoHeader>
        + ReceiptProvider
        + Clone
        + Send
        + Sync
        + 'static,
{
    // Subscribe before taking the startup snapshot so a block persisted during reconciliation is
    // either included in the snapshot or delivered by the stream.
    let mut persisted = provider.persisted_block_stream();

    // Reconstruct only the unresolved suffix after the Portal's last quorum-certified zone
    // height. Portal-tip validation inside the builder selects the first still-current boundary.
    let head = match provider.best_block_number() {
        Ok(head) => head,
        Err(err) => {
            tracing::error!(target: "zone::p2p", %err, "Failed reading head for settlement recovery");
            return;
        }
    };
    let portal = ZonePortal::new(context.domain.portal_address, context.l1_provider.clone());
    let portal_height_call = portal.zoneHeight();
    let portal_tip_call = portal.blockHash();
    let portal_batch_index_call = portal.withdrawalBatchIndex();
    let (portal_height, portal_tip, portal_batch_index) = match tokio::try_join!(
        portal_height_call.call(),
        portal_tip_call.call(),
        portal_batch_index_call.call(),
    ) {
        Ok(state) => state,
        Err(err) => {
            tracing::error!(target: "zone::p2p", %err, "Failed reading Portal state for settlement recovery");
            return;
        }
    };
    let portal_height = match u64::try_from(portal_height) {
        Ok(height) => height,
        Err(_) => {
            tracing::error!(target: "zone::p2p", %portal_height, "Portal zone height does not fit in u64");
            return;
        }
    };
    let submitted_height = if portal_height != 0 {
        match provider.sealed_header(portal_height) {
            Ok(Some(header)) if header.hash() == portal_tip => portal_height,
            Ok(Some(header)) => {
                tracing::error!(target: "zone::p2p", portal_height, local = %header.hash(), %portal_tip, "Portal zone height does not match local canonical state");
                return;
            }
            Ok(None) => {
                tracing::error!(target: "zone::p2p", portal_height, "Portal zone height is missing locally");
                return;
            }
            Err(err) => {
                tracing::error!(target: "zone::p2p", %err, portal_height, "Failed reading the Portal zone height locally");
                return;
            }
        }
    } else if portal_batch_index == 0 {
        // Before the first batch, `blockHash` is an arbitrary configured genesis commitment and
        // need not identify a local zone header.
        0
    } else {
        match provider.header(portal_tip) {
            Ok(Some(header)) => header.inner.number,
            Ok(None) => {
                tracing::error!(target: "zone::p2p", %portal_tip, "Portal tip is missing locally");
                return;
            }
            Err(err) => {
                tracing::error!(target: "zone::p2p", %err, %portal_tip, "Failed locating the Portal tip locally");
                return;
            }
        }
    };
    let start = submitted_height.saturating_add(1);
    let mut last_scanned = head;
    let mut pending_boundary =
        match scan_settlement_boundaries(&provider, &commands, &context, start, head, None).await {
            ProposalScan::Complete => None,
            ProposalScan::Pending(number) => {
                last_scanned = number;
                Some(number)
            }
            ProposalScan::Halted => return,
        };

    let mut retry = tokio::time::interval(Duration::from_secs(5));
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            tip = persisted.next() => {
                let Some(tip) = tip else { return };
                if pending_boundary.is_none() && tip.number > last_scanned {
                    match scan_settlement_boundaries(
                        &provider,
                        &commands,
                        &context,
                        last_scanned.saturating_add(1),
                        tip.number,
                        None,
                    ).await {
                        ProposalScan::Complete => last_scanned = tip.number,
                        ProposalScan::Pending(number) => {
                            last_scanned = number;
                            pending_boundary = Some(number);
                        }
                        ProposalScan::Halted => return,
                    }
                }
            }
            _ = retry.tick(), if pending_boundary.is_some() => {
                let number = pending_boundary.expect("guarded by is_some");
                match propose_settlement(&provider, number, &commands, &context).await {
                    Ok(ProposalStatus::Pending) => {}
                    Ok(ProposalStatus::Skipped) => {
                        let head = match provider.best_block_number() {
                            Ok(head) => head,
                            Err(err) => {
                                debug!(target: "zone::p2p", %err, "Failed reading head after skipped settlement boundary");
                                continue;
                            }
                        };
                        match scan_settlement_boundaries(
                            &provider,
                            &commands,
                            &context,
                            number.saturating_add(1),
                            head,
                            None,
                        ).await {
                            ProposalScan::Complete => {
                                last_scanned = head;
                                pending_boundary = None;
                            }
                            ProposalScan::Pending(next) => {
                                last_scanned = next;
                                pending_boundary = Some(next);
                            }
                            ProposalScan::Halted => return,
                        }
                    }
                    Ok(ProposalStatus::Halted) => return,
                    Err(err) => {
                        debug!(target: "zone::p2p", %err, height = number, "Settlement proposal retry is not currently valid");

                        let portal_height = match portal.zoneHeight().call().await {
                            Ok(height) => height,
                            Err(err) => {
                                debug!(target: "zone::p2p", %err, "Failed reading Portal height while advancing settlement proposal");
                                continue;
                            }
                        };
                        if portal_height < U256::from(number) {
                            // The Portal has not accepted this proposal. Treat the failure as
                            // transient and retry only this boundary without rescanning the suffix.
                            continue;
                        }

                        // A successful submitBatch makes the previously pending proposal stale.
                        // Walk the already-persisted boundaries after it so the next batch can be
                        // proposed even when the live tip is now far ahead of the portal tip.
                        let head = match provider.best_block_number() {
                            Ok(head) => head,
                            Err(err) => {
                                debug!(target: "zone::p2p", %err, "Failed reading head while advancing settlement proposal");
                                continue;
                            }
                        };
                        match scan_settlement_boundaries(
                            &provider,
                            &commands,
                            &context,
                            number.saturating_add(1),
                            head,
                            Some(portal_height),
                        ).await {
                            ProposalScan::Complete => {
                                last_scanned = head;
                                pending_boundary = None;
                            }
                            ProposalScan::Pending(next) => {
                                last_scanned = next;
                                pending_boundary = Some(next);
                            }
                            ProposalScan::Halted => return,
                        }
                    }
                }
            }
        }
    }
}

/// Before we settle on L1 with `submitBatch`, we need to collect follower signatures for this
/// batch. Create a settlement proposal and send it to followers, who will sign and return a
/// SettlementAttestation that will be sent along with the submitBatch for the zoneportal's
/// on-chain quorum.
async fn propose_settlement<P>(
    provider: &P,
    number: u64,
    commands: &mpsc::Sender<P2pCommand>,
    context: &AttestationContext,
) -> eyre::Result<ProposalStatus>
where
    P: HeaderProvider<Header = TempoHeader> + ReceiptProvider,
{
    let Some(built) = build_settlement_attestation(provider, number, context, None).await? else {
        return Ok(ProposalStatus::Skipped);
    };
    let decision = context.withdrawal_checker.decide(built.target).await;
    match decision {
        WithdrawalCheckDecision::Submit => {}
        WithdrawalCheckDecision::Retry(error) => {
            error.emit_withheld();
            debug!(target: "zone::p2p", %error, height = number, "Settlement proposal validation is temporarily unavailable");
            return Ok(ProposalStatus::Pending);
        }
        WithdrawalCheckDecision::Halt(error) => {
            error.emit_withheld();
            tracing::error!(target: "zone::p2p", %error, height = number, "Settlement proposal validation halted");
            return Ok(ProposalStatus::Halted);
        }
    }
    if let Some(proposal) = context.store.settlement_proposal(built.target) {
        commands
            .send(P2pCommand::BroadcastSettlementProposal(proposal.encode()))
            .await
            .map_err(|_| eyre::eyre!("P2P command channel closed"))?;
        debug!(target: "zone::p2p", height = number, "Rebroadcast settlement proposal");
        return Ok(ProposalStatus::Pending);
    }
    let signed = SignedSettlementAttestation::sign(
        built.attestation.clone(),
        context.domain,
        &context.signer,
    )?;
    let signer = signed.recover_signer(context.domain)?;
    context
        .store
        .register_settlement(context.domain, signer, built.target, signed)?;
    commands
        .send(P2pCommand::BroadcastSettlementProposal(
            built.attestation.encode(),
        ))
        .await
        .map_err(|_| eyre::eyre!("P2P command channel closed"))?;
    info!(target: "zone::p2p", height = number, %signer, "Signed and broadcast settlement proposal");
    Ok(ProposalStatus::Pending)
}

#[cfg(test)]
mod tests {
    use super::{MAX_L1_ANCESTRY_HEADERS, validate_l1_anchor_bounds};
    use zone_sequencer::BatchAnchorConfig;

    #[test]
    fn proposed_l1_anchor_is_bounded_before_ancestry_iteration() {
        let config = BatchAnchorConfig::new(100, 10).unwrap();
        assert!(validate_l1_anchor_bounds(config, 100, 100, 150).is_ok());
        assert!(validate_l1_anchor_bounds(config, 50, 100, 150).is_ok());

        assert!(validate_l1_anchor_bounds(config, 100, u64::MAX, 150).is_err());
        assert!(validate_l1_anchor_bounds(config, 100, 150, 150).is_err());
        assert!(validate_l1_anchor_bounds(config, 100, 100, 201).is_err());
        assert!(
            validate_l1_anchor_bounds(
                BatchAnchorConfig::default(),
                0,
                MAX_L1_ANCESTRY_HEADERS + 1,
                MAX_L1_ANCESTRY_HEADERS + 2,
            )
            .is_err()
        );
    }
}
