mod kind;
mod primitives;

use crate::store::codec::{CheckedCompact, CodecError, Decoder, Encoder, impl_value_codec};

use super::types::{FindingRecord, MAX_RECORD_SIZE};
use kind::{decode_kind, encode_kind};
use primitives::{decode_optional_tip, decode_status, encode_optional_tip, encode_status};

impl CheckedCompact for FindingRecord {
    fn encode_checked(&self, out: &mut Encoder) {
        out.version();
        out.hash(self.zone_parent_hash);
        encode_optional_tip(out, self.imported_tempo);
        encode_status(out, self.status);
        encode_kind(out, &self.kind);
    }

    fn decode_checked(input: &mut Decoder<'_>) -> Result<Self, CodecError> {
        if input.remaining_len() > MAX_RECORD_SIZE {
            return Err(invalid(
                "finding record",
                "encoded value exceeds 2048 bytes",
            ));
        }
        input.version()?;
        let parent = input.hash("finding Zone parent hash")?;
        let imported = decode_optional_tip(input)?;
        let status = decode_status(input)?;
        let kind = decode_kind(input)?;
        Self::new(parent, imported, status, kind)
            .ok_or_else(|| invalid("finding kind", "invalid leaf code or protocol chain"))
    }
}

impl_value_codec!(FindingRecord);

pub(super) fn invalid(field: &'static str, reason: &'static str) -> CodecError {
    CodecError::Invalid { field, reason }
}
