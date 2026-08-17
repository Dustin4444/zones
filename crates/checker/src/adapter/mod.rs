//! Converts authenticated observations into independent checker facts and effects.

use std::num::NonZeroU64;

use alloy_consensus::BlockHeader as _;
use alloy_primitives::{B256, FixedBytes};

use crate::{
    failure::Failure,
    kernel::{
        BatchSubmission, BounceBackDeposit, Cursor, DepositOutcome, DepositPayload, Effect,
        ExpectedState, ImportedFacts, OrdinaryDeposit, PortalIdentity, RefundClaim, TokenEnable,
        Withdrawal, WithdrawalOutcome,
    },
    observe::{
        L1BlockObservation, L2BlockObservation, ZonePostStateOutputs,
        events::{Factory, Inbox, Portal},
    },
    persistence::BlockNumHash,
    runtime::{AuthenticatedBlock, AuthenticatedOutputs},
};

mod tempo;
mod zone;

/// Authenticated L1, L2, and post-state inputs for one Zone block.
pub(crate) struct AuthenticatedObservation {
    pub l2: L2BlockObservation,
    pub l1: Vec<L1BlockObservation>,
    pub state: ZonePostStateOutputs,
    pub portal_creation_block_hash: B256,
    pub zone_id: u32,
}

/// Facts and effects derived from one authenticated Tempo block.
pub(crate) struct ImportedFactsAndEffects {
    pub(crate) facts: ImportedFacts,
    pub(crate) effects: Vec<Effect>,
}

/// Facts and effects derived from one authenticated Zone block.
pub(super) struct ZoneFactsAndEffects {
    pub(super) facts: crate::kernel::ZoneFacts,
    pub(super) effects: Vec<Effect>,
}

/// Stable finding codes emitted while adapting authenticated observations.
#[derive(Debug, Clone, Copy)]
#[repr(u16)]
pub(crate) enum AdapterFindingCode {
    HeaderSequence = 100,
    EventSequence = 200,
}

impl AdapterFindingCode {
    /// Build an authenticated-divergence failure for this adapter invariant.
    fn failure(self, message: impl Into<String>) -> Failure {
        Failure::authenticated_divergence(
            message,
            crate::kernel::Finding::coded(
                crate::kernel::FindingCategory::Observation,
                self as u16,
                crate::kernel::FindingLocation::Block,
            ),
        )
    }
}

/// Adapt one authenticated Zone block into kernel inputs and independent outputs.
pub(crate) fn adapt(observation: &AuthenticatedObservation) -> Result<AuthenticatedBlock, Failure> {
    let header = observation.l2.inputs().advance_tempo().imported_header();
    let [tempo_observation] = observation.l1.as_slice() else {
        return Err(AdapterFindingCode::HeaderSequence
            .failure("advanceTempo requires exactly one Tempo observation"));
    };
    let ImportedFactsAndEffects {
        facts: imported_facts,
        effects: mut imported_effects,
    } = adapt_imported(
        tempo_observation,
        header,
        observation.portal_creation_block_hash,
        observation.zone_id,
    )?;
    let ZoneFactsAndEffects {
        facts: zone_facts,
        effects: mut zone_effects,
    } = zone::adapt(observation)?;
    imported_effects.append(&mut zone_effects);
    let state = ExpectedState {
        tempo_block_hash: observation.state.tempo_block_hash,
        tempo_block_number: observation.state.tempo_block_number,
        processed_deposit_hash: observation.state.processed_deposit_queue_hash,
        processed_deposit_number: observation.state.processed_deposit_number,
        withdrawal_queue_hash: observation.state.withdrawal_queue_hash,
        withdrawal_batch_index: observation.state.withdrawal_batch_index,
    };
    Ok(AuthenticatedBlock {
        zone: BlockNumHash {
            number: observation.l2.block_number(),
            hash: observation.l2.block_hash(),
        },
        parent: BlockNumHash {
            number: observation
                .l2
                .block_number()
                .checked_sub(1)
                .ok_or_else(|| {
                    AdapterFindingCode::HeaderSequence.failure("Zone genesis has no parent")
                })?,
            hash: observation.l2.parent_hash(),
        },
        tempo: BlockNumHash {
            number: tempo_observation.block_number(),
            hash: tempo_observation.block_hash(),
        },
        tempo_parent: BlockNumHash {
            number: header.number().checked_sub(1).ok_or_else(|| {
                AdapterFindingCode::HeaderSequence.failure("imported genesis has no parent")
            })?,
            hash: header.header().parent_hash(),
        },
        imported: imported_facts,
        zone_facts,
        outputs: AuthenticatedOutputs {
            effects: imported_effects,
            state,
            supplies: observation.state.token_supplies.clone(),
        },
    })
}

/// Adapt one authenticated imported Tempo block for bootstrap or live checking.
pub(crate) fn adapt_imported(
    observation: &L1BlockObservation,
    header: &crate::observe::ImportedTempoHeader,
    portal_creation_block_hash: B256,
    zone_id: u32,
) -> Result<ImportedFactsAndEffects, Failure> {
    if (observation.block_hash(), observation.block_number()) != (header.hash(), header.number()) {
        return Err(AdapterFindingCode::EventSequence
            .failure("Tempo observation does not match imported header"));
    }
    tempo::adapt(observation, header, portal_creation_block_hash, zone_id)
}

