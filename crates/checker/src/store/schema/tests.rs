use super::*;

fn bytes<T: Encode>(value: T) -> Vec<u8> {
    value.encode().as_ref().to_vec()
}

fn tagged(tag: u8, parts: &[&[u8]]) -> Vec<u8> {
    let mut encoded = vec![tag];
    for part in parts {
        encoded.extend_from_slice(part);
    }
    encoded
}

fn assert_model_key(key: ModelKey, golden: Vec<u8>) {
    assert_eq!(bytes(key), golden);
    assert_eq!(ModelKey::decode(&golden).unwrap(), key);
}

#[test]
fn metadata_tags_are_golden_and_decode_strictly() {
    let keys = [
        MetaKey::Version,
        MetaKey::ZoneIdentity,
        MetaKey::L1ChainId,
        MetaKey::Contracts,
        MetaKey::PortalCreationBlock,
        MetaKey::Bootstrap,
        MetaKey::VerifiedZoneTip,
        MetaKey::ImportedTempoTip,
        MetaKey::ActiveAlert,
    ];
    for (tag, key) in keys.into_iter().enumerate() {
        assert_eq!(bytes(key), [tag as u8]);
        assert_eq!(MetaKey::decode(&[tag as u8]).unwrap(), key);
    }

    assert!(MetaKey::decode(&[]).is_err());
    assert!(MetaKey::decode(&[meta_tag::VERSION, 0]).is_err());
    assert!(MetaKey::decode(&[0xff]).is_err());
}

#[test]
fn release_one_schema_is_exactly_five_non_dupsort_tables() {
    assert_eq!(CheckerTables::COUNT, 5);
    assert!(CheckerTables::ALL.iter().all(|table| !table.is_dupsort()));
    assert_eq!(
        CheckerTables::ALL
            .iter()
            .map(CheckerTables::name)
            .collect::<Vec<_>>(),
        vec![
            "CheckerMeta",
            "CheckerCanonical",
            "CheckerModelState",
            "CheckerChangesets",
            "CheckerFindings",
        ]
    );
}

#[test]
fn canonical_hash_value_is_exactly_32_bytes() {
    let value = CanonicalHash::new(B256::repeat_byte(0xa5));
    let encoded = value.compress();
    assert_eq!(encoded, vec![0xa5; HASH_LEN]);
    assert_eq!(CanonicalHash::decompress(&encoded).unwrap(), value);
    assert!(CanonicalHash::decompress(&encoded[..HASH_LEN - 1]).is_err());

    let mut trailing = encoded;
    trailing.push(0);
    assert!(CanonicalHash::decompress(&trailing).is_err());
}

#[test]
fn canonical_height_key_is_fixed_width_big_endian_and_strict() {
    let height = 0x0102_0304_0506_0708_u64;
    let golden = height.to_be_bytes();
    assert_eq!(bytes(height), golden);
    assert_eq!(u64::decode(&golden).unwrap(), height);
    assert!(u64::decode(&golden[..golden.len() - 1]).is_err());

    let mut trailing = golden.to_vec();
    trailing.push(0);
    assert!(u64::decode(&trailing).is_err());
}

#[test]
fn model_key_families_have_golden_bytes_and_round_trip() {
    let token = Address::repeat_byte(0x11);
    let recipient = Address::repeat_byte(0x22);
    let origin = 0x0102_0304_0506_0708;
    for (key, tag) in [
        (ModelKey::PortalConfig, model_tag::PORTAL_CONFIG),
        (ModelKey::ZoneConfig, model_tag::ZONE_CONFIG),
        (
            ModelKey::PortalDepositCursor,
            model_tag::PORTAL_DEPOSIT_CURSOR,
        ),
        (
            ModelKey::ZoneProcessedDepositCursor,
            model_tag::ZONE_PROCESSED_DEPOSIT_CURSOR,
        ),
        (ModelKey::PortalSettlement, model_tag::PORTAL_SETTLEMENT),
        (
            ModelKey::ZoneBatchAccumulator,
            model_tag::ZONE_BATCH_ACCUMULATOR,
        ),
        (
            ModelKey::ZoneNextWithdrawalIndex,
            model_tag::ZONE_NEXT_WITHDRAWAL_INDEX,
        ),
        (
            ModelKey::ZoneLastFallbackNonce,
            model_tag::ZONE_LAST_FALLBACK_NONCE,
        ),
    ] {
        assert_model_key(key, vec![tag]);
    }
    assert_model_key(
        ModelKey::Token(token),
        tagged(model_tag::TOKEN, &[token.as_slice()]),
    );
    for (key, tag) in [
        (ModelKey::PendingDeposit(origin), model_tag::PENDING_DEPOSIT),
        (ModelKey::Withdrawal(origin), model_tag::WITHDRAWAL),
        (ModelKey::FallbackOwner(origin), model_tag::FALLBACK_OWNER),
        (ModelKey::Batch(origin), model_tag::BATCH),
    ] {
        assert_model_key(key, tagged(tag, &[&origin.to_be_bytes()]));
    }
    for (key, tag) in [
        (
            ModelKey::PortalRefundCredit {
                token,
                recipient,
                origin,
            },
            model_tag::PORTAL_REFUND_CREDIT,
        ),
        (
            ModelKey::InboxRefundCredit {
                token,
                recipient,
                origin,
            },
            model_tag::INBOX_REFUND_CREDIT,
        ),
    ] {
        assert_model_key(
            key,
            tagged(
                tag,
                &[
                    token.as_slice(),
                    recipient.as_slice(),
                    &origin.to_be_bytes(),
                ],
            ),
        );
    }
}

