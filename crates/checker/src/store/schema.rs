//! Ordered checker database keys and the five Goal 6 tables.

use std::fmt;

use alloy_primitives::{Address, B256};
use reth_codecs::{Compress, Decompress, DecompressError};
use reth_db::{
    DatabaseError, TableSet, TableType, TableViewer,
    table::{Decode, Encode, TableInfo},
    tables,
};
use serde::{Deserialize, Serialize};

use super::value::{BeforeImage, FindingRecord, MetaValue, ModelValue};

pub(crate) mod meta_tag {
    pub(crate) const VERSION: u8 = 0x00;
    pub(crate) const ZONE_IDENTITY: u8 = 0x01;
    pub(crate) const L1_CHAIN_ID: u8 = 0x02;
    pub(crate) const CONTRACTS: u8 = 0x03;
    pub(crate) const PORTAL_CREATION_BLOCK: u8 = 0x04;
    pub(crate) const BOOTSTRAP: u8 = 0x05;
    pub(crate) const VERIFIED_ZONE_TIP: u8 = 0x06;
    pub(crate) const IMPORTED_TEMPO_TIP: u8 = 0x07;
    pub(crate) const ACTIVE_ALERT: u8 = 0x08;
}

pub(crate) mod model_tag {
    pub(crate) const PORTAL_CONFIG: u8 = 0x00;
    pub(crate) const ZONE_CONFIG: u8 = 0x01;
    pub(crate) const PORTAL_DEPOSIT_CURSOR: u8 = 0x02;
    pub(crate) const ZONE_PROCESSED_DEPOSIT_CURSOR: u8 = 0x03;
    pub(crate) const PORTAL_SETTLEMENT: u8 = 0x04;
    pub(crate) const ZONE_BATCH_ACCUMULATOR: u8 = 0x05;
    pub(crate) const ZONE_NEXT_WITHDRAWAL_INDEX: u8 = 0x06;
    pub(crate) const ZONE_LAST_FALLBACK_NONCE: u8 = 0x07;
    pub(crate) const TOKEN: u8 = 0x20;
    pub(crate) const PENDING_DEPOSIT: u8 = 0x30;
    pub(crate) const WITHDRAWAL: u8 = 0x40;
    pub(crate) const FALLBACK_OWNER: u8 = 0x50;
    pub(crate) const BATCH: u8 = 0x60;
    pub(crate) const PORTAL_REFUND_CREDIT: u8 = 0x70;
    pub(crate) const INBOX_REFUND_CREDIT: u8 = 0x71;
}

const TAG_LEN: usize = 1;
const ADDRESS_LEN: usize = 20;
const U64_LEN: usize = size_of::<u64>();
const U32_LEN: usize = size_of::<u32>();
const HASH_LEN: usize = 32;

pub(crate) const INDEXED_MODEL_KEY_LEN: usize = TAG_LEN + U64_LEN;
pub(crate) const REFUND_MODEL_KEY_LEN: usize = TAG_LEN + ADDRESS_LEN + ADDRESS_LEN + U64_LEN;
pub(crate) const BLOCK_ORDINAL_KEY_LEN: usize = U64_LEN + HASH_LEN + U32_LEN;

/// Singleton metadata keys. The tag byte is the complete encoded key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MetaKey {
    Version,
    ZoneIdentity,
    L1ChainId,
    Contracts,
    PortalCreationBlock,
    Bootstrap,
    VerifiedZoneTip,
    ImportedTempoTip,
    ActiveAlert,
}

impl MetaKey {
    const fn tag(self) -> u8 {
        match self {
            Self::Version => meta_tag::VERSION,
            Self::ZoneIdentity => meta_tag::ZONE_IDENTITY,
            Self::L1ChainId => meta_tag::L1_CHAIN_ID,
            Self::Contracts => meta_tag::CONTRACTS,
            Self::PortalCreationBlock => meta_tag::PORTAL_CREATION_BLOCK,
            Self::Bootstrap => meta_tag::BOOTSTRAP,
            Self::VerifiedZoneTip => meta_tag::VERIFIED_ZONE_TIP,
            Self::ImportedTempoTip => meta_tag::IMPORTED_TEMPO_TIP,
            Self::ActiveAlert => meta_tag::ACTIVE_ALERT,
        }
    }
}

impl Encode for MetaKey {
    type Encoded = [u8; TAG_LEN];

    fn encode(self) -> Self::Encoded {
        [self.tag()]
    }
}

