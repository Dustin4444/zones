use super::*;

#[test]
fn every_model_and_nested_variant_has_complete_golden_bytes() {
    for (name, value) in fixtures() {
        let expected = golden_model(&value);
        let actual = value.clone().compress();
        assert_eq!(actual, expected, "wire drift in {name}");
        assert_eq!(ModelValue::decompress(&actual).unwrap(), value, "{name}");
        for cut in 0..actual.len() {
            assert!(
                ModelValue::decompress(&actual[..cut]).is_err(),
                "truncation accepted for {name} at {cut}"
            );
        }
    }
}
