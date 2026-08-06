use alloy_primitives::{B256, U256};
use tempo_zone_contracts::IZoneOutbox;

use super::super::support::*;
use crate::{
    check::finding::{CheckError, Finding, FixedStateFinding},
    model::accounting::TokenAccounting,
};

#[derive(Debug, Clone, Copy)]
enum FixedSlot {
    TempoHash,
    TempoNumber,
    ProcessedHash,
    ProcessedNumber,
    WithdrawalHash,
    WithdrawalBatchIndex,
}

#[tokio::test]
async fn each_exact_commitment_mismatch_is_typed_and_keeps_the_verified_parent_atomic() {
    for slot in [
        FixedSlot::TempoHash,
        FixedSlot::TempoNumber,
        FixedSlot::ProcessedHash,
        FixedSlot::ProcessedNumber,
        FixedSlot::WithdrawalHash,
        FixedSlot::WithdrawalBatchIndex,
    ] {
        let imported = imported_header(0);
        let model = created_model(TokenAccounting {
            supply: U256::from(10),
            deposit_liability: U256::ZERO,
            withdrawal_liability: U256::ZERO,
        });
        let l2 = zone_observation(
            &imported,
            Vec::new(),
            Vec::new(),
            advance_logs(&imported, Vec::new(), B256::ZERO, 0),
            Vec::new(),
            Some(ZoneFinalization {
                encrypted_senders: Vec::new(),
                event: zone_log(
                    crate::model::constants::ZONE_OUTBOX_ADDRESS,
                    IZoneOutbox::BatchFinalized {
                        withdrawalQueueHash: B256::ZERO,
                        withdrawalBatchIndex: 1,
                    },
                ),
            }),
        );
        let mut exact =
            ExactPostState::from_model(&model).with_supply(INITIAL_TOKEN, U256::from(10));
        exact.withdrawal_batch_index = 1;
        match slot {
            FixedSlot::TempoHash => exact.tempo_hash = Some(B256::repeat_byte(0xa1)),
            FixedSlot::TempoNumber => exact.tempo_number = Some(TEMPO_NUMBER + 1),
            FixedSlot::ProcessedHash => exact.processed_hash = B256::repeat_byte(0xa2),
            FixedSlot::ProcessedNumber => exact.processed_number = 1,
            FixedSlot::WithdrawalHash => exact.withdrawal_hash = B256::repeat_byte(0xa3),
            FixedSlot::WithdrawalBatchIndex => exact.withdrawal_batch_index = 2,
        }
        let (checker, result) = run_block(
            model.clone(),
            &imported,
            Vec::new(),
            &l2,
            &[U256::from(10)],
            exact,
            false,
        )
        .await;
        let error = result.unwrap_err();

        assert!(matches_slot(&error, slot), "{slot:?}: {error}");
        assert_eq!(checker.model(), &model);
        assert_eq!(
            checker.zone_tip(),
            alloy_eips::BlockNumHash::new(ZONE_NUMBER - 1, ZONE_PARENT)
        );
        assert_eq!(
            checker.tempo_tip(),
            alloy_eips::BlockNumHash::new(TEMPO_NUMBER - 1, TEMPO_PARENT)
        );
    }
}

fn matches_slot(error: &CheckError, slot: FixedSlot) -> bool {
    let CheckError::Finding(finding) = error else {
        return false;
    };
    matches!(
        (finding.as_ref(), slot),
        (
            Finding::FixedState(FixedStateFinding::TempoBlockHash { .. }),
            FixedSlot::TempoHash
        ) | (
            Finding::FixedState(FixedStateFinding::TempoBlockNumber { .. }),
            FixedSlot::TempoNumber,
        ) | (
            Finding::FixedState(FixedStateFinding::ProcessedDepositHash { .. }),
            FixedSlot::ProcessedHash,
        ) | (
            Finding::FixedState(FixedStateFinding::ProcessedDepositNumber { .. }),
            FixedSlot::ProcessedNumber,
        ) | (
            Finding::FixedState(FixedStateFinding::WithdrawalQueueHash { .. }),
            FixedSlot::WithdrawalHash,
        ) | (
            Finding::FixedState(FixedStateFinding::WithdrawalBatchIndex { .. }),
            FixedSlot::WithdrawalBatchIndex,
        )
    )
}
