//! Exhaustive projection from disposable checker diagnostics into durable rows.
//!
//! Location recovery lives here because the candidate block is the only
//! authoritative source for Zone transaction hashes. Canonical evidence bytes
//! are encoded in the sibling semantic modules; runtime formatting is never a
//! persistence input.

mod model_key;
mod semantic;

use alloy_consensus::{BlockHeader as _, transaction::TxHashRef as _};
use alloy_eips::BlockNumHash;
use alloy_primitives::B256;
use reth_primitives_traits::RecoveredBlock;
use tempo_primitives::Block;

use crate::{
    check::finding::{Finding, FixedStateFinding, ObservationFinding},
    model::adapter::{ImportedProjectionError, ZoneProjectionError},
    observe::{AuthenticatedTransaction, EnvelopeLocation, PortalCallError, ProtocolChain},
    store::{
        error::{StoreError, StoreResult},
        schema::FindingKey,
    },
};

use super::{
    leaf::{StoredDataSource, StoredProtocolChain},
    types::{ChainLocation, FindingKind, FindingRecord, FindingStatus},
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

mod outputs;

#[cfg(test)]
mod tests;

impl FindingRecord {
    /// Lower one authenticated candidate finding into its stable store key and
    /// value. Ordinal zero is release one's single-finding-per-block policy.
    pub(crate) fn from_candidate(
        block: &RecoveredBlock<Block>,
        imported_tempo: Option<BlockNumHash>,
        finding: &Finding,
    ) -> StoreResult<(FindingKey, Self)> {
        let context = ProjectionContext { block };
        let key = FindingKey::new(block.header().number(), block.hash(), 0);
        let kind = context.finding_kind(finding)?;
        let record = Self::new(
            block.header().parent_hash(),
            imported_tempo,
            FindingStatus::Canonical,
            kind,
        )
        .ok_or(StoreError::InvalidPersistedValue(
            "projected checker finding",
        ))?;
        Ok((key, record))
    }
}

struct ProjectionContext<'a> {
    block: &'a RecoveredBlock<Block>,
}