impl From<&Factory::ZoneCreated> for PortalIdentity {
    fn from(event: &Factory::ZoneCreated) -> Self {
        Self {
            portal: event.portal,
            zone_id: event.zoneId,
            initial_token: event.initialToken,
        }
    }
}

impl From<&Portal::TokenEnabled> for TokenEnable {
    fn from(event: &Portal::TokenEnabled) -> Self {
        Self {
            token: event.token,
            name: event.name.clone(),
            symbol: event.symbol.clone(),
            currency: event.currency.clone(),
        }
    }
}

impl From<&Inbox::EnabledToken> for TokenEnable {
    fn from(event: &Inbox::EnabledToken) -> Self {
        Self {
            token: event.token,
            name: event.name.clone(),
            symbol: event.symbol.clone(),
            currency: event.currency.clone(),
        }
    }
}

impl TryFrom<&tempo_zone_contracts::ZonePortal::Deposit> for OrdinaryDeposit {
    type Error = Failure;

    fn try_from(deposit: &tempo_zone_contracts::ZonePortal::Deposit) -> Result<Self, Failure> {
        Ok(Self {
            token: deposit.token,
            sender: deposit.sender,
            amount: deposit.amount,
            tempo_refund_recipient: deposit.tempoRefundRecipient,
            key_index: deposit.keyIndex,
            encrypted: DepositPayload {
                ephemeral_pubkey_x: deposit.encrypted.ephemeralPubkeyX,
                ephemeral_pubkey_y_parity: deposit.encrypted.ephemeralPubkeyYParity,
                ciphertext: ciphertext(deposit.encrypted.ciphertext.as_ref())?,
                nonce: deposit.encrypted.nonce,
                tag: deposit.encrypted.tag,
            },
        })
    }
}

impl TryFrom<&tempo_zone_contracts::ZonePortal::DepositMade> for OrdinaryDeposit {
    type Error = Failure;

    fn try_from(deposit: &tempo_zone_contracts::ZonePortal::DepositMade) -> Result<Self, Failure> {
        Ok(Self {
            token: deposit.token,
            sender: deposit.sender,
            amount: deposit.netAmount,
            tempo_refund_recipient: deposit.tempoRefundRecipient,
            key_index: deposit.keyIndex,
            encrypted: DepositPayload {
                ephemeral_pubkey_x: deposit.ephemeralPubkeyX,
                ephemeral_pubkey_y_parity: deposit.ephemeralPubkeyYParity,
                ciphertext: ciphertext(deposit.ciphertext.as_ref())?,
                nonce: deposit.nonce,
                tag: deposit.tag,
            },
        })
    }
}

impl TryFrom<&Inbox::WithdrawalBounceBackDeposit> for BounceBackDeposit {
    type Error = Failure;

    fn try_from(deposit: &Inbox::WithdrawalBounceBackDeposit) -> Result<Self, Failure> {
        let bytes = deposit.to.as_slice();
        if bytes[..12].iter().any(|byte| *byte != 0) {
            return Err(AdapterFindingCode::EventSequence
                .failure("bounceback recipient has non-canonical high bytes"));
        }
        if deposit.amount == 0 {
            return Err(AdapterFindingCode::EventSequence.failure("zero bounceback amount"));
        }
        let mut nonce_bytes = [0; 8];
        nonce_bytes.copy_from_slice(&bytes[12..]);
        let fallback_nonce = NonZeroU64::new(u64::from_be_bytes(nonce_bytes))
            .ok_or_else(|| AdapterFindingCode::EventSequence.failure("zero bounceback nonce"))?;
        Ok(Self {
            token: deposit.token,
            fallback_nonce,
            amount: deposit.amount,
        })
    }
}

impl From<&Portal::RefundClaimed> for RefundClaim {
    fn from(event: &Portal::RefundClaimed) -> Self {
        Self {
            token: event.token,
            recipient: event.recipient,
            amount: event.amount,
        }
    }
}

impl From<&Inbox::RefundClaimed> for RefundClaim {
    fn from(event: &Inbox::RefundClaimed) -> Self {
        Self {
            token: event.token,
            recipient: event.recipient,
            amount: event.amount,
        }
    }
}

impl From<&Portal::submitBatchCall> for BatchSubmission {
    fn from(call: &Portal::submitBatchCall) -> Self {
        Self {
            tempo_block: call.tempoBlockNumber,
            previous_block: call.blockTransition.prevBlockHash,
            next_block: call.blockTransition.nextBlockHash,
            previous_deposit: Cursor {
                hash: call.depositQueueTransition.prevProcessedHash,
                number: call.depositQueueTransition.prevDepositNumber,
            },
            next_deposit: Cursor {
                hash: call.depositQueueTransition.nextProcessedHash,
                number: call.depositQueueTransition.nextDepositNumber,
            },
            withdrawal_queue_hash: call.withdrawalQueueHash,
            next_zone_height: call.nextZoneHeight,
        }
    }
}

