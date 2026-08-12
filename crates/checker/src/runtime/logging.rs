use crate::{
    kernel::{Deposit, DepositOutcome, ImportedOperation, ZoneOperation},
    persistence::{BlockNumHash, Finding},
    runtime::AuthenticatedBlock,
};

#[derive(Clone, Copy)]
struct BlockContext {
    zone: BlockNumHash,
    tempo: BlockNumHash,
}

impl From<&AuthenticatedBlock> for BlockContext {
    fn from(block: &AuthenticatedBlock) -> Self {
        Self {
            zone: block.zone,
            tempo: block.tempo,
        }
    }
}

pub(super) fn verified(block: &AuthenticatedBlock) {
    let context = BlockContext::from(block);
    tracing::debug!(
        target: "zone::checker",
        zone_block = context.zone.number,
        zone_hash = %context.zone.hash,
        tempo_block = context.tempo.number,
        tempo_hash = %context.tempo.hash,
        imported_operations = block.imported.operations.len(),
        enabled_tokens = block.zone_facts.enabled_tokens.len(),
        deposits = block.zone_facts.deposits.len(),
        deposit_outcomes = block.zone_facts.outcomes.len(),
        zone_operations = block.zone_facts.operations.len(),
        finalized = block.zone_facts.finalization.is_some(),
        "verified Zone block"
    );
    for operation in &block.imported.operations {
        log_imported(context, operation);
    }
    for (deposit, outcome) in block
        .zone_facts
        .deposits
        .iter()
        .zip(&block.zone_facts.outcomes)
    {
        log_deposit(context, deposit, outcome);
    }
    for operation in &block.zone_facts.operations {
        log_zone(context, operation);
    }
    if let Some(finalization) = &block.zone_facts.finalization {
        tracing::info!(
            target: "zone::checker",
            zone_block = context.zone.number,
            tempo_block = context.tempo.number,
            finalized_block = finalization.block_number,
            withdrawals = finalization.declared_count,
            encrypted_senders = finalization.encrypted_senders.len(),
            "verified withdrawal finalization"
        );
    }
}

pub(super) fn divergence(block: &AuthenticatedBlock, finding: &Finding) {
    tracing::error!(
        target: "zone::checker",
        zone_block = block.zone.number,
        zone_hash = %block.zone.hash,
        tempo_block = block.tempo.number,
        tempo_hash = %block.tempo.hash,
        category = ?finding.details.category,
        code = finding.details.code,
        location = ?finding.details.location,
        summary = finding.summary,
        "checker recorded authenticated divergence"
    );
}

fn log_imported(context: BlockContext, operation: &ImportedOperation) {
    match operation {
        ImportedOperation::Create {
            identity,
            initial_token,
        } => {
            tracing::info!(target: "zone::checker", zone_block = context.zone.number, tempo_block = context.tempo.number, portal = %identity.portal, zone_id = identity.zone_id, "verified Portal creation");
            log_token(context, initial_token);
        }
        ImportedOperation::EnableToken(token) => log_token(context, token),
        ImportedOperation::AppendDeposit(_) => {}
        ImportedOperation::SubmitBatch(batch) => {
            tracing::info!(target: "zone::checker", zone_block = context.zone.number, tempo_block = context.tempo.number, next_zone_height = %batch.next_zone_height, "verified batch submission")
        }
        ImportedOperation::ProcessWithdrawals(processing) => {
            tracing::info!(target: "zone::checker", zone_block = context.zone.number, tempo_block = context.tempo.number, withdrawals = processing.withdrawals.len(), outcomes = processing.outcomes.len(), "verified withdrawal processing")
        }
        ImportedOperation::ClaimPortalRefund(refund) => {
            tracing::info!(target: "zone::checker", zone_block = context.zone.number, tempo_block = context.tempo.number, token = %refund.token, amount = refund.amount, "verified Portal refund claim")
        }
        ImportedOperation::UpdateBouncebackGas(gas) => {
            tracing::info!(target: "zone::checker", zone_block = context.zone.number, tempo_block = context.tempo.number, bounceback_gas = gas, "verified bounce-back gas update")
        }
    }
}

fn log_deposit(context: BlockContext, deposit: &Deposit, outcome: &DepositOutcome) {
    match deposit {
        Deposit::Ordinary(deposit) => {
            tracing::info!(target: "zone::checker", zone_block = context.zone.number, tempo_block = context.tempo.number, token = %deposit.token, amount = deposit.amount, outcome = outcome_name(outcome), "verified deposit")
        }
        Deposit::BounceBack(deposit) => {
            tracing::info!(target: "zone::checker", zone_block = context.zone.number, tempo_block = context.tempo.number, token = %deposit.token, amount = deposit.amount, fallback_nonce = deposit.fallback_nonce.get(), outcome = outcome_name(outcome), "verified bounce-back deposit")
        }
    }
}

fn log_zone(context: BlockContext, operation: &ZoneOperation) {
    match operation {
        ZoneOperation::AcceptWithdrawal(withdrawal) => {
            tracing::info!(target: "zone::checker", zone_block = context.zone.number, tempo_block = context.tempo.number, token = %withdrawal.token, amount = withdrawal.amount, gas_limit = withdrawal.gas_limit, "verified withdrawal request")
        }
        ZoneOperation::ClaimInboxRefund(refund) => {
            tracing::info!(target: "zone::checker", zone_block = context.zone.number, tempo_block = context.tempo.number, token = %refund.token, amount = refund.amount, "verified Inbox refund claim")
        }
        ZoneOperation::UpdateTempoGasRate(rate) => {
            tracing::info!(target: "zone::checker", zone_block = context.zone.number, tempo_block = context.tempo.number, tempo_gas_rate = rate, "verified Tempo gas-rate update")
        }
        ZoneOperation::UpdateMaxWithdrawals(max) => {
            tracing::info!(target: "zone::checker", zone_block = context.zone.number, tempo_block = context.tempo.number, max_withdrawals = max, "verified maximum-withdrawals update")
        }
    }
}

fn log_token(context: BlockContext, token: &crate::kernel::TokenEnable) {
    tracing::info!(target: "zone::checker", zone_block = context.zone.number, tempo_block = context.tempo.number, token = %token.token, name = %token.name, symbol = %token.symbol, currency = %token.currency, "verified token enablement");
}

const fn outcome_name(outcome: &DepositOutcome) -> &'static str {
    match outcome {
        DepositOutcome::Minted => "minted",
        DepositOutcome::Failed => "failed",
        DepositOutcome::BounceBackMinted { .. } => "bounce_back_minted",
        DepositOutcome::BounceBackPending { .. } => "bounce_back_pending",
    }
}
