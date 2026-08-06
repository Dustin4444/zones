use alloy_primitives::{Address, B256, U256};

use crate::model::adapter::{ObservedImportedOutput, ObservedProcessedWithdrawal};

use super::super::super::observed_imported_output;
use super::super::{Golden, assert_golden};
use super::{assert_coverage, position};

fn operation_kind(output: &ObservedImportedOutput) -> &'static str {
    match output {
        ObservedImportedOutput::DepositAppended(_) => "deposit",
        ObservedImportedOutput::BatchSubmitted(_) => "batch",
        ObservedImportedOutput::WithdrawalsProcessed(_) => "processing",
        ObservedImportedOutput::RefundClaimed(_) => "refund",
    }
}

fn member_kind(output: &ObservedProcessedWithdrawal) -> &'static str {
    match output {
        ObservedProcessedWithdrawal::UserDelivered(_) => "delivered",
        ObservedProcessedWithdrawal::UserBounced(_) => "bounced",
        ObservedProcessedWithdrawal::FailedDepositPaid(_) => "paid",
        ObservedProcessedWithdrawal::FailedDepositPending(_) => "pending",
    }
}

fn expected_bytes(kind: &str) -> Vec<u8> {
    let mut bytes = Golden::tagged(match kind {
        "deposit" => 1,
        "batch" => 2,
        "processing" => 3,
        "refund" => 4,
        _ => unreachable!(),
    });
    match kind {
        "deposit" => {
            position(&mut bytes, 1, 2, 3, 4);
            bytes.hash(B256::repeat_byte(5));
            bytes.u64(6);
        }
        "batch" => {
            position(&mut bytes, 7, 8, 9, 10);
            bytes.u64(11);
            bytes.u256(U256::from(12));
            bytes.hash(B256::repeat_byte(13));
            bytes.hash(B256::repeat_byte(14));
            bytes.hash(B256::repeat_byte(15));
            bytes.u64(16);
        }
        "processing" => {
            bytes.usize(17);
            bytes.hash(B256::repeat_byte(18));
            bytes.usize(4);

            bytes.u8(1);
            bytes.usize(1);
            position(&mut bytes, 19, 20, 21, 22);
            bytes.hash(B256::repeat_byte(23));
            bytes.u64(24);
            position(&mut bytes, 25, 26, 27, 28);
            bytes.address(Address::repeat_byte(29));
            bytes.hash(B256::repeat_byte(30));
            bytes.address(Address::repeat_byte(31));
            bytes.u128(32);
            bytes.bool(true);

            bytes.u8(2);
            position(&mut bytes, 33, 34, 35, 36);
            bytes.hash(B256::repeat_byte(37));
            bytes.u64(38);
            bytes.address(Address::repeat_byte(39));
            bytes.u128(40);
            bytes.u64(41);
            position(&mut bytes, 42, 43, 44, 45);
            bytes.address(Address::repeat_byte(46));
            bytes.hash(B256::repeat_byte(47));
            bytes.address(Address::repeat_byte(48));
            bytes.u128(49);
            bytes.bool(false);

            bytes.u8(3);
            position(&mut bytes, 50, 51, 52, 53);
            bytes.address(Address::repeat_byte(54));
            bytes.address(Address::repeat_byte(55));
            bytes.u128(56);
            bytes.u128(57);

            bytes.u8(4);
            position(&mut bytes, 58, 59, 60, 61);
            bytes.address(Address::repeat_byte(62));
            bytes.address(Address::repeat_byte(63));
            bytes.u128(64);
            bytes.u128(65);
        }
        "refund" => {
            position(&mut bytes, 66, 67, 68, 69);
            bytes.address(Address::repeat_byte(70));
            bytes.address(Address::repeat_byte(71));
            bytes.u128(72);
        }
        _ => unreachable!(),
    }
    bytes.finish()
}

#[test]
fn every_observed_imported_output_and_nested_member_is_golden() {
    let operations = ObservedImportedOutput::semantic_fixtures_for_test();
    assert_coverage(
        operations.iter().map(operation_kind),
        &["deposit", "batch", "processing", "refund"],
    );
    for output in &operations {
        let kind = operation_kind(output);
        assert_golden(
            observed_imported_output(output).unwrap(),
            &expected_bytes(kind),
        );
    }

    let processing = operations
        .iter()
        .find_map(|output| match output {
            ObservedImportedOutput::WithdrawalsProcessed(output) => Some(output),
            ObservedImportedOutput::DepositAppended(_)
            | ObservedImportedOutput::BatchSubmitted(_)
            | ObservedImportedOutput::RefundClaimed(_) => None,
        })
        .unwrap();
    assert_coverage(
        processing.members().iter().map(member_kind),
        &["delivered", "bounced", "paid", "pending"],
    );
}