impl From<&Portal::Withdrawal> for Withdrawal {
    fn from(withdrawal: &Portal::Withdrawal) -> Self {
        Self {
            token: withdrawal.token,
            sender_tag: withdrawal.senderTag,
            to: withdrawal.to,
            amount: withdrawal.amount,
            memo: withdrawal.memo,
            gas_limit: withdrawal.gasLimit,
            fallback_nonce: withdrawal.fallbackNonce,
            callback_data: withdrawal.callbackData.clone(),
            encrypted_sender: withdrawal.encryptedSender.clone(),
        }
    }
}

impl From<&Portal::DepositBounceBack> for WithdrawalOutcome {
    fn from(event: &Portal::DepositBounceBack) -> Self {
        Self::FailedDepositPaid {
            collected_fee: event.bouncebackFee,
        }
    }
}

impl From<&Portal::DepositBounceBackPending> for WithdrawalOutcome {
    fn from(event: &Portal::DepositBounceBackPending) -> Self {
        Self::FailedDepositPending {
            collected_fee: event.bouncebackFee,
        }
    }
}

impl From<&Inbox::WithdrawalBounceBackProcessed> for DepositOutcome {
    fn from(event: &Inbox::WithdrawalBounceBackProcessed) -> Self {
        Self::BounceBackMinted {
            recipient: event.zoneFallbackRecipient,
        }
    }
}

impl From<&Inbox::WithdrawalBounceBackPending> for DepositOutcome {
    fn from(event: &Inbox::WithdrawalBounceBackPending) -> Self {
        Self::BounceBackPending {
            recipient: event.zoneFallbackRecipient,
        }
    }
}

impl From<&Portal::WithdrawalProcessed> for Effect {
    fn from(event: &Portal::WithdrawalProcessed) -> Self {
        Self::UserWithdrawalProcessed {
            to: event.to,
            sender_tag: event.senderTag,
            token: event.token,
            amount: event.amount,
            callback_success: event.callbackSuccess,
        }
    }
}

impl From<&Portal::DepositBounceBack> for Effect {
    fn from(event: &Portal::DepositBounceBack) -> Self {
        Self::FailedDepositRefunded {
            recipient: event.tempoRefundRecipient,
            token: event.token,
            amount: event.amount,
            fee: event.bouncebackFee,
            pending: false,
        }
    }
}

impl From<&Portal::DepositBounceBackPending> for Effect {
    fn from(event: &Portal::DepositBounceBackPending) -> Self {
        Self::FailedDepositRefunded {
            recipient: event.tempoRefundRecipient,
            token: event.token,
            amount: event.amount,
            fee: event.bouncebackFee,
            pending: true,
        }
    }
}

impl From<&Inbox::TokenEnabled> for Effect {
    fn from(event: &Inbox::TokenEnabled) -> Self {
        Self::TokenEnabled {
            token: event.token,
            name: event.name.clone(),
            symbol: event.symbol.clone(),
            currency: event.currency.clone(),
        }
    }
}

impl From<&Inbox::DepositProcessed> for Effect {
    fn from(event: &Inbox::DepositProcessed) -> Self {
        Self::DepositProcessed {
            deposit_hash: event.depositHash,
            sender: event.sender,
            token: event.token,
            amount: event.amount,
        }
    }
}

impl From<&Inbox::DepositFailed> for Effect {
    fn from(event: &Inbox::DepositFailed) -> Self {
        Self::DepositFailed {
            deposit_hash: event.depositHash,
            sender: event.sender,
            token: event.token,
            amount: event.amount,
        }
    }
}

impl From<&Portal::RefundClaimed> for Effect {
    fn from(event: &Portal::RefundClaimed) -> Self {
        Self::RefundClaimed {
            token: event.token,
            recipient: event.recipient,
            amount: event.amount,
        }
    }
}

impl From<&Inbox::RefundClaimed> for Effect {
    fn from(event: &Inbox::RefundClaimed) -> Self {
        Self::RefundClaimed {
            token: event.token,
            recipient: event.recipient,
            amount: event.amount,
        }
    }
}

impl From<&Inbox::WithdrawalBounceBackProcessed> for Effect {
    fn from(event: &Inbox::WithdrawalBounceBackProcessed) -> Self {
        Self::BounceBackMinted {
            token: event.token,
            amount: event.amount,
        }
    }
}

impl From<&Inbox::WithdrawalBounceBackPending> for Effect {
    fn from(event: &Inbox::WithdrawalBounceBackPending) -> Self {
        Self::BounceBackPending {
            token: event.token,
            amount: event.amount,
        }
    }
}

/// Decode the fixed-size ciphertext authenticated by deposit calldata or events.
fn ciphertext(bytes: &[u8]) -> Result<FixedBytes<64>, Failure> {
    let ciphertext: [u8; 64] = bytes.try_into().map_err(|_| {
        AdapterFindingCode::EventSequence.failure("ordinary deposit ciphertext is not 64 bytes")
    })?;
    Ok(FixedBytes::from(ciphertext))
}
