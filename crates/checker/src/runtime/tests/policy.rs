//! Runtime failure-policy tests.

use super::*;

#[test]
fn observation_errors_map_to_runtime_policy_and_findings() {
    let transaction =
        AuthenticatedTransaction::new(ProtocolChain::ZoneL2, 3, B256::repeat_byte(0x31));
    let evidence = AuthenticatedDataEvidence::from_bytes(b"malformed authenticated bytes");
    let cases = [
        (
            "unavailable acquisition",
            AcquisitionError::unavailable(AcquisitionSource::L1Block, "offline").into(),
            FailureClass::TransientRetry,
            CoverageGapReason::ProviderUnavailable,
            None,
        ),
        (
            "missing acquisition",
            AcquisitionError::missing(AcquisitionSource::L1Receipts, "block").into(),
            FailureClass::BoundedRetry,
            CoverageGapReason::MissingTempoData,
            None,
        ),
        (
            "inconsistent acquisition",
            AcquisitionError::inconsistent(AcquisitionSource::L1Transaction, "expected", "actual")
                .into(),
            FailureClass::BoundedRetry,
            CoverageGapReason::MissingTempoData,
            None,
        ),
        (
            "malformed authenticated data",
            ObservationError::malformed(
                DataSource::AdvanceTempoCalldata,
                transaction,
                evidence,
                "bad encoding",
            ),
            FailureClass::AuthenticatedDivergence,
            CoverageGapReason::NotCheckedAncestorDivergence,
            Some((
                110,
                FindingLocation::Operation(3),
                Datum::Bytes {
                    length: evidence.length(),
                    digest: evidence.digest(),
                },
            )),
        ),
        (
            "invalid envelope",
            ObservationError::invalid_envelope(3, EnvelopeRule::AdvanceSystemCaller),
            FailureClass::AuthenticatedDivergence,
            CoverageGapReason::NotCheckedAncestorDivergence,
            Some((120, FindingLocation::Block, Datum::Code(120))),
        ),
        (
            "protocol event",
            ObservationError::protocol_event(
                ProtocolChain::TempoL1,
                3,
                1,
                2,
                B256::repeat_byte(0x32),
                ProtocolEventError::UnsupportedProtocolEvent {
                    emitter: Address::repeat_byte(0x33),
                    topic0: None,
                },
            ),
            FailureClass::AuthenticatedDivergence,
            CoverageGapReason::NotCheckedAncestorDivergence,
            Some((130, FindingLocation::Operation(3), Datum::Code(130))),
        ),
        (
            "portal call",
            PortalCallError::ConflictingFamilies {
                transaction_hash: B256::repeat_byte(0x34),
            }
            .into(),
            FailureClass::AuthenticatedDivergence,
            CoverageGapReason::NotCheckedAncestorDivergence,
            Some((140, FindingLocation::Block, Datum::Code(140))),
        ),
    ];

    for (name, error, class, gap_reason, expected_finding) in cases {
        let failure = Failure::from(error);
        assert_eq!(failure.class, class, "{name}");
        assert_eq!(failure.gap_reason, gap_reason, "{name}");
        assert!(!failure.message.is_empty(), "{name}");
        match (failure.finding.as_deref(), expected_finding) {
            (None, None) => {}
            (Some(finding), Some((code, location, actual))) => {
                assert_eq!(finding.category, FindingCategory::Observation, "{name}");
                assert_eq!(finding.code, code, "{name}");
                assert_eq!(finding.location.as_ref(), Some(&location), "{name}");
                assert_eq!(finding.expected, None, "{name}");
                assert_eq!(finding.actual.as_ref(), Some(&actual), "{name}");
            }
            (actual, expected) => panic!("{name}: finding mismatch: {actual:?} != {expected:?}"),
        }
    }
}