impl Decode for MetaKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        if value.len() != TAG_LEN {
            return Err(DatabaseError::Decode);
        }
        match value[0] {
            meta_tag::VERSION => Ok(Self::Version),
            meta_tag::ZONE_IDENTITY => Ok(Self::ZoneIdentity),
            meta_tag::L1_CHAIN_ID => Ok(Self::L1ChainId),
            meta_tag::CONTRACTS => Ok(Self::Contracts),
            meta_tag::PORTAL_CREATION_BLOCK => Ok(Self::PortalCreationBlock),
            meta_tag::BOOTSTRAP => Ok(Self::Bootstrap),
            meta_tag::VERIFIED_ZONE_TIP => Ok(Self::VerifiedZoneTip),
            meta_tag::IMPORTED_TEMPO_TIP => Ok(Self::ImportedTempoTip),
            meta_tag::ACTIVE_ALERT => Ok(Self::ActiveAlert),
            _ => Err(DatabaseError::Decode),
        }
    }
}

/// Ordered keys for the flattened authoritative model state.
///
/// Declaration order, tag order, field order, and encoded byte order are kept
/// identical because Reth's range walker compares decoded end keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ModelKey {
    PortalConfig,
    ZoneConfig,
    PortalDepositCursor,
    ZoneProcessedDepositCursor,
    PortalSettlement,
    ZoneBatchAccumulator,
    ZoneNextWithdrawalIndex,
    ZoneLastFallbackNonce,
    Token(Address),
    PendingDeposit(u64),
    Withdrawal(u64),
    FallbackOwner(u64),
    Batch(u64),
    PortalRefundCredit {
        token: Address,
        recipient: Address,
        origin: u64,
    },
    InboxRefundCredit {
        token: Address,
        recipient: Address,
        origin: u64,
    },
}

impl Encode for ModelKey {
    type Encoded = Vec<u8>;

    fn encode(self) -> Self::Encoded {
        let mut encoded = Vec::with_capacity(match self {
            Self::PortalConfig
            | Self::ZoneConfig
            | Self::PortalDepositCursor
            | Self::ZoneProcessedDepositCursor
            | Self::PortalSettlement
            | Self::ZoneBatchAccumulator
            | Self::ZoneNextWithdrawalIndex
            | Self::ZoneLastFallbackNonce => TAG_LEN,
            Self::Token(_) => TAG_LEN + ADDRESS_LEN,
            Self::PendingDeposit(_)
            | Self::Withdrawal(_)
            | Self::FallbackOwner(_)
            | Self::Batch(_) => INDEXED_MODEL_KEY_LEN,
            Self::PortalRefundCredit { .. } | Self::InboxRefundCredit { .. } => {
                REFUND_MODEL_KEY_LEN
            }
        });

        match self {
            Self::PortalConfig => encoded.push(model_tag::PORTAL_CONFIG),
            Self::ZoneConfig => encoded.push(model_tag::ZONE_CONFIG),
            Self::PortalDepositCursor => encoded.push(model_tag::PORTAL_DEPOSIT_CURSOR),
            Self::ZoneProcessedDepositCursor => {
                encoded.push(model_tag::ZONE_PROCESSED_DEPOSIT_CURSOR);
            }
            Self::PortalSettlement => encoded.push(model_tag::PORTAL_SETTLEMENT),
            Self::ZoneBatchAccumulator => encoded.push(model_tag::ZONE_BATCH_ACCUMULATOR),
            Self::ZoneNextWithdrawalIndex => encoded.push(model_tag::ZONE_NEXT_WITHDRAWAL_INDEX),
            Self::ZoneLastFallbackNonce => encoded.push(model_tag::ZONE_LAST_FALLBACK_NONCE),
            Self::Token(token) => {
                encoded.push(model_tag::TOKEN);
                encoded.extend_from_slice(token.as_slice());
            }
            Self::PendingDeposit(index) => {
                encode_indexed(&mut encoded, model_tag::PENDING_DEPOSIT, index);
            }
            Self::Withdrawal(index) => {
                encode_indexed(&mut encoded, model_tag::WITHDRAWAL, index);
            }
            Self::FallbackOwner(nonce) => {
                encode_indexed(&mut encoded, model_tag::FALLBACK_OWNER, nonce);
            }
            Self::Batch(index) => encode_indexed(&mut encoded, model_tag::BATCH, index),
            Self::PortalRefundCredit {
                token,
                recipient,
                origin,
            } => encode_refund(
                &mut encoded,
                model_tag::PORTAL_REFUND_CREDIT,
                token,
                recipient,
                origin,
            ),
            Self::InboxRefundCredit {
                token,
                recipient,
                origin,
            } => encode_refund(
                &mut encoded,
                model_tag::INBOX_REFUND_CREDIT,
                token,
                recipient,
                origin,
            ),
        }
        encoded
    }
}

