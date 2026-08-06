//! Shared physical rows for coarse Portal and Zone replacements.

use crate::store::{schema::ModelKey, value::*};

use super::cursor;
use crate::model::state::{PortalLifecycle, ZoneState};

pub(super) fn portal_rows(portal: &PortalLifecycle) -> [(ModelKey, Option<ModelValue>); 3] {
    let PortalLifecycle::Created(portal) = portal else {
        return [
            (ModelKey::PortalConfig, None),
            (ModelKey::PortalDepositCursor, None),
            (ModelKey::PortalSettlement, None),
        ];
    };
    let settlement = portal.settlement();
    let submitted = settlement.last_submitted_deposit_cursor();
    [
        (
            ModelKey::PortalConfig,
            Some(ModelValue::PortalConfig {
                bounceback_gas: portal.config().bounceback_gas(),
            }),
        ),
        (
            ModelKey::PortalDepositCursor,
            Some(ModelValue::PortalDepositCursor(cursor(
                portal.deposit_cursor().hash(),
                portal.deposit_cursor().number(),
            ))),
        ),
        (
            ModelKey::PortalSettlement,
            Some(ModelValue::PortalSettlement(PortalSettlementValue {
                withdrawal_batch_index: settlement.withdrawal_batch_index(),
                block_hash: settlement.block_hash(),
                last_synced_tempo_block_number: settlement.last_synced_tempo_block_number(),
                last_submitted_deposit_cursor: cursor(submitted.hash, submitted.number),
                zone_height: settlement.zone_height(),
                withdrawal_queue_head: settlement.withdrawal_queue_head(),
                withdrawal_queue_tail: settlement.withdrawal_queue_tail(),
            })),
        ),
    ]
}

pub(super) fn zone_rows(zone: &ZoneState) -> [(ModelKey, ModelValue); 5] {
    let processed = zone.processed_deposit_cursor();
    let last = zone.last_batch();
    let start = zone.batch_start();
    let first_processed = start.first_processed_deposit();
    [
        (
            ModelKey::ZoneConfig,
            ModelValue::ZoneConfig {
                tempo_gas_rate: zone.config().tempo_gas_rate(),
                max_withdrawals_per_block: zone.config().max_withdrawals_per_block(),
            },
        ),
        (
            ModelKey::ZoneProcessedDepositCursor,
            ModelValue::ZoneProcessedDepositCursor(cursor(processed.hash(), processed.number())),
        ),
        (
            ModelKey::ZoneBatchAccumulator,
            ModelValue::ZoneBatchAccumulator(ZoneBatchAccumulatorValue {
                last_withdrawal_queue_hash: last.withdrawal_queue_hash(),
                last_withdrawal_batch_index: last.withdrawal_batch_index(),
                first_zone_parent_hash: start.first_zone_parent_hash(),
                first_processed_deposit: cursor(first_processed.hash(), first_processed.number()),
                first_withdrawal_index: start.first_withdrawal_index(),
            }),
        ),
        (
            ModelKey::ZoneNextWithdrawalIndex,
            ModelValue::ZoneNextWithdrawalIndex(zone.next_withdrawal_index()),
        ),
        (
            ModelKey::ZoneLastFallbackNonce,
            ModelValue::ZoneLastFallbackNonce(zone.last_fallback_nonce()),
        ),
    ]
}