impl ProjectionContext<'_> {
    fn finding_kind(&self, finding: &Finding) -> StoreResult<FindingKind> {
        Ok(match finding {
            Finding::Observation(finding) => self.observation(finding)?,
            Finding::ZoneContinuity {
                expected_number,
                expected_hash,
                actual_number,
                actual_parent,
            } => FindingKind::ZoneContinuity(
                BlockNumHash::new(*expected_number, *expected_hash),
                *actual_number,
                *actual_parent,
            ),
            Finding::TempoContinuity {
                expected_number,
                expected_hash,
                actual_number,
                actual_parent,
            } => FindingKind::TempoContinuity(
                BlockNumHash::new(*expected_number, *expected_hash),
                *actual_number,
                *actual_parent,
            ),
            Finding::PortalObservationIdentityMismatch { expected, actual } => {
                FindingKind::PortalObservationIdentityMismatch(*expected, *actual)
            }
            Finding::PortalCreationBlockMismatch { expected, actual } => {
                FindingKind::PortalCreationBlockMismatch(*expected, *actual)
            }
            Finding::PortalCreationMissing { block_hash } => {
                FindingKind::PortalCreationMissing(*block_hash)
            }
            Finding::ImportedProjection(error) => FindingKind::ImportedProjectionViolation(
                self.imported_projection_location(error)?,
                error.into(),
                semantic::imported_projection(error)?,
            ),
            Finding::ZoneProjection(error) => FindingKind::ZoneProjectionViolation(
                self.zone_projection_location(error)?,
                error.into(),
                semantic::zone_projection(error)?,
            ),
            Finding::Model(error) => FindingKind::ModelViolation(
                self.zone_block(),
                error.into(),
                model_key::model_key(error),
                semantic::model(error)?,
            ),
            Finding::ImportedOutput(finding) => self.imported_output(finding)?,
            Finding::ZoneOutput(finding) => self.zone_output(finding)?,
            Finding::FixedState(finding) => fixed_state(*finding),
            Finding::CollateralDeficit {
                token,
                required,
                actual,
            } => FindingKind::CollateralDeficit(*token, *required, *actual),
            Finding::MissingSupply { token } => FindingKind::MissingSupply(*token),
            Finding::SupplyMismatch {
                token,
                expected,
                actual,
            } => FindingKind::SupplyMismatch(*token, *expected, *actual),
        })
    }

    fn observation(&self, finding: &ObservationFinding) -> StoreResult<FindingKind> {
        Ok(match finding {
            ObservationFinding::InvalidEnvelope { location, rule } => {
                let location = match location {
                    EnvelopeLocation::Block => self.zone_block(),
                    EnvelopeLocation::Transaction(index) => self.zone_transaction(*index)?,
                };
                FindingKind::InvalidEnvelope(location, (*rule).into())
            }
            ObservationFinding::MalformedAuthenticatedData {
                kind,
                transaction,
                evidence,
                detail: _,
            } => {
                let source = StoredDataSource::from(*kind);
                FindingKind::MalformedAuthenticatedData(
                    self.authenticated_transaction(*transaction)?,
                    source,
                    semantic::malformed_authenticated_data(*evidence),
                )
            }
            ObservationFinding::ProtocolEvent {
                chain,
                transaction_index,
                receipt_log_index,
                block_log_index,
                transaction_hash,
                error,
            } => {
                let location = self.protocol_log(
                    *chain,
                    *transaction_index,
                    *transaction_hash,
                    *receipt_log_index,
                    *block_log_index,
                )?;
                match error.as_ref() {
                    crate::model::events::ProtocolEventError::UnsupportedProtocolEvent {
                        emitter,
                        topic0,
                    } => FindingKind::UnsupportedProtocolEvent(location, *emitter, *topic0),
                    crate::model::events::ProtocolEventError::MalformedProtocolEvent {
                        emitter,
                        topic0,
                        event,
                        reason: _,
                    } => FindingKind::MalformedProtocolEvent(
                        location,
                        *emitter,
                        *topic0,
                        semantic::malformed_event(event)?,
                    ),
                }
            }
            ObservationFinding::PortalCall(error) => FindingKind::PortalCallViolation(
                portal_call_location(error),
                error.into(),
                semantic::portal_call(error)?,
            ),
        })
    }

    fn imported_projection_location(
        &self,
        error: &ImportedProjectionError,
    ) -> StoreResult<ChainLocation> {
        match error {
            ImportedProjectionError::OutcomeCoordinateMismatch {
                transaction_index,
                transaction_hash,
                ..
            } => tempo_transaction(*transaction_index, *transaction_hash),
            ImportedProjectionError::TransactionOrderMismatch {
                next: transaction_index,
                ..
            }
            | ImportedProjectionError::InvalidCreationGrammar { transaction_index }
            | ImportedProjectionError::InvalidSubmitBatchGrammar { transaction_index }
            | ImportedProjectionError::DirectCallRequired {
                transaction_index, ..
            }
            | ImportedProjectionError::UnexpectedEvent {
                transaction_index, ..
            }
            | ImportedProjectionError::InvalidWithdrawalPreimage {
                transaction_index, ..
            }
            | ImportedProjectionError::MissingWithdrawalOutcome {
                transaction_index, ..
            }
            | ImportedProjectionError::UnexpectedWithdrawalOutcome {
                transaction_index, ..
            }
            | ImportedProjectionError::WithdrawalCallbackSuccessMismatch {
                transaction_index,
                ..
            }
            | ImportedProjectionError::ExtraWithdrawalOutcomes {
                transaction_index, ..
            } => tempo_transaction_index(*transaction_index),
            ImportedProjectionError::InvalidDepositCiphertextLength {
                block_log_index, ..
            }
            | ImportedProjectionError::InvalidDepositKeyParity {
                block_log_index, ..
            } => tempo_block_log(*block_log_index),
            ImportedProjectionError::MissingBaseFee
            | ImportedProjectionError::BlockHashMismatch { .. }
            | ImportedProjectionError::BlockNumberMismatch { .. } => Ok(self.tempo_block()),
        }
    }

    fn zone_projection_location(&self, error: &ZoneProjectionError) -> StoreResult<ChainLocation> {
        use ZoneProjectionError::*;
        match error {
            ReorderedTempoBlockFinalized { position, .. }
            | ReorderedTokenEnabled { position, .. }
            | ReorderedDepositOutcome { position, .. }
            | ReorderedDepositFailed { position, .. }
            | ReorderedTempoAdvanced { position, .. }
            | ExtraAdvanceEvent { position, .. }
            | AdvanceTransactionHashMismatch { position, .. }
            | UnexpectedPostAdvanceEvent { position, .. }
            | BatchFinalizedWithoutEnvelope { position }
            | BatchFinalizedWrongTransaction { position, .. }
            | ReorderedBatchFinalized { position, .. }
            | ExtraFinalizationEvent { position, .. } => zone_log!(self, *position),
            InvalidWithdrawalRequest {
                transaction_index, ..
            } => self.zone_transaction(*transaction_index),
            MissingBatchFinalized { transaction_hash } => {
                self.zone_transaction_hash(*transaction_hash)
            }
            MissingTempoBlockFinalized
            | MissingTokenEnabled { .. }
            | MissingDepositOutcome { .. }
            | MissingDepositFailed { .. }
            | MissingTempoAdvanced
            | InvalidDepositKeyParity { .. }
            | InvalidDepositCiphertextLength { .. }
            | InvalidBounceBackRecipient { .. }
            | ZeroBounceBackNonce { .. }
            | ZeroBounceBackAmount { .. }
            | UnsupportedDepositKind { .. } => Ok(self.zone_block()),
        }
    }

    const fn zone_block(&self) -> ChainLocation {
        ChainLocation::block(StoredProtocolChain::ZoneL2)
    }

    const fn tempo_block(&self) -> ChainLocation {
        ChainLocation::block(StoredProtocolChain::TempoL1)
    }

    fn zone_transaction(&self, transaction_index: usize) -> StoreResult<ChainLocation> {
        let transaction = self
            .block
            .body()
            .transactions
            .get(transaction_index)
            .ok_or(StoreError::InvalidPersistedValue(
                "finding Zone transaction index",
            ))?;
        Ok(ChainLocation::transaction(
            StoredProtocolChain::ZoneL2,
            index(transaction_index)?,
            *transaction.tx_hash(),
        ))
    }

    fn zone_transaction_hash(&self, hash: B256) -> StoreResult<ChainLocation> {
        let transaction_index = self
            .block
            .body()
            .transactions
            .iter()
            .position(|transaction| *transaction.tx_hash() == hash)
            .ok_or(StoreError::InvalidPersistedValue(
                "finding Zone transaction hash",
            ))?;
        Ok(ChainLocation::transaction(
            StoredProtocolChain::ZoneL2,
            index(transaction_index)?,
            hash,
        ))
    }

    fn zone_log(
        &self,
        transaction_index: usize,
        transaction_hash: B256,
        receipt_log_index: usize,
        block_log_index: usize,
    ) -> StoreResult<ChainLocation> {
        let transaction = self.zone_transaction(transaction_index)?;
        if transaction.transaction_coordinate().map(|(_, hash)| hash) != Some(transaction_hash) {
            return Err(StoreError::InvalidPersistedValue(
                "finding Zone log transaction hash",
            ));
        }
        Ok(ChainLocation::log(
            StoredProtocolChain::ZoneL2,
            index(transaction_index)?,
            transaction_hash,
            index(receipt_log_index)?,
            index(block_log_index)?,
        ))
    }

    fn protocol_log(
        &self,
        chain: ProtocolChain,
        transaction_index: usize,
        transaction_hash: B256,
        receipt_log_index: usize,
        block_log_index: usize,
    ) -> StoreResult<ChainLocation> {
        let chain = StoredProtocolChain::from(chain);
        if chain == StoredProtocolChain::ZoneL2 {
            let candidate = self.zone_transaction(transaction_index)?;
            if candidate.transaction_coordinate().map(|(_, hash)| hash) != Some(transaction_hash) {
                return Err(StoreError::InvalidPersistedValue(
                    "finding Zone protocol-event transaction hash",
                ));
            }
        }
        Ok(ChainLocation::log(
            chain,
            index(transaction_index)?,
            transaction_hash,
            index(receipt_log_index)?,
            index(block_log_index)?,
        ))
    }

    fn authenticated_transaction(
        &self,
        transaction: AuthenticatedTransaction,
    ) -> StoreResult<ChainLocation> {
        match transaction.chain() {
            ProtocolChain::ZoneL2 => {
                let location = self.zone_transaction(transaction.transaction_index())?;
                if location.transaction_hash_coordinate() != Some(transaction.transaction_hash()) {
                    return Err(StoreError::InvalidPersistedValue(
                        "malformed-data Zone transaction hash",
                    ));
                }
                Ok(location)
            }
            ProtocolChain::TempoL1 => tempo_transaction(
                transaction.transaction_index(),
                transaction.transaction_hash(),
            ),
        }
    }
}

