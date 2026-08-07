//! Read-only retained-history diagnostics for checker operators.

use std::{fmt, num::NonZeroU64, path::Path, str::FromStr};

use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, hex};
use reth_codecs::Compress;

use crate::store::{
    db::CheckerStore, diagnostic::HistoricalKeyChange, schema::ModelKey, value::ModelValue,
};

/// One typed selector for every release-one model-key family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticModelKey {
    /// `portal-config`.
    PortalConfig,
    /// `zone-config`.
    ZoneConfig,
    /// `portal-deposit-cursor`.
    PortalDepositCursor,
    /// `zone-processed-deposit-cursor`.
    ZoneProcessedDepositCursor,
    /// `portal-settlement`.
    PortalSettlement,
    /// `zone-batch-accumulator`.
    ZoneBatchAccumulator,
    /// `zone-next-withdrawal-index`.
    ZoneNextWithdrawalIndex,
    /// `zone-last-fallback-nonce`.
    ZoneLastFallbackNonce,
    /// `token:<address>`.
    Token(Address),
    /// `pending-deposit:<nonzero-number>`.
    PendingDeposit(NonZeroU64),
    /// `withdrawal:<index>`.
    Withdrawal(u64),
    /// `fallback-owner:<nonzero-nonce>`.
    FallbackOwner(NonZeroU64),
    /// `batch:<nonzero-index>`.
    Batch(NonZeroU64),
    /// `portal-refund-credit:<token>:<recipient>:<nonzero-origin>`.
    PortalRefundCredit {
        token: Address,
        recipient: Address,
        origin: NonZeroU64,
    },
    /// `inbox-refund-credit:<token>:<recipient>:<origin>`.
    InboxRefundCredit {
        token: Address,
        recipient: Address,
        origin: u64,
    },
}

impl DiagnosticModelKey {
    fn into_store_key(self) -> ModelKey {
        match self {
            Self::PortalConfig => ModelKey::PortalConfig,
            Self::ZoneConfig => ModelKey::ZoneConfig,
            Self::PortalDepositCursor => ModelKey::PortalDepositCursor,
            Self::ZoneProcessedDepositCursor => ModelKey::ZoneProcessedDepositCursor,
            Self::PortalSettlement => ModelKey::PortalSettlement,
            Self::ZoneBatchAccumulator => ModelKey::ZoneBatchAccumulator,
            Self::ZoneNextWithdrawalIndex => ModelKey::ZoneNextWithdrawalIndex,
            Self::ZoneLastFallbackNonce => ModelKey::ZoneLastFallbackNonce,
            Self::Token(token) => ModelKey::Token(token),
            Self::PendingDeposit(index) => ModelKey::PendingDeposit(index.get()),
            Self::Withdrawal(index) => ModelKey::Withdrawal(index),
            Self::FallbackOwner(nonce) => ModelKey::FallbackOwner(nonce.get()),
            Self::Batch(index) => ModelKey::Batch(index.get()),
            Self::PortalRefundCredit {
                token,
                recipient,
                origin,
            } => ModelKey::PortalRefundCredit {
                token,
                recipient,
                origin: origin.get(),
            },
            Self::InboxRefundCredit {
                token,
                recipient,
                origin,
            } => ModelKey::InboxRefundCredit {
                token,
                recipient,
                origin,
            },
        }
    }
}

impl FromStr for DiagnosticModelKey {
    type Err = eyre::Report;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let parts = value.split(':').collect::<Vec<_>>();
        match parts.as_slice() {
            ["portal-config"] => Ok(Self::PortalConfig),
            ["zone-config"] => Ok(Self::ZoneConfig),
            ["portal-deposit-cursor"] => Ok(Self::PortalDepositCursor),
            ["zone-processed-deposit-cursor"] => Ok(Self::ZoneProcessedDepositCursor),
            ["portal-settlement"] => Ok(Self::PortalSettlement),
            ["zone-batch-accumulator"] => Ok(Self::ZoneBatchAccumulator),
            ["zone-next-withdrawal-index"] => Ok(Self::ZoneNextWithdrawalIndex),
            ["zone-last-fallback-nonce"] => Ok(Self::ZoneLastFallbackNonce),
            ["token", token] => Ok(Self::Token(parse_address(token, "token")?)),
            ["pending-deposit", index] => {
                Ok(Self::PendingDeposit(parse_nonzero_index(index, "deposit")?))
            }
            ["withdrawal", index] => Ok(Self::Withdrawal(parse_index(index, "withdrawal")?)),
            ["fallback-owner", nonce] => Ok(Self::FallbackOwner(parse_nonzero_index(
                nonce,
                "fallback nonce",
            )?)),
            ["batch", index] => Ok(Self::Batch(parse_nonzero_index(index, "batch")?)),
            ["portal-refund-credit", token, recipient, origin] => Ok(Self::PortalRefundCredit {
                token: parse_address(token, "token")?,
                recipient: parse_address(recipient, "recipient")?,
                origin: parse_nonzero_index(origin, "refund origin")?,
            }),
            ["inbox-refund-credit", token, recipient, origin] => Ok(Self::InboxRefundCredit {
                token: parse_address(token, "token")?,
                recipient: parse_address(recipient, "recipient")?,
                origin: parse_index(origin, "refund origin")?,
            }),
            _ => Err(eyre::eyre!(
                "invalid checker model key `{value}`; use a singleton name, \
                 token:<address>, pending-deposit:<nonzero-number>, withdrawal:<index>, \
                 fallback-owner:<nonzero-nonce>, batch:<nonzero-index>, \
                 portal-refund-credit:<token>:<recipient>:<nonzero-origin>, or \
                 inbox-refund-credit:<token>:<recipient>:<withdrawal-index>"
            )),
        }
    }
}