impl Decode for ModelKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let (&tag, payload) = value.split_first().ok_or(DatabaseError::Decode)?;
        match tag {
            model_tag::PORTAL_CONFIG => decode_singleton(payload, Self::PortalConfig),
            model_tag::ZONE_CONFIG => decode_singleton(payload, Self::ZoneConfig),
            model_tag::PORTAL_DEPOSIT_CURSOR => {
                decode_singleton(payload, Self::PortalDepositCursor)
            }
            model_tag::ZONE_PROCESSED_DEPOSIT_CURSOR => {
                decode_singleton(payload, Self::ZoneProcessedDepositCursor)
            }
            model_tag::PORTAL_SETTLEMENT => decode_singleton(payload, Self::PortalSettlement),
            model_tag::ZONE_BATCH_ACCUMULATOR => {
                decode_singleton(payload, Self::ZoneBatchAccumulator)
            }
            model_tag::ZONE_NEXT_WITHDRAWAL_INDEX => {
                decode_singleton(payload, Self::ZoneNextWithdrawalIndex)
            }
            model_tag::ZONE_LAST_FALLBACK_NONCE => {
                decode_singleton(payload, Self::ZoneLastFallbackNonce)
            }
            model_tag::TOKEN => {
                require_len(payload, ADDRESS_LEN)?;
                Ok(Self::Token(Address::from_slice(payload)))
            }
            model_tag::PENDING_DEPOSIT => Ok(Self::PendingDeposit(decode_u64(payload)?)),
            model_tag::WITHDRAWAL => Ok(Self::Withdrawal(decode_u64(payload)?)),
            model_tag::FALLBACK_OWNER => Ok(Self::FallbackOwner(decode_u64(payload)?)),
            model_tag::BATCH => Ok(Self::Batch(decode_u64(payload)?)),
            model_tag::PORTAL_REFUND_CREDIT => {
                let (token, recipient, origin) = decode_refund(payload)?;
                Ok(Self::PortalRefundCredit {
                    token,
                    recipient,
                    origin,
                })
            }
            model_tag::INBOX_REFUND_CREDIT => {
                let (token, recipient, origin) = decode_refund(payload)?;
                Ok(Self::InboxRefundCredit {
                    token,
                    recipient,
                    origin,
                })
            }
            _ => Err(DatabaseError::Decode),
        }
    }
}

/// One ordered changeset row. Ordinal zero is reserved for block metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChangesetKey {
    pub(crate) zone_height: u64,
    pub(crate) block_hash: B256,
    pub(crate) ordinal: u32,
}

impl ChangesetKey {
    pub(crate) const fn new(zone_height: u64, block_hash: B256, ordinal: u32) -> Self {
        Self {
            zone_height,
            block_hash,
            ordinal,
        }
    }
}

impl Encode for ChangesetKey {
    type Encoded = [u8; BLOCK_ORDINAL_KEY_LEN];

    fn encode(self) -> Self::Encoded {
        encode_block_ordinal(self.zone_height, self.block_hash, self.ordinal)
    }
}

impl Decode for ChangesetKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let (zone_height, block_hash, ordinal) = decode_block_ordinal(value)?;
        Ok(Self::new(zone_height, block_hash, ordinal))
    }
}

/// One ordered durable finding row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FindingKey {
    pub(crate) zone_height: u64,
    pub(crate) zone_hash: B256,
    pub(crate) ordinal: u32,
}

impl FindingKey {
    pub(crate) const fn new(zone_height: u64, zone_hash: B256, ordinal: u32) -> Self {
        Self {
            zone_height,
            zone_hash,
            ordinal,
        }
    }

    pub(crate) const fn zone_height(self) -> u64 {
        self.zone_height
    }

    pub(crate) const fn zone_hash(self) -> B256 {
        self.zone_hash
    }

    pub(crate) const fn ordinal(self) -> u32 {
        self.ordinal
    }
}

impl Encode for FindingKey {
    type Encoded = [u8; BLOCK_ORDINAL_KEY_LEN];

    fn encode(self) -> Self::Encoded {
        encode_block_ordinal(self.zone_height, self.zone_hash, self.ordinal)
    }
}

impl Decode for FindingKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let (zone_height, zone_hash, ordinal) = decode_block_ordinal(value)?;
        Ok(Self::new(zone_height, zone_hash, ordinal))
    }
}

