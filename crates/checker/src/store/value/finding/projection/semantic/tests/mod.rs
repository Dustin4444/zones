use alloy_primitives::{Address, B256, U256, keccak256};

use crate::store::value::FindingSummary;

use super::Canonical;

mod errors;
mod model;
mod outputs;

#[derive(Default)]
struct Golden(Vec<u8>);

impl Golden {
    fn tagged(tag: u8) -> Self {
        Self(vec![tag])
    }

    fn u8(&mut self, value: u8) {
        self.0.push(value);
    }

    fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_be_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_be_bytes());
    }

    fn usize(&mut self, value: usize) {
        self.u64(u64::try_from(value).unwrap());
    }

    fn u128(&mut self, value: u128) {
        self.0.extend_from_slice(&value.to_be_bytes());
    }

    fn u256(&mut self, value: U256) {
        self.0.extend_from_slice(&value.to_be_bytes::<32>());
    }

    fn address(&mut self, value: Address) {
        self.0.extend_from_slice(value.as_slice());
    }

    fn hash(&mut self, value: B256) {
        self.0.extend_from_slice(value.as_slice());
    }

    fn bytes(&mut self, value: &[u8]) {
        self.usize(value.len());
        self.0.extend_from_slice(value);
    }

    fn finish(self) -> Vec<u8> {
        self.0
    }
}

fn assert_golden(actual: FindingSummary, expected: &[u8]) {
    assert_eq!(actual.length(), u64::try_from(expected.len()).unwrap());
    assert_eq!(actual.hash(), keccak256(expected));
}

#[test]
fn canonical_scalar_and_optional_layout_is_golden() {
    let address = Address::repeat_byte(0x11);
    let hash = B256::repeat_byte(0x22);
    let mut actual = Canonical::tagged(0xa0);
    actual.bool(true);
    actual.u32(0x0102_0304);
    actual.u64(0x0102_0304_0506_0708);
    actual.u128(0x0102_0304_0506_0708_1112_1314_1516_1718);
    actual.u256(U256::from(0x2122_2324_u64));
    actual.address(address);
    actual.hash(hash);
    actual.bytes(&[0x31, 0x32, 0x33]).unwrap();
    actual
        .option(Some(0x41_u8), |encoder, value| {
            encoder.u8(value);
            Ok(())
        })
        .unwrap();
    actual.option::<u8>(None, |_, _| unreachable!()).unwrap();

    let mut expected = Golden::tagged(0xa0);
    expected.bool(true);
    expected.u32(0x0102_0304);
    expected.u64(0x0102_0304_0506_0708);
    expected.u128(0x0102_0304_0506_0708_1112_1314_1516_1718);
    expected.u256(U256::from(0x2122_2324_u64));
    expected.address(address);
    expected.hash(hash);
    expected.bytes(&[0x31, 0x32, 0x33]);
    expected.u8(1);
    expected.u8(0x41);
    expected.u8(0);
    assert_golden(actual.finish().unwrap(), &expected.finish());
}