impl fmt::Display for DiagnosticModelKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PortalConfig => f.write_str("portal-config"),
            Self::ZoneConfig => f.write_str("zone-config"),
            Self::PortalDepositCursor => f.write_str("portal-deposit-cursor"),
            Self::ZoneProcessedDepositCursor => f.write_str("zone-processed-deposit-cursor"),
            Self::PortalSettlement => f.write_str("portal-settlement"),
            Self::ZoneBatchAccumulator => f.write_str("zone-batch-accumulator"),
            Self::ZoneNextWithdrawalIndex => f.write_str("zone-next-withdrawal-index"),
            Self::ZoneLastFallbackNonce => f.write_str("zone-last-fallback-nonce"),
            Self::Token(token) => write!(f, "token:{token}"),
            Self::PendingDeposit(index) => write!(f, "pending-deposit:{index}"),
            Self::Withdrawal(index) => write!(f, "withdrawal:{index}"),
            Self::FallbackOwner(nonce) => write!(f, "fallback-owner:{nonce}"),
            Self::Batch(index) => write!(f, "batch:{index}"),
            Self::PortalRefundCredit {
                token,
                recipient,
                origin,
            } => write!(f, "portal-refund-credit:{token}:{recipient}:{origin}"),
            Self::InboxRefundCredit {
                token,
                recipient,
                origin,
            } => write!(f, "inbox-refund-credit:{token}:{recipient}:{origin}"),
        }
    }
}

/// Exact persisted bytes plus an operator-readable typed decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticValue {
    /// Canonical checker MDBX value bytes, hex encoded with a `0x` prefix.
    pub encoded: String,
    /// Complete typed value rendered for an operator.
    pub decoded: String,
}

impl DiagnosticValue {
    fn from_model(value: ModelValue) -> Self {
        let decoded = format!("{value:?}");
        Self {
            encoded: format!("0x{}", hex::encode(value.compress())),
            decoded,
        }
    }
}

/// One selected key at the exact canonical block boundary before and after a target block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedModelChange {
    /// Selected typed model key.
    pub key: DiagnosticModelKey,
    /// Exact canonical Zone parent boundary.
    pub zone_before: BlockNumHash,
    /// Exact canonical target Zone block.
    pub zone_after: BlockNumHash,
    /// Imported Tempo tip at the Zone parent boundary.
    pub tempo_before: BlockNumHash,
    /// Imported Tempo tip at the target Zone boundary.
    pub tempo_after: BlockNumHash,
    /// Target changeset ordinal, or `None` when the selected key did not change.
    pub changeset_ordinal: Option<u32>,
    /// Selected value at the Zone parent boundary.
    pub before: Option<DiagnosticValue>,
    /// Selected value at the target Zone boundary.
    pub after: Option<DiagnosticValue>,
}

impl fmt::Display for RetainedModelChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "model key: {}", self.key)?;
        writeln!(
            f,
            "Zone before: {} / {}",
            self.zone_before.number, self.zone_before.hash
        )?;
        writeln!(
            f,
            "Zone after:  {} / {}",
            self.zone_after.number, self.zone_after.hash
        )?;
        writeln!(
            f,
            "Tempo before: {} / {}",
            self.tempo_before.number, self.tempo_before.hash
        )?;
        writeln!(
            f,
            "Tempo after:  {} / {}",
            self.tempo_after.number, self.tempo_after.hash
        )?;
        match self.changeset_ordinal {
            Some(ordinal) => writeln!(f, "changeset ordinal: {ordinal}")?,
            None => writeln!(f, "changeset ordinal: unchanged in target block")?,
        }
        write_value(f, "before", self.before.as_ref())?;
        write_value(f, "after", self.after.as_ref())
    }
}

