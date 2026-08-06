use crate::{
    check::finding::{ImportedOutputFinding, ZoneOutputFinding},
    model::adapter::{ObservedDepositOutcome, ObservedImportedOutput, ObservedZoneOperation},
    store::error::StoreResult,
};

use super::{
    ChainLocation, FindingKind, ProjectionContext, index, semantic, tempo_log, tempo_transaction,
};

macro_rules! zone_log {
    ($context:expr, $position:expr) => {{
        let position = $position;
        $context.zone_log(
            position.transaction_index(),
            position.transaction_hash(),
            position.receipt_log_index(),
            position.block_log_index(),
        )
    }};
}

macro_rules! tempo_log {
    ($position:expr) => {{
        let position = $position;
        tempo_log(
            position.transaction_index(),
            position.transaction_hash(),
            position.receipt_log_index(),
            position.block_log_index(),
        )
    }};
}

impl ProjectionContext<'_> {
    pub(super) fn imported_output(
        &self,
        finding: &ImportedOutputFinding,
    ) -> StoreResult<FindingKind> {
        Ok(match finding {
            ImportedOutputFinding::Count { expected, actual } => {
                FindingKind::ImportedOutputCountMismatch(index(*expected)?, index(*actual)?)
            }
            ImportedOutputFinding::Mismatch {
                index: operation,
                expected,
                actual,
            } => FindingKind::ImportedOutputMismatch(
                index(*operation)?,
                imported_output_location(actual)?,
                semantic::expected_imported_output(expected)?,
                semantic::observed_imported_output(actual)?,
            ),
        })
    }

    pub(super) fn zone_output(&self, finding: &ZoneOutputFinding) -> StoreResult<FindingKind> {
        Ok(match finding {
            ZoneOutputFinding::TempoBlockFinalized { expected, actual } => {
                FindingKind::TempoBlockFinalizedMismatch(
                    zone_log!(self, actual.position())?,
                    semantic::tempo_block_finalized_expectation(*expected)?,
                    semantic::observed_tempo_block_finalized(actual)?,
                )
            }
            ZoneOutputFinding::TokenEnableCount { expected, actual } => {
                FindingKind::TokenEnableCountMismatch(index(*expected)?, index(*actual)?)
            }
            ZoneOutputFinding::TokenEnable {
                index: operation,
                expected,
                actual,
            } => FindingKind::TokenEnableMismatch(
                index(*operation)?,
                zone_log!(self, actual.position())?,
                semantic::expected_token_enable(expected)?,
                semantic::observed_token_enable(actual)?,
            ),
            ZoneOutputFinding::DepositOutcomeCount { expected, actual } => {
                FindingKind::DepositOutcomeCountMismatch(index(*expected)?, index(*actual)?)
            }
            ZoneOutputFinding::DepositOutcome {
                index: operation,
                expected,
                actual,
            } => FindingKind::DepositOutcomeMismatch(
                index(*operation)?,
                self.deposit_outcome_location(actual)?,
                semantic::expected_deposit_outcome(expected)?,
                semantic::observed_deposit_outcome(actual)?,
            ),
            ZoneOutputFinding::TempoAdvanced { expected, actual } => {
                FindingKind::TempoAdvancedMismatch(
                    zone_log!(self, actual.position())?,
                    semantic::tempo_advanced_expectation(*expected)?,
                    semantic::observed_tempo_advanced(actual)?,
                )
            }
            ZoneOutputFinding::OperationCount { expected, actual } => {
                FindingKind::ZoneOperationCountMismatch(index(*expected)?, index(*actual)?)
            }
            ZoneOutputFinding::Operation {
                index: operation,
                expected,
                actual,
            } => FindingKind::ZoneOperationMismatch(
                index(*operation)?,
                self.zone_operation_location(actual)?,
                semantic::expected_zone_operation(expected)?,
                semantic::observed_zone_operation(actual)?,
            ),
            ZoneOutputFinding::BatchFinalized { expected, actual } => {
                let location = match actual {
                    Some(actual) => zone_log!(self, actual.position())?,
                    None => self.zone_block(),
                };
                FindingKind::BatchFinalizedMismatch(
                    location,
                    semantic::expected_batch_finalized(*expected)?,
                    semantic::observed_batch_finalized(actual.as_deref())?,
                )
            }
        })
    }

    fn deposit_outcome_location(
        &self,
        output: &ObservedDepositOutcome,
    ) -> StoreResult<ChainLocation> {
        match output {
            ObservedDepositOutcome::OrdinaryMinted(output) => zone_log!(self, output.position()),
            ObservedDepositOutcome::OrdinaryFailed { withdrawal, .. } => {
                zone_log!(self, withdrawal.position())
            }
            ObservedDepositOutcome::WithdrawalBounceBackMinted(output) => {
                zone_log!(self, output.position())
            }
            ObservedDepositOutcome::WithdrawalBounceBackPending(output) => {
                zone_log!(self, output.position())
            }
        }
    }

    fn zone_operation_location(
        &self,
        output: &ObservedZoneOperation,
    ) -> StoreResult<ChainLocation> {
        match output {
            ObservedZoneOperation::WithdrawalRequested(output) => {
                zone_log!(self, output.position())
            }
            ObservedZoneOperation::RefundClaimed(output) => zone_log!(self, output.position()),
        }
    }
}

fn imported_output_location(output: &ObservedImportedOutput) -> StoreResult<ChainLocation> {
    match output {
        ObservedImportedOutput::DepositAppended(output) => tempo_log!(output.position()),
        ObservedImportedOutput::BatchSubmitted(output) => tempo_log!(output.position()),
        ObservedImportedOutput::WithdrawalsProcessed(output) => {
            tempo_transaction(output.transaction_index(), output.transaction_hash())
        }
        ObservedImportedOutput::RefundClaimed(output) => tempo_log!(output.position()),
    }
}
