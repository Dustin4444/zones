use alloy_primitives::{Address, B256, U256};

use crate::model::output::{
    ExpectedImportedTempoOperation, ExpectedOutputs, ExpectedProcessedWithdrawal,
};

use super::super::super::expected_imported_output;
use super::super::{Golden, assert_golden};
use super::assert_coverage;

fn operation_kind(output: &ExpectedImportedTempoOperation) -> &'static str {
    match output {
        ExpectedImportedTempoOperation::DepositAppended(_) => "deposit",
        ExpectedImportedTempoOperation::BatchSubmitted(_) => "batch",
        ExpectedImportedTempoOperation::WithdrawalsProcessed(_) => "processing",
        ExpectedImportedTempoOperation::RefundClaimed(_) => "refund",
    }
}

fn member_kind(output: &ExpectedProcessedWithdrawal) -> &'static str {
    match output {
        ExpectedProcessedWithdrawal::UserDelivered(_) => "delivered",
        ExpectedProcessedWithdrawal::UserBounced(_) => "bounced",
        ExpectedProcessedWithdrawal::FailedDepositPaid(_) => "paid",
        ExpectedProcessedWithdrawal::FailedDepositPending(_) => "pending",
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
            bytes.address(Address::repeat_byte(1));
            bytes.u64(2);
            bytes.hash(B256::repeat_byte(3));
        }
        "batch" => {
            bytes.u32(4);
            bytes.u64(5);
            bytes.u256(U256::from(6));
            bytes.hash(B256::repeat_byte(7));
            bytes.hash(B256::repeat_byte(8));
            bytes.hash(B256::repeat_byte(9));
            bytes.u64(10);
        }
        "processing" => {
            bytes.usize(4);

            bytes.u8(1);
            bytes.usize(1);
            bytes.address(Address::repeat_byte(11));
            bytes.u64(12);
            bytes.hash(B256::repeat_byte(13));
            bytes.u32(14);
            bytes.u64(15);
            bytes.address(Address::repeat_byte(16));
            bytes.hash(B256::repeat_byte(17));
            bytes.address(Address::repeat_byte(18));
            bytes.u128(19);
            bytes.bool(true);

            bytes.u8(2);
            bytes.address(Address::repeat_byte(20));
            bytes.u64(21);
            bytes.u128(22);
            bytes.address(Address::repeat_byte(23));
            bytes.u64(24);
            bytes.hash(B256::repeat_byte(25));
            bytes.u32(26);
            bytes.u64(27);
            bytes.address(Address::repeat_byte(28));
            bytes.hash(B256::repeat_byte(29));
            bytes.address(Address::repeat_byte(30));
            bytes.u128(31);
            bytes.bool(false);

            bytes.u8(3);
            bytes.address(Address::repeat_byte(32));
            bytes.u64(33);
            bytes.address(Address::repeat_byte(34));
            bytes.address(Address::repeat_byte(35));
            bytes.u128(36);
            bytes.u128(37);

            bytes.u8(4);
            bytes.address(Address::repeat_byte(38));
            bytes.u64(39);
            bytes.address(Address::repeat_byte(40));
            bytes.address(Address::repeat_byte(41));
            bytes.u128(42);
            bytes.u128(43);
        }
        "refund" => {
            bytes.address(Address::repeat_byte(44));
            bytes.address(Address::repeat_byte(45));
            bytes.u128(46);
        }
        _ => unreachable!(),
    }
    bytes.finish()
}

#[test]
fn every_expected_imported_output_and_nested_member_is_golden() {
    let fixture = ExpectedOutputs::semantic_fixture_for_test();
    let operations = fixture.imported_tempo_block().operations();

    assert_coverage(
        operations.iter().map(operation_kind),
        &["deposit", "batch", "processing", "refund"],
    );
    for output in operations {
        let kind = operation_kind(output);
        assert_golden(
            expected_imported_output(output).unwrap(),
            &expected_bytes(kind),
        );
    }

    let processing = operations
        .iter()
        .find_map(|output| match output {
            ExpectedImportedTempoOperation::WithdrawalsProcessed(output) => Some(output),
            ExpectedImportedTempoOperation::DepositAppended(_)
            | ExpectedImportedTempoOperation::BatchSubmitted(_)
            | ExpectedImportedTempoOperation::RefundClaimed(_) => None,
        })
        .unwrap();
    assert_coverage(
        processing.members().iter().map(member_kind),
        &["delivered", "bounced", "paid", "pending"],
    );
}