/// Inspect an initialized checker database without acquiring a writer or mutating history.
///
/// The Zone node must be stopped before this opens its checker MDBX environment. A consistent
/// offline copy is also acceptable. Target height zero fails explicitly because it has no retained
/// parent boundary with which to form a before/after report.
pub fn diagnose_retained_model_change(
    database_path: impl AsRef<Path>,
    target_zone_height: u64,
    key: DiagnosticModelKey,
) -> eyre::Result<RetainedModelChange> {
    let store = CheckerStore::open_diagnostic_at(database_path)?;
    let change = store.diagnose_key(target_zone_height, key.into_store_key())?;
    Ok(from_store_change(key, change))
}

fn from_store_change(key: DiagnosticModelKey, change: HistoricalKeyChange) -> RetainedModelChange {
    RetainedModelChange {
        key,
        zone_before: change.zone_before,
        zone_after: change.zone_after,
        tempo_before: change.tempo_before,
        tempo_after: change.tempo_after,
        changeset_ordinal: change.changeset_ordinal,
        before: change.before.map(DiagnosticValue::from_model),
        after: change.after.map(DiagnosticValue::from_model),
    }
}

fn parse_address(value: &str, field: &'static str) -> eyre::Result<Address> {
    value
        .parse()
        .map_err(|error| eyre::eyre!("invalid {field} address `{value}`: {error}"))
}

fn parse_index(value: &str, field: &'static str) -> eyre::Result<u64> {
    value
        .parse()
        .map_err(|error| eyre::eyre!("invalid {field} `{value}`: {error}"))
}

fn parse_nonzero_index(value: &str, field: &'static str) -> eyre::Result<NonZeroU64> {
    value
        .parse()
        .map_err(|error| eyre::eyre!("invalid {field} `{value}`: {error}"))
}

fn write_value(
    f: &mut fmt::Formatter<'_>,
    label: &'static str,
    value: Option<&DiagnosticValue>,
) -> fmt::Result {
    match value {
        Some(value) => {
            writeln!(f, "{label} decoded: {}", value.decoded)?;
            writeln!(f, "{label} encoded: {}", value.encoded)
        }
        None => {
            writeln!(f, "{label} decoded: <absent>")?;
            writeln!(f, "{label} encoded: <absent>")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0x1111111111111111111111111111111111111111";
    const RECIPIENT: &str = "0x2222222222222222222222222222222222222222";

    #[test]
    fn every_model_key_selector_round_trips() {
        let selectors = [
            "portal-config".to_owned(),
            "zone-config".to_owned(),
            "portal-deposit-cursor".to_owned(),
            "zone-processed-deposit-cursor".to_owned(),
            "portal-settlement".to_owned(),
            "zone-batch-accumulator".to_owned(),
            "zone-next-withdrawal-index".to_owned(),
            "zone-last-fallback-nonce".to_owned(),
            format!("token:{TOKEN}"),
            "pending-deposit:1".to_owned(),
            "withdrawal:0".to_owned(),
            "fallback-owner:3".to_owned(),
            "batch:4".to_owned(),
            format!("portal-refund-credit:{TOKEN}:{RECIPIENT}:5"),
            format!("inbox-refund-credit:{TOKEN}:{RECIPIENT}:0"),
        ];

        for selector in selectors {
            let parsed = selector.parse::<DiagnosticModelKey>().unwrap();
            assert_eq!(
                parsed.to_string().parse::<DiagnosticModelKey>().unwrap(),
                parsed
            );
        }
    }

    #[test]
    fn malformed_model_key_selectors_fail_explicitly() {
        for selector in [
            "token",
            "token:not-an-address",
            "withdrawal:not-a-number",
            "pending-deposit:0",
            "fallback-owner:0",
            "batch:0",
            "portal-refund-credit:too:few",
            "unknown",
        ] {
            assert!(
                selector.parse::<DiagnosticModelKey>().is_err(),
                "{selector}"
            );
        }
        let zero_portal_origin = format!("portal-refund-credit:{TOKEN}:{RECIPIENT}:0");
        assert!(
            zero_portal_origin.parse::<DiagnosticModelKey>().is_err(),
            "{zero_portal_origin}"
        );
    }
}
