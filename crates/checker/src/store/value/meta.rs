use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256};

use crate::{
    model::state::PortalIdentity,
    store::{
        SCHEMA_VERSION,
        codec::{CheckedCompact, CodecError, Decoder, Encoder, impl_value_codec},
        schema::{FindingKey, MetaKey},
    },
};

/// Stable envelope for the version row. Unlike every other metadata value,
/// this prefix must remain decodable after `SCHEMA_VERSION` changes so startup
/// can report the old version and the required rebuild path.
const VERSION_RECORD_PREFIX: u8 = 0;
const _: () = assert!(SCHEMA_VERSION != VERSION_RECORD_PREFIX);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapState {
    L1Replay { cursor: Option<BlockNumHash> },
    ZoneReplay { cursor: BlockNumHash },
    Live,
}

impl BootstrapState {
    pub(crate) const fn l1_replay(cursor: Option<BlockNumHash>) -> Self {
        Self::L1Replay { cursor }
    }

    pub(crate) const fn zone_replay(cursor: BlockNumHash) -> Self {
        Self::ZoneReplay { cursor }
    }

    pub(crate) const fn live() -> Self {
        Self::Live
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveAlert {
    pub(crate) finding: FindingKey,
    pub(crate) last_verified_parent: BlockNumHash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreIdentity {
    zone_chain_id: u64,
    zone_genesis_hash: B256,
    portal_identity: PortalIdentity,
    l1_chain_id: u64,
    zone_factory: Address,
    portal_creation_block_hash: B256,
}

impl StoreIdentity {
    pub(crate) const fn new(
        zone_chain_id: u64,
        zone_genesis_hash: B256,
        portal_identity: PortalIdentity,
        l1_chain_id: u64,
        zone_factory: Address,
        portal_creation_block_hash: B256,
    ) -> Self {
        Self {
            zone_chain_id,
            zone_genesis_hash,
            portal_identity,
            l1_chain_id,
            zone_factory,
            portal_creation_block_hash,
        }
    }

    pub(crate) const fn portal_identity(self) -> PortalIdentity {
        self.portal_identity
    }

    pub(crate) const fn zone_genesis_hash(self) -> B256 {
        self.zone_genesis_hash
    }

    pub(crate) const fn portal_creation_block_hash(self) -> B256 {
        self.portal_creation_block_hash
    }

    pub(crate) fn metadata(self) -> [(MetaKey, MetaValue); 5] {
        [
            (
                MetaKey::Version,
                MetaValue::Version(u32::from(SCHEMA_VERSION)),
            ),
            (
                MetaKey::ZoneIdentity,
                MetaValue::ZoneIdentity {
                    chain_id: self.zone_chain_id,
                    genesis_hash: self.zone_genesis_hash,
                    zone_id: self.portal_identity.zone_id(),
                    initial_token: self.portal_identity.initial_token(),
                },
            ),
            (MetaKey::L1ChainId, MetaValue::L1ChainId(self.l1_chain_id)),
            (
                MetaKey::Contracts,
                MetaValue::Contracts {
                    zone_factory: self.zone_factory,
                    portal: self.portal_identity.portal(),
                },
            ),
            (
                MetaKey::PortalCreationBlockHash,
                MetaValue::PortalCreationBlockHash(self.portal_creation_block_hash),
            ),
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetaValue {
    Version(u32),
    ZoneIdentity {
        chain_id: u64,
        genesis_hash: B256,
        zone_id: u32,
        initial_token: Address,
    },
    L1ChainId(u64),
    Contracts {
        zone_factory: Address,
        portal: Address,
    },
    PortalCreationBlockHash(B256),
    Bootstrap(BootstrapState),
    VerifiedZoneTip(BlockNumHash),
    ImportedTempoTip(BlockNumHash),
    ActiveAlert(ActiveAlert),
}

impl MetaValue {
    pub(crate) const fn matches_key(&self, key: MetaKey) -> bool {
        matches!(
            (key, self),
            (MetaKey::Version, Self::Version(_))
                | (MetaKey::ZoneIdentity, Self::ZoneIdentity { .. })
                | (MetaKey::L1ChainId, Self::L1ChainId(_))
                | (MetaKey::Contracts, Self::Contracts { .. })
                | (
                    MetaKey::PortalCreationBlockHash,
                    Self::PortalCreationBlockHash(_)
                )
                | (MetaKey::Bootstrap, Self::Bootstrap(_))
                | (MetaKey::VerifiedZoneTip, Self::VerifiedZoneTip(_))
                | (MetaKey::ImportedTempoTip, Self::ImportedTempoTip(_))
                | (MetaKey::ActiveAlert, Self::ActiveAlert(_))
        )
    }
}

impl CheckedCompact for MetaValue {
    fn encode_checked(&self, out: &mut Encoder) {
        match self {
            Self::Version(version) => {
                out.u8(VERSION_RECORD_PREFIX);
                out.u32(*version);
            }
            Self::ZoneIdentity {
                chain_id,
                genesis_hash,
                zone_id,
                initial_token,
            } => {
                out.version();
                out.u8(0x01);
                out.u64(*chain_id);
                out.hash(*genesis_hash);
                out.u32(*zone_id);
                out.address(*initial_token);
            }
            Self::L1ChainId(chain_id) => {
                out.version();
                out.u8(0x02);
                out.u64(*chain_id);
            }
            Self::Contracts {
                zone_factory,
                portal,
            } => {
                out.version();
                out.u8(0x03);
                out.address(*zone_factory);
                out.address(*portal);
            }
            Self::PortalCreationBlockHash(hash) => {
                out.version();
                out.u8(0x04);
                out.hash(*hash);
            }
            Self::Bootstrap(state) => {
                out.version();
                out.u8(0x05);
                encode_bootstrap(out, *state);
            }
            Self::VerifiedZoneTip(tip) => {
                out.version();
                out.u8(0x06);
                encode_tip(out, *tip);
            }
            Self::ImportedTempoTip(tip) => {
                out.version();
                out.u8(0x07);
                encode_tip(out, *tip);
            }
            Self::ActiveAlert(alert) => {
                out.version();
                out.u8(0x08);
                out.u64(alert.finding.zone_height());
                out.hash(alert.finding.zone_hash());
                out.u32(alert.finding.ordinal());
                encode_tip(out, alert.last_verified_parent);
            }
        }
    }

    fn decode_checked(input: &mut Decoder<'_>) -> Result<Self, CodecError> {
        let envelope = input.u8("metadata value envelope")?;
        if envelope == VERSION_RECORD_PREFIX {
            return Ok(Self::Version(input.u32("model version")?));
        }
        if envelope != SCHEMA_VERSION {
            return Err(CodecError::UnknownVersion {
                actual: envelope,
                expected: SCHEMA_VERSION,
            });
        }
        match input.u8("metadata value tag")? {
            0x01 => Ok(Self::ZoneIdentity {
                chain_id: input.u64("Zone chain ID")?,
                genesis_hash: input.hash("Zone genesis hash")?,
                zone_id: input.u32("Zone ID")?,
                initial_token: input.address("initial token")?,
            }),
            0x02 => Ok(Self::L1ChainId(input.u64("L1 chain ID")?)),
            0x03 => Ok(Self::Contracts {
                zone_factory: input.address("ZoneFactory address")?,
                portal: input.address("Portal address")?,
            }),
            0x04 => Ok(Self::PortalCreationBlockHash(
                input.hash("Portal creation block hash")?,
            )),
            0x05 => Ok(Self::Bootstrap(decode_bootstrap(input)?)),
            0x06 => Ok(Self::VerifiedZoneTip(decode_tip(
                input,
                "verified Zone tip",
            )?)),
            0x07 => Ok(Self::ImportedTempoTip(decode_tip(
                input,
                "imported Tempo tip",
            )?)),
            0x08 => Ok(Self::ActiveAlert(ActiveAlert {
                finding: FindingKey::new(
                    input.u64("finding Zone height")?,
                    input.hash("finding Zone hash")?,
                    input.u32("finding ordinal")?,
                ),
                last_verified_parent: decode_tip(input, "alert parent")?,
            })),
            tag => Err(CodecError::UnknownTag {
                kind: "metadata value",
                tag,
            }),
        }
    }
}

impl_value_codec!(MetaValue);

fn encode_tip(out: &mut Encoder, tip: BlockNumHash) {
    out.u64(tip.number);
    out.hash(tip.hash);
}

fn decode_tip(input: &mut Decoder<'_>, field: &'static str) -> Result<BlockNumHash, CodecError> {
    Ok(BlockNumHash {
        number: input.u64(field)?,
        hash: input.hash(field)?,
    })
}

fn encode_bootstrap(out: &mut Encoder, state: BootstrapState) {
    match state {
        BootstrapState::L1Replay { cursor: None } => {
            out.u8(0x00);
            out.u8(0x00);
        }
        BootstrapState::L1Replay {
            cursor: Some(cursor),
        } => {
            out.u8(0x00);
            out.u8(0x01);
            encode_tip(out, cursor);
        }
        BootstrapState::ZoneReplay { cursor } => {
            out.u8(0x01);
            out.u8(0x01);
            encode_tip(out, cursor);
        }
        BootstrapState::Live => {
            out.u8(0x02);
            out.u8(0x00);
        }
    }
}

fn decode_bootstrap(input: &mut Decoder<'_>) -> Result<BootstrapState, CodecError> {
    let phase = input.u8("bootstrap phase")?;
    let cursor = match input.u8("bootstrap cursor presence")? {
        0x00 => None,
        0x01 => Some(decode_tip(input, "bootstrap L1 cursor")?),
        tag => {
            return Err(CodecError::UnknownTag {
                kind: "bootstrap cursor presence",
                tag,
            });
        }
    };
    match (phase, cursor) {
        (0x00, cursor) => Ok(BootstrapState::L1Replay { cursor }),
        (0x01, Some(cursor)) => Ok(BootstrapState::ZoneReplay { cursor }),
        (0x02, None) => Ok(BootstrapState::Live),
        (0x01, None) => Err(CodecError::Invalid {
            field: "bootstrap state",
            reason: "Zone replay requires an L1 cursor",
        }),
        (0x02, Some(_)) => Err(CodecError::Invalid {
            field: "bootstrap state",
            reason: "live phase cannot retain an L1 replay cursor",
        }),
        (tag, _) => Err(CodecError::UnknownTag {
            kind: "bootstrap phase",
            tag,
        }),
    }
}

#[cfg(test)]
mod tests {
    use reth_codecs::{Compress, Decompress};

    use super::*;

    fn hash(byte: u8) -> B256 {
        B256::repeat_byte(byte)
    }

    #[test]
    fn every_metadata_family_has_stable_round_trip() {
        let finding = FindingKey::new(9, hash(0x44), 3);
        let values = [
            MetaValue::Version(u32::from(SCHEMA_VERSION)),
            MetaValue::ZoneIdentity {
                chain_id: 4242,
                genesis_hash: hash(0x11),
                zone_id: 7,
                initial_token: Address::repeat_byte(0x22),
            },
            MetaValue::L1ChainId(31337),
            MetaValue::Contracts {
                zone_factory: Address::repeat_byte(0x33),
                portal: Address::repeat_byte(0x44),
            },
            MetaValue::PortalCreationBlockHash(hash(0x55)),
            MetaValue::Bootstrap(BootstrapState::l1_replay(Some(BlockNumHash::new(
                4,
                hash(0x66),
            )))),
            MetaValue::VerifiedZoneTip(BlockNumHash::new(5, hash(0x77))),
            MetaValue::ImportedTempoTip(BlockNumHash::new(6, hash(0x88))),
            MetaValue::ActiveAlert(ActiveAlert {
                finding,
                last_verified_parent: BlockNumHash::new(8, hash(0x99)),
            }),
        ];

        for value in values {
            let bytes = value.clone().compress();
            assert_eq!(bytes, golden(&value));
            assert_eq!(MetaValue::decompress(&bytes).unwrap(), value);
            for cut in 0..bytes.len() {
                assert!(MetaValue::decompress(&bytes[..cut]).is_err());
            }
        }
    }

    #[test]
    fn version_bytes_are_golden_and_strict() {
        let bytes = MetaValue::Version(u32::from(SCHEMA_VERSION)).compress();
        assert_eq!(bytes, vec![0, 0, 0, 0, 1]);
        assert_eq!(
            MetaValue::decompress(&[0, 0, 0, 0, 2]).unwrap(),
            MetaValue::Version(2)
        );

        let mut trailing = bytes;
        trailing.push(0);
        assert!(MetaValue::decompress(&trailing).is_err());
    }

    fn golden(value: &MetaValue) -> Vec<u8> {
        let mut bytes = Vec::new();
        match value {
            MetaValue::Version(version) => {
                bytes.push(VERSION_RECORD_PREFIX);
                bytes.extend_from_slice(&version.to_be_bytes());
            }
            MetaValue::ZoneIdentity {
                chain_id,
                genesis_hash,
                zone_id,
                initial_token,
            } => {
                bytes.extend_from_slice(&[SCHEMA_VERSION, 0x01]);
                bytes.extend_from_slice(&chain_id.to_be_bytes());
                bytes.extend_from_slice(genesis_hash.as_slice());
                bytes.extend_from_slice(&zone_id.to_be_bytes());
                bytes.extend_from_slice(initial_token.as_slice());
            }
            MetaValue::L1ChainId(chain_id) => {
                bytes.extend_from_slice(&[SCHEMA_VERSION, 0x02]);
                bytes.extend_from_slice(&chain_id.to_be_bytes());
            }
            MetaValue::Contracts {
                zone_factory,
                portal,
            } => {
                bytes.extend_from_slice(&[SCHEMA_VERSION, 0x03]);
                bytes.extend_from_slice(zone_factory.as_slice());
                bytes.extend_from_slice(portal.as_slice());
            }
            MetaValue::PortalCreationBlockHash(hash) => {
                bytes.extend_from_slice(&[SCHEMA_VERSION, 0x04]);
                bytes.extend_from_slice(hash.as_slice());
            }
            MetaValue::Bootstrap(state) => {
                bytes.extend_from_slice(&[SCHEMA_VERSION, 0x05, 0x00, 0x01]);
                let BootstrapState::L1Replay {
                    cursor: Some(cursor),
                } = state
                else {
                    panic!("golden bootstrap state must carry an L1 cursor")
                };
                bytes.extend_from_slice(&cursor.number.to_be_bytes());
                bytes.extend_from_slice(cursor.hash.as_slice());
            }
            MetaValue::VerifiedZoneTip(tip) => {
                bytes.extend_from_slice(&[SCHEMA_VERSION, 0x06]);
                bytes.extend_from_slice(&tip.number.to_be_bytes());
                bytes.extend_from_slice(tip.hash.as_slice());
            }
            MetaValue::ImportedTempoTip(tip) => {
                bytes.extend_from_slice(&[SCHEMA_VERSION, 0x07]);
                bytes.extend_from_slice(&tip.number.to_be_bytes());
                bytes.extend_from_slice(tip.hash.as_slice());
            }
            MetaValue::ActiveAlert(alert) => {
                bytes.extend_from_slice(&[SCHEMA_VERSION, 0x08]);
                bytes.extend_from_slice(&alert.finding.zone_height().to_be_bytes());
                bytes.extend_from_slice(alert.finding.zone_hash().as_slice());
                bytes.extend_from_slice(&alert.finding.ordinal().to_be_bytes());
                bytes.extend_from_slice(&alert.last_verified_parent.number.to_be_bytes());
                bytes.extend_from_slice(alert.last_verified_parent.hash.as_slice());
            }
        }
        bytes
    }
}