/// Exact-width canonical block hash.
///
/// Reth's blanket `B256` decompressor ignores a trailing remainder. Keeping a
/// local wrapper makes malformed canonical-index rows fail closed instead of
/// accepting a byte prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CanonicalHash(B256);

impl CanonicalHash {
    pub(crate) const fn new(hash: B256) -> Self {
        Self(hash)
    }

    pub(crate) const fn into_inner(self) -> B256 {
        self.0
    }
}

impl Compress for CanonicalHash {
    type Compressed = Vec<u8>;

    fn uncompressable_ref(&self) -> Option<&[u8]> {
        Some(self.0.as_slice())
    }

    fn compress_to_buf<B: bytes::BufMut + AsMut<[u8]>>(&self, buf: &mut B) {
        bytes::BufMut::put_slice(buf, self.0.as_slice());
    }
}

impl Decompress for CanonicalHash {
    fn decompress(value: &[u8]) -> Result<Self, DecompressError> {
        require_len(value, HASH_LEN)
            .map_err(|error| DecompressError::new(std::io::Error::other(error)))?;
        Ok(Self(B256::from_slice(value)))
    }
}

tables! {
    table CheckerMeta {
        type Key = MetaKey;
        type Value = MetaValue;
    }

    table CheckerCanonical {
        type Key = u64;
        type Value = CanonicalHash;
    }

    table CheckerModelState {
        type Key = ModelKey;
        type Value = ModelValue;
    }

    table CheckerChangesets {
        type Key = ChangesetKey;
        type Value = BeforeImage;
    }

    table CheckerFindings {
        type Key = FindingKey;
        type Value = FindingRecord;
    }
}

pub use Tables as CheckerTables;

fn require_len(value: &[u8], expected: usize) -> Result<(), DatabaseError> {
    if value.len() == expected {
        Ok(())
    } else {
        Err(DatabaseError::Decode)
    }
}

fn decode_singleton(payload: &[u8], key: ModelKey) -> Result<ModelKey, DatabaseError> {
    require_len(payload, 0)?;
    Ok(key)
}

fn encode_indexed(encoded: &mut Vec<u8>, tag: u8, index: u64) {
    encoded.push(tag);
    encoded.extend_from_slice(&index.to_be_bytes());
}

fn decode_u64(payload: &[u8]) -> Result<u64, DatabaseError> {
    require_len(payload, U64_LEN)?;
    Ok(u64::from_be_bytes(
        payload.try_into().map_err(|_| DatabaseError::Decode)?,
    ))
}

fn encode_refund(encoded: &mut Vec<u8>, tag: u8, token: Address, recipient: Address, origin: u64) {
    encoded.push(tag);
    encoded.extend_from_slice(token.as_slice());
    encoded.extend_from_slice(recipient.as_slice());
    encoded.extend_from_slice(&origin.to_be_bytes());
}

fn decode_refund(payload: &[u8]) -> Result<(Address, Address, u64), DatabaseError> {
    require_len(payload, REFUND_MODEL_KEY_LEN - TAG_LEN)?;
    let token = Address::from_slice(&payload[..ADDRESS_LEN]);
    let recipient = Address::from_slice(&payload[ADDRESS_LEN..ADDRESS_LEN * 2]);
    let origin = decode_u64(&payload[ADDRESS_LEN * 2..])?;
    Ok((token, recipient, origin))
}

fn encode_block_ordinal(zone_height: u64, hash: B256, ordinal: u32) -> [u8; BLOCK_ORDINAL_KEY_LEN] {
    let mut encoded = [0_u8; BLOCK_ORDINAL_KEY_LEN];
    encoded[..U64_LEN].copy_from_slice(&zone_height.to_be_bytes());
    encoded[U64_LEN..U64_LEN + HASH_LEN].copy_from_slice(hash.as_slice());
    encoded[U64_LEN + HASH_LEN..].copy_from_slice(&ordinal.to_be_bytes());
    encoded
}

fn decode_block_ordinal(value: &[u8]) -> Result<(u64, B256, u32), DatabaseError> {
    require_len(value, BLOCK_ORDINAL_KEY_LEN)?;
    let zone_height = u64::from_be_bytes(
        value[..U64_LEN]
            .try_into()
            .map_err(|_| DatabaseError::Decode)?,
    );
    let block_hash = B256::from_slice(&value[U64_LEN..U64_LEN + HASH_LEN]);
    let ordinal = u32::from_be_bytes(
        value[U64_LEN + HASH_LEN..]
            .try_into()
            .map_err(|_| DatabaseError::Decode)?,
    );
    Ok((zone_height, block_hash, ordinal))
}

#[cfg(test)]
mod tests;
