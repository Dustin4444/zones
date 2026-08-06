use alloy_primitives::{Address, B256, U256};

use crate::model::adapter::{ObservedDepositOutcome, ObservedZoneOperation, ObservedZoneOutputs};

use super::super::super::{
    observed_batch_finalized, observed_deposit_outcome, observed_tempo_advanced,
    observed_tempo_block_finalized, observed_token_enable, observed_zone_operation,
};
use super::super::{Golden, assert_golden};
use super::{assert_coverage, position};

fn deposit_kind(output: &ObservedDepositOutcome) -> &'static str {
    match output {
        ObservedDepositOutcome::OrdinaryMinted(_) => "minted",
        ObservedDepositOutcome::OrdinaryFailed { .. } => "failed",
        ObservedDepositOutcome::WithdrawalBounceBackMinted(_) => "bounce_minted",
        ObservedDepositOutcome::WithdrawalBounceBackPending(_) => "bounce_pending",
    }
}

fn operation_kind(output: &ObservedZoneOperation) -> &'static str {
    match output {
        ObservedZoneOperation::WithdrawalRequested(_) => "withdrawal",
        ObservedZoneOperation::RefundClaimed(_) => "refund",
    }
}

fn expected_deposit_bytes(kind: &str) -> Vec<u8> {
    let mut bytes = Golden::tagged(match kind {
        "minted" => 1,
        "failed" => 2,
        "bounce_minted" => 3,
        "bounce_pending" => 4,
        _ => unreachable!(),
    });
    match kind {
        "minted" => {
            position(&mut bytes, 13, 14, 15, 16);
            bytes.hash(B256::repeat_byte(17));
            bytes.address(Address::repeat_byte(18));
            bytes.address(Address::repeat_byte(19));
            bytes.address(Address::repeat_byte(20));
            bytes.u128(21);
            bytes.hash(B256::repeat_byte(22));
        }
        "failed" => {
            position(&mut bytes, 23, 24, 25, 26);
            bytes.u64(27);
            bytes.address(Address::repeat_byte(28));
            bytes.address(Address::repeat_byte(29));
            bytes.address(Address::repeat_byte(30));
            bytes.u128(31);
            bytes.u128(32);
            bytes.hash(B256::repeat_byte(33));
            bytes.u64(34);
            bytes.u64(35);
            bytes.bytes(&[36, 37]);
            bytes.bytes(&[38]);
            position(&mut bytes, 39, 40, 41, 42);
            bytes.hash(B256::repeat_byte(43));
            bytes.address(Address::repeat_byte(44));
            bytes.address(Address::repeat_byte(45));
            bytes.u128(46);
        }
        "bounce_minted" => {
            position(&mut bytes, 47, 48, 49, 50);
            bytes.address(Address::repeat_byte(51));
            bytes.address(Address::repeat_byte(52));
            bytes.u128(53);
        }
        "bounce_pending" => {
            position(&mut bytes, 54, 55, 56, 57);
            bytes.address(Address::repeat_byte(58));
            bytes.address(Address::repeat_byte(59));
            bytes.u128(60);
        }
        _ => unreachable!(),
    }
    bytes.finish()
}

fn expected_operation_bytes(kind: &str) -> Vec<u8> {
    let mut bytes = Golden::tagged(match kind {
        "withdrawal" => 1,
        "refund" => 2,
        _ => unreachable!(),
    });
    match kind {
        "withdrawal" => {
            position(&mut bytes, 70, 71, 72, 73);
            bytes.u64(74);
            bytes.address(Address::repeat_byte(75));
            bytes.address(Address::repeat_byte(76));
            bytes.address(Address::repeat_byte(77));
            bytes.u128(78);
            bytes.u128(79);
            bytes.hash(B256::repeat_byte(80));
            bytes.u64(81);
            bytes.u64(82);
            bytes.bytes(&[83]);
            bytes.bytes(&[84, 85]);
        }
        "refund" => {
            position(&mut bytes, 86, 87, 88, 89);
            bytes.address(Address::repeat_byte(90));
            bytes.address(Address::repeat_byte(91));
            bytes.u128(92);
        }
        _ => unreachable!(),
    }
    bytes.finish()
}

#[test]
fn every_observed_zone_output_family_and_branch_is_golden() {
    let fixture = ObservedZoneOutputs::semantic_fixture_for_test();

    let mut finalized_bytes = Golden::tagged(1);
    position(&mut finalized_bytes, 1, 2, 3, 4);
    finalized_bytes.hash(B256::repeat_byte(5));
    finalized_bytes.u64(6);
    finalized_bytes.hash(B256::repeat_byte(7));
    assert_golden(
        observed_tempo_block_finalized(&fixture.tempo_block_finalized()).unwrap(),
        &finalized_bytes.finish(),
    );

    let mut token_bytes = Golden::tagged(1);
    position(&mut token_bytes, 8, 9, 10, 11);
    token_bytes.address(Address::repeat_byte(12));
    token_bytes.bytes(b"name");
    token_bytes.bytes(b"SYM");
    token_bytes.bytes(b"CUR");
    assert_golden(
        observed_token_enable(&fixture.token_enables()[0]).unwrap(),
        &token_bytes.finish(),
    );

    assert_coverage(
        fixture.deposit_outcomes().iter().map(deposit_kind),
        &["minted", "failed", "bounce_minted", "bounce_pending"],
    );
    for output in fixture.deposit_outcomes() {
        let kind = deposit_kind(output);
        assert_golden(
            observed_deposit_outcome(output).unwrap(),
            &expected_deposit_bytes(kind),
        );
    }

    let mut advanced_bytes = Golden::tagged(1);
    position(&mut advanced_bytes, 61, 62, 63, 64);
    advanced_bytes.hash(B256::repeat_byte(65));
    advanced_bytes.u64(66);
    advanced_bytes.u256(U256::from(67));
    advanced_bytes.hash(B256::repeat_byte(68));
    advanced_bytes.u64(69);
    assert_golden(
        observed_tempo_advanced(&fixture.tempo_advanced()).unwrap(),
        &advanced_bytes.finish(),
    );

    assert_coverage(
        fixture.operations().iter().map(operation_kind),
        &["withdrawal", "refund"],
    );
    for output in fixture.operations() {
        let kind = operation_kind(output);
        assert_golden(
            observed_zone_operation(output).unwrap(),
            &expected_operation_bytes(kind),
        );
    }

    let mut some_bytes = Golden::tagged(1);
    some_bytes.u8(1);
    position(&mut some_bytes, 93, 94, 95, 96);
    some_bytes.hash(B256::repeat_byte(97));
    some_bytes.u64(98);
    let finalized = fixture.batch_finalized();
    let none = None;
    assert_golden(
        observed_batch_finalized(finalized.as_ref()).unwrap(),
        &some_bytes.finish(),
    );
    assert_golden(observed_batch_finalized(none).unwrap(), &[1, 0]);
    assert_coverage(
        [
            finalized.map_or("none", |_| "some"),
            none.map_or("none", |_| "some"),
        ],
        &["some", "none"],
    );
}
