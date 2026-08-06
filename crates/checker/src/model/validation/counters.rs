use super::{AuthoritativeStateError, CursorKind, cursor, require_cursor_prefix};
use crate::model::{
    constants::WITHDRAWAL_QUEUE_CAPACITY,
    state::{BatchStart, CreatedPortalState, ModelState, PortalSettlementState},
};

pub(super) fn validate(
    state: &ModelState,
    portal: &CreatedPortalState,
) -> Result<(), AuthoritativeStateError> {
    let settlement = portal.settlement();
    let zone = state.zone();
    let queue_length = settlement
        .withdrawal_queue_tail()
        .checked_sub(settlement.withdrawal_queue_head())
        .ok_or(AuthoritativeStateError::PortalQueueCountersReversed {
            head: settlement.withdrawal_queue_head(),
            tail: settlement.withdrawal_queue_tail(),
        })?;
    if queue_length > WITHDRAWAL_QUEUE_CAPACITY {
        return Err(AuthoritativeStateError::PortalQueueCapacityExceeded {
            length: queue_length,
            capacity: WITHDRAWAL_QUEUE_CAPACITY,
        });
    }

    let portal_cursor = cursor(
        CursorKind::PortalDeposit,
        portal.deposit_cursor().hash(),
        portal.deposit_cursor().number(),
    )?;
    let zone_cursor = cursor(
        CursorKind::ZoneProcessedDeposit,
        zone.processed_deposit_cursor().hash(),
        zone.processed_deposit_cursor().number(),
    )?;
    let submitted_cursor = cursor(
        CursorKind::PortalLastSubmittedDeposit,
        settlement.last_submitted_deposit_cursor().hash,
        settlement.last_submitted_deposit_cursor().number,
    )?;
    let batch_start_cursor = cursor(
        CursorKind::ZoneBatchStartDeposit,
        zone.batch_start().first_processed_deposit().hash(),
        zone.batch_start().first_processed_deposit().number(),
    )?;
    require_cursor_prefix(zone_cursor, portal_cursor)?;
    require_cursor_prefix(submitted_cursor, zone_cursor)?;
    require_cursor_prefix(batch_start_cursor, zone_cursor)?;

    if settlement.withdrawal_batch_index() == 0 && settlement != PortalSettlementState::ZERO {
        return Err(AuthoritativeStateError::UnsubmittedPortalHasSettlementProgress);
    }
    let zone_batch_index = zone.last_batch().withdrawal_batch_index();
    if settlement.withdrawal_batch_index() > zone_batch_index {
        return Err(AuthoritativeStateError::PortalBatchCounterBeyondZone {
            portal_batch_index: settlement.withdrawal_batch_index(),
            zone_batch_index,
        });
    }
    if settlement.zone_height() > alloy_primitives::U256::from(u64::MAX) {
        return Err(AuthoritativeStateError::PortalZoneHeightOverflow {
            height: settlement.zone_height(),
        });
    }
    if zone.last_fallback_nonce() > zone.next_withdrawal_index() {
        return Err(AuthoritativeStateError::FallbackCounterBeyondWithdrawals {
            fallback_nonce: zone.last_fallback_nonce(),
            next_withdrawal_index: zone.next_withdrawal_index(),
        });
    }
    if zone_batch_index == 0
        && (!zone.last_batch().withdrawal_queue_hash().is_zero()
            || zone.batch_start() != BatchStart::INITIAL)
    {
        return Err(AuthoritativeStateError::UnfinalizedZoneHasBatchProgress);
    }
    if zone.batch_start().first_withdrawal_index() > zone.next_withdrawal_index() {
        return Err(AuthoritativeStateError::BatchStartBeyondNextWithdrawal {
            first_withdrawal_index: zone.batch_start().first_withdrawal_index(),
            next_withdrawal_index: zone.next_withdrawal_index(),
        });
    }
    Ok(())
}