#[test]
fn model_key_decode_rejects_unknown_short_and_trailing_bytes() {
    assert!(ModelKey::decode(&[]).is_err());
    assert!(ModelKey::decode(&[0xff]).is_err());
    assert!(ModelKey::decode(&[model_tag::PORTAL_CONFIG, 0]).is_err());
    assert!(ModelKey::decode(&[model_tag::TOKEN]).is_err());

    let mut trailing = bytes(ModelKey::PendingDeposit(1));
    trailing.push(0);
    assert!(ModelKey::decode(&trailing).is_err());

    let mut trailing = bytes(ModelKey::PortalRefundCredit {
        token: Address::ZERO,
        recipient: Address::ZERO,
        origin: 1,
    });
    trailing.push(0);
    assert!(ModelKey::decode(&trailing).is_err());
}

#[test]
fn model_key_ord_matches_encoded_byte_order() {
    let token = Address::repeat_byte(0x10);
    let recipient = Address::repeat_byte(0x20);
    let mut keys = [
        ModelKey::InboxRefundCredit {
            token,
            recipient,
            origin: 0,
        },
        ModelKey::PendingDeposit(256),
        ModelKey::Token(Address::repeat_byte(0x11)),
        ModelKey::PortalConfig,
        ModelKey::PendingDeposit(255),
        ModelKey::PortalRefundCredit {
            token,
            recipient,
            origin: u64::MAX,
        },
        ModelKey::Token(Address::repeat_byte(0x10)),
        ModelKey::Batch(0),
    ];
    keys.sort();
    let logically_sorted = keys.iter().copied().map(bytes).collect::<Vec<_>>();

    let mut byte_sorted = logically_sorted.clone();
    byte_sorted.sort();
    assert_eq!(logically_sorted, byte_sorted);
}

#[test]
fn changeset_key_is_fixed_width_big_endian_and_strict() {
    let key = ChangesetKey::new(0x0102_0304_0506_0708, B256::repeat_byte(0xaa), 0x0a0b_0c0d);
    let mut golden = Vec::new();
    golden.extend_from_slice(&0x0102_0304_0506_0708_u64.to_be_bytes());
    golden.extend_from_slice(B256::repeat_byte(0xaa).as_slice());
    golden.extend_from_slice(&0x0a0b_0c0d_u32.to_be_bytes());

    assert_eq!(bytes(key), golden);
    assert_eq!(ChangesetKey::decode(&golden).unwrap(), key);
    assert!(ChangesetKey::decode(&golden[..golden.len() - 1]).is_err());
    golden.push(0);
    assert!(ChangesetKey::decode(&golden).is_err());
}

#[test]
fn finding_key_is_fixed_width_big_endian_and_strict() {
    let key = FindingKey::new(256, B256::repeat_byte(0xbb), 513);
    let encoded = bytes(key);
    assert_eq!(&encoded[..U64_LEN], &256_u64.to_be_bytes());
    assert_eq!(
        &encoded[U64_LEN..U64_LEN + HASH_LEN],
        B256::repeat_byte(0xbb).as_slice()
    );
    assert_eq!(&encoded[U64_LEN + HASH_LEN..], &513_u32.to_be_bytes());
    assert_eq!(FindingKey::decode(&encoded).unwrap(), key);
    assert!(FindingKey::decode(&encoded[..encoded.len() - 1]).is_err());

    let mut trailing = encoded;
    trailing.push(0);
    assert!(FindingKey::decode(&trailing).is_err());
}

#[test]
fn block_and_ordinal_keys_order_across_byte_boundaries() {
    let hash_a = B256::repeat_byte(0x11);
    let hash_b = B256::repeat_byte(0x22);
    let changesets = [
        ChangesetKey::new(255, hash_b, u32::MAX),
        ChangesetKey::new(256, hash_a, 0),
        ChangesetKey::new(255, hash_a, 256),
        ChangesetKey::new(255, hash_a, 255),
    ];
    let findings = [
        FindingKey::new(255, hash_b, u32::MAX),
        FindingKey::new(256, hash_a, 0),
        FindingKey::new(255, hash_a, 256),
        FindingKey::new(255, hash_a, 255),
    ];

    assert_encoded_order(changesets);
    assert_encoded_order(findings);
}

fn assert_encoded_order<T>(mut keys: [T; 4])
where
    T: Encode + Ord + Copy,
{
    keys.sort();
    let logical = keys.map(bytes);
    let mut physical = logical.clone();
    physical.sort();
    assert_eq!(logical, physical);
}
