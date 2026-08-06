//! Strict native Zone event grammar and model-input projection.

mod advance;
mod cursor;
mod deposits;
mod post_advance;
mod withdrawal;

use crate::{
    model::input::{
        BatchFinalizationInput, ZoneBlockContext, ZoneBlockInput, ZoneDepositPrefixInput,
    },
    observe::L2BlockObservation,
};

use self::{
    advance::project_advance,
    cursor::ZoneEventCursor,
    post_advance::{project_batch_finalization, project_zone_operations},
};
use super::{ObservedZoneOutputs, ZoneProjection, ZoneProjectionError};

/// Project one authenticated Zone observation into deterministic model input
/// and independently retained implementation output.
pub(crate) fn project_zone(
    observation: &L2BlockObservation,
) -> Result<ZoneProjection, ZoneProjectionError> {
    let authenticated = observation.inputs();
    let advance = authenticated.advance_tempo();
    let finalization_hash = authenticated
        .finalization()
        .map(|envelope| envelope.transaction_hash());
    let mut events = ZoneEventCursor::new(
        observation.outcomes().events(),
        authenticated.advance_transaction_hash(),
    );

    let advance_projection = project_advance(&mut events, advance)?;
    let zone_operations = project_zone_operations(&mut events, finalization_hash)?;
    let finalization = authenticated.finalization().map(|envelope| {
        BatchFinalizationInput::new(
            envelope.input().count(),
            envelope.input().block_number(),
            envelope.input().encrypted_senders().to_vec(),
        )
    });
    let batch_finalized = project_batch_finalization(&mut events, finalization_hash)?;

    Ok(ZoneProjection {
        input: ZoneBlockInput::new(
            ZoneBlockContext::new(observation.block_hash(), observation.block_number()),
            ZoneDepositPrefixInput::new(
                advance_projection.enabled_tokens,
                advance_projection.deposits,
                advance_projection.deposit_inputs,
            ),
            zone_operations.inputs,
            finalization,
        ),
        outputs: ObservedZoneOutputs {
            tempo_block_finalized: advance_projection.tempo_block_finalized,
            token_enables: advance_projection.token_enables,
            deposit_outcomes: advance_projection.deposit_outcomes,
            tempo_advanced: advance_projection.tempo_advanced,
            operations: zone_operations.outputs,
            batch_finalized,
        },
    })
}