fn tempo_transaction(transaction_index: usize, hash: B256) -> StoreResult<ChainLocation> {
    Ok(ChainLocation::transaction(
        StoredProtocolChain::TempoL1,
        index(transaction_index)?,
        hash,
    ))
}

fn tempo_transaction_index(transaction_index: usize) -> StoreResult<ChainLocation> {
    Ok(ChainLocation::transaction_index(
        StoredProtocolChain::TempoL1,
        index(transaction_index)?,
    ))
}

fn tempo_block_log(block_log_index: usize) -> StoreResult<ChainLocation> {
    Ok(ChainLocation::block_log_index(
        StoredProtocolChain::TempoL1,
        index(block_log_index)?,
    ))
}

fn portal_call_location(error: &PortalCallError) -> ChainLocation {
    let transaction_hash = match error {
        PortalCallError::UnsupportedNestedPortalCall {
            transaction_hash, ..
        }
        | PortalCallError::ConflictingFamilies { transaction_hash }
        | PortalCallError::FamilyMismatch {
            transaction_hash, ..
        }
        | PortalCallError::EmptyProcessWithOutcomes { transaction_hash } => *transaction_hash,
    };
    ChainLocation::transaction_hash(StoredProtocolChain::TempoL1, transaction_hash)
}

fn tempo_log(
    transaction_index: usize,
    transaction_hash: B256,
    receipt_log_index: usize,
    block_log_index: usize,
) -> StoreResult<ChainLocation> {
    Ok(ChainLocation::log(
        StoredProtocolChain::TempoL1,
        index(transaction_index)?,
        transaction_hash,
        index(receipt_log_index)?,
        index(block_log_index)?,
    ))
}

fn fixed_state(finding: FixedStateFinding) -> FindingKind {
    match finding {
        FixedStateFinding::TempoBlockHash { expected, actual } => {
            FindingKind::TempoBlockHashMismatch(expected, actual)
        }
        FixedStateFinding::TempoBlockNumber { expected, actual } => {
            FindingKind::TempoBlockNumberMismatch(expected, actual)
        }
        FixedStateFinding::ProcessedDepositHash { expected, actual } => {
            FindingKind::ProcessedDepositHashMismatch(expected, actual)
        }
        FixedStateFinding::ProcessedDepositNumber { expected, actual } => {
            FindingKind::ProcessedDepositNumberMismatch(expected, actual)
        }
        FixedStateFinding::WithdrawalQueueHash { expected, actual } => {
            FindingKind::WithdrawalQueueHashMismatch(expected, actual)
        }
        FixedStateFinding::WithdrawalBatchIndex { expected, actual } => {
            FindingKind::WithdrawalBatchIndexMismatch(expected, actual)
        }
    }
}

fn index(value: usize) -> StoreResult<u64> {
    u64::try_from(value).map_err(|_| StoreError::InvalidPersistedValue("finding usize field"))
}
