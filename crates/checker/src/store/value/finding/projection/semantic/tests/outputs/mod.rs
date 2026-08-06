//! Exhaustive golden vectors for expected and observed output summaries.

use std::collections::BTreeSet;

use alloy_primitives::B256;

use super::Golden;

mod expected_imported;
mod expected_zone;
mod observed_imported;
mod observed_zone;

fn position(bytes: &mut Golden, transaction: usize, hash: u8, receipt: usize, block: usize) {
    bytes.usize(transaction);
    bytes.hash(B256::repeat_byte(hash));
    bytes.usize(receipt);
    bytes.usize(block);
}

fn assert_coverage(actual: impl IntoIterator<Item = &'static str>, expected: &[&'static str]) {
    let actual = actual.into_iter().collect::<BTreeSet<_>>();
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(actual, expected);
}
