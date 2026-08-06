use alloy_primitives::{Address, B256, U256};

use crate::{
    check::finding::{TempoAdvancedExpectation, TempoBlockFinalizedExpectation},
    model::output::{ExpectedDepositOutcome, ExpectedOutputs, ExpectedZoneOperation},
};

use super::super::super::{
    expected_batch_finalized, expected_deposit_outcome, expected_token_enable,
    expected_zone_operation, tempo_advanced_expectation, tempo_block_finalized_expectation,
};
use super::super::{Golden, assert_golden};
use super::assert_coverage;

fn deposit_kind(output: &ExpectedDepositOutcome) -> &'static str {
    match output {
        ExpectedDepositOutcome::OrdinaryMinted(_) => "minted",
        ExpectedDepositOutcome::OrdinaryFailed(_) => "failed",
        ExpectedDepositOutcome::WithdrawalBounceBackMinted(_) => "bounce_minted",
        ExpectedDepositOutcome::WithdrawalBounceBackPending(_) => "bounce_pending",
    }
}

fn operation_kind(output: &ExpectedZoneOperation) -> &'static str {
    match output {
        ExpectedZoneOperation::WithdrawalRequested(_) => "withdrawal",
        ExpectedZoneOperation::RefundClaimed(_) => "refund",
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
            bytes.hash(B256::repeat_byte(52));
            bytes.address(Address::repeat_byte(53));
            bytes.address(Address::repeat_byte(54));
            bytes.u128(55);
        }
        "failed" => {
            bytes.u32(56);
            bytes.u64(57);
            bytes.address(Address::repeat_byte(58));
            bytes.address(Address::repeat_byte(59));
            bytes.address(Address::repeat_byte(60));
            bytes.u128(61);
            bytes.u128(62);
            bytes.hash(B256::repeat_byte(63));
            bytes.u64(64);
            bytes.u64(65);
            bytes.bytes(&[66, 67]);
            bytes.bytes(&[68]);
            bytes.hash(B256::repeat_byte(69));
            bytes.address(Address::repeat_byte(70));
            bytes.address(Address::repeat_byte(71));
            bytes.u128(72);
        }
        "bounce_minted" => {
            bytes.address(Address::repeat_byte(73));
            bytes.u128(74);
        }
        "bounce_pending" => {
            bytes.address(Address::repeat_byte(75));
            bytes.u128(76);
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
            bytes.u32(83);
            bytes.u64(84);
            bytes.address(Address::repeat_byte(85));
            bytes.address(Address::repeat_byte(86));
            bytes.address(Address::repeat_byte(87));
            bytes.u128(88);
            bytes.u128(89);
            bytes.hash(B256::repeat_byte(90));
            bytes.u64(91);
            bytes.u64(92);
            bytes.bytes(&[93]);
            bytes.bytes(&[94, 95]);
        }
        "refund" => {
            bytes.address(Address::repeat_byte(77));
            bytes.address(Address::repeat_byte(78));
            bytes.u128(79);
        }
        _ => unreachable!(),
    }
    bytes.finish()
}

#[test]
fn every_expected_zone_output_family_and_branch_is_golden() {
    let fixture = ExpectedOutputs::semantic_fixture_for_test();
    let prefix = fixture.zone_deposit_prefix();
    let block = fixture.zone_block();

    let finalized =
        TempoBlockFinalizedExpectation::for_test(B256::repeat_byte(90), 91, B256::repeat_byte(92));
    let mut finalized_bytes = Golden::tagged(1);
    finalized_bytes.hash(B256::repeat_byte(90));
    finalized_bytes.u64(91);
    finalized_bytes.hash(B256::repeat_byte(92));
    assert_golden(
        tempo_block_finalized_expectation(finalized).unwrap(),
        &finalized_bytes.finish(),
    );

    let token = &prefix.token_enables()[0];
    let mut token_bytes = Golden::tagged(1);
    token_bytes.address(Address::repeat_byte(51));
    token_bytes.bytes(b"name");
    token_bytes.bytes(b"SYM");
    token_bytes.bytes(b"CUR");
    assert_golden(expected_token_enable(token).unwrap(), &token_bytes.finish());

    assert_coverage(
        prefix.deposit_outcomes().iter().map(deposit_kind),
        &["minted", "failed", "bounce_minted", "bounce_pending"],
    );
    for output in prefix.deposit_outcomes() {
        let kind = deposit_kind(output);
        assert_golden(
            expected_deposit_outcome(output).unwrap(),
            &expected_deposit_bytes(kind),
        );
    }

    let advanced = TempoAdvancedExpectation::for_test(
        B256::repeat_byte(93),
        94,
        U256::from(95),
        B256::repeat_byte(96),
        97,
    );
    let mut advanced_bytes = Golden::tagged(1);
    advanced_bytes.hash(B256::repeat_byte(93));
    advanced_bytes.u64(94);
    advanced_bytes.u256(U256::from(95));
    advanced_bytes.hash(B256::repeat_byte(96));
    advanced_bytes.u64(97);
    assert_golden(
        tempo_advanced_expectation(advanced).unwrap(),
        &advanced_bytes.finish(),
    );

    assert_coverage(
        block.operations().iter().map(operation_kind),
        &["withdrawal", "refund"],
    );
    for output in block.operations() {
        let kind = operation_kind(output);
        assert_golden(
            expected_zone_operation(output).unwrap(),
            &expected_operation_bytes(kind),
        );
    }

    let mut some_bytes = Golden::tagged(1);
    some_bytes.u8(1);
    some_bytes.u32(80);
    some_bytes.u64(81);
    some_bytes.hash(B256::repeat_byte(82));
    let some = block.finalized_batch();
    let none = None;
    assert_golden(
        expected_batch_finalized(some).unwrap(),
        &some_bytes.finish(),
    );
    assert_golden(expected_batch_finalized(none).unwrap(), &[1, 0]);
    assert_coverage(
        [
            some.map_or("none", |_| "some"),
            none.map_or("none", |_| "some"),
        ],
        &["some", "none"],
    );
}
