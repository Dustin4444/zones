//! Programmatic conformance tests for Solidity and native Rust storage layouts.

use super::{RustStorageField, artifact, assert_layout};
use tempo_precompiles::{
    test_util::storage_conformance::{RustStorageSlot, assert_foundry_slots},
    zone_factory::zone_portal_slots,
};
use tempo_precompiles_macros::gen_test_fields_layout as layout_fields;

#[test]
fn zone_portal_slot_constants_match_solidity() {
    let fields = [
        ("admin", zone_portal_slots::ADMIN),
        (
            "currentDepositQueueHash",
            zone_portal_slots::CURRENT_DEPOSIT_QUEUE_HASH,
        ),
        ("_encryptionKeys", zone_portal_slots::ENCRYPTION_KEYS),
        ("_tokenConfigs", zone_portal_slots::TOKEN_CONFIGS),
        ("role", zone_portal_slots::ROLE),
        ("_isAccessEnforced", zone_portal_slots::IS_ACCESS_ENFORCED),
        ("_isGatewayEnforced", zone_portal_slots::IS_GATEWAY_ENFORCED),
        ("maxTempoGasRate", zone_portal_slots::MAX_TEMPO_GAS_RATE),
        ("pauseExpiry", zone_portal_slots::PAUSE_EXPIRY),
        (
            "tokenEnablementHash",
            zone_portal_slots::TOKEN_ENABLEMENT_HASH,
        ),
        (
            "abdicationEffectiveAt",
            zone_portal_slots::ABDICATION_EFFECTIVE_AT,
        ),
    ]
    .map(|(name, slot)| RustStorageSlot::new(name, slot));
    assert_foundry_slots(&artifact("ZonePortal"), &fields);
}

#[test]
fn tempo_state_layout_matches_solidity() {
    use zone_precompiles::tempo_state::slots;
    assert_layout(
        "TempoState",
        layout_fields!(tempo_block_hash, tempo_block_number),
    );
}

#[test]
fn zone_inbox_layout_matches_solidity() {
    use zone_precompiles::inbox::slots;
    assert_layout(
        "ZoneInbox",
        layout_fields!(
            processed_deposit_queue_hash,
            processed_deposit_number,
            withdrawal_bounce_backs,
            processed_token_enablement_hash
        )
        .into_iter()
        .map(|field| match field.name {
            "withdrawalBounceBacks" => field.solidity_name("_refunds"),
            _ => field,
        })
        .collect(),
    );
}

#[test]
fn zone_outbox_layout_matches_solidity() {
    use zone_precompiles::outbox::slots;
    assert_layout(
        "ZoneOutbox",
        layout_fields!(
            tempo_gas_rate,
            next_withdrawal_index,
            withdrawal_queue_hash,
            withdrawal_batch_index,
            max_withdrawals_per_block,
            withdrawals_this_block,
            current_block_number,
            last_finalized_timestamp,
            pending_withdrawals,
            last_fallback_nonce,
            fallback_recipients
        )
        .into_iter()
        .map(|field| match field.name {
            "withdrawalQueueHash" => field.solidity_name("_withdrawalQueueHash"),
            "withdrawalBatchIndex" => field.solidity_name("_withdrawalBatchIndex"),
            "withdrawalsThisBlock" => field.solidity_name("_withdrawalsThisBlock"),
            "currentBlockNumber" => field.solidity_name("_currentBlockNumber"),
            "pendingWithdrawals" => field.solidity_name("_pendingWithdrawals"),
            "fallbackRecipients" => field.solidity_name("_zoneFallbackRecipients"),
            _ => field,
        })
        .collect(),
    );
}
