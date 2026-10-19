//! Quake's conventions inside a PXBSP v6 container.
//!
//! PXBSP is PSoXide's shared world container. It has no slot for Quake's
//! per-texture records or for Quake's fixed 50-byte map entities, so a Quake
//! map carries them in the places the container leaves to the game:
//!
//! * Each Quake map entity is one `PxbspEntity` of class [`ENTITY_CLASS`]
//!   whose payload is the unchanged 50-byte [`crate::MapEntity`] record.
//! * The per-texture table (the 14-byte [`crate::TextureInfo`] records, one
//!   per Materials-lump entry, in the same order) is a sidecar at the end of
//!   the Strings lump: records, then an eight-byte footer. Entity-string
//!   offsets address the unchanged prefix, as the leaf-bounds sidecar does for
//!   the Visibility lump.

use crate::TextureInfo;

/// `PxbspEntity::class_id` of a Quake map entity.
pub const ENTITY_CLASS: u16 = 0x0100;
/// Footer magic of the texture-table sidecar (`QTI1`).
pub const TEXTURE_TABLE_TRAILER_MAGIC: u32 = u32::from_le_bytes(*b"QTI1");
pub const TEXTURE_TABLE_RECORD_BYTES: usize = 14;
pub const TEXTURE_TABLE_FOOTER_BYTES: usize = 8;

/// The texture-table records at the end of a PXBSP Strings lump, or `None`
/// for a lump without the footer.
pub fn texture_table_records(strings: &[u8]) -> Option<&[u8]> {
    let footer = strings.get(strings.len().checked_sub(TEXTURE_TABLE_FOOTER_BYTES)?..)?;
    if u32::from_le_bytes(footer[0..4].try_into().ok()?) != TEXTURE_TABLE_TRAILER_MAGIC
        || u16::from_le_bytes(footer[6..8].try_into().ok()?) as usize != TEXTURE_TABLE_RECORD_BYTES
    {
        return None;
    }
    let count = u16::from_le_bytes(footer[4..6].try_into().ok()?) as usize;
    let table_bytes = count.checked_mul(TEXTURE_TABLE_RECORD_BYTES)?;
    let start = strings
        .len()
        .checked_sub(TEXTURE_TABLE_FOOTER_BYTES + table_bytes)?;
    strings.get(start..start + table_bytes)
}

/// Texture `index` of the sidecar table, decoded.
pub fn texture_table_entry(strings: &[u8], index: usize) -> Option<TextureInfo> {
    use crate::CookedRecord;
    let records = texture_table_records(strings)?;
    let start = index.checked_mul(TEXTURE_TABLE_RECORD_BYTES)?;
    let record = records.get(start..start + TEXTURE_TABLE_RECORD_BYTES)?;
    Some(TextureInfo::decode(record))
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn sidecar_round_trips_and_legacy_strings_have_none() {
        assert!(texture_table_records(b"abc\0").is_none());
        let mut table = std::vec![0u8; 2 * TEXTURE_TABLE_RECORD_BYTES];
        table[0] = 7;
        table[TEXTURE_TABLE_RECORD_BYTES] = 9;
        let mut output = std::vec![b'x', 0, b'y'];
        while output.len() % 4 != 0 {
            output.push(0);
        }
        output.extend_from_slice(&table);
        output.extend_from_slice(&TEXTURE_TABLE_TRAILER_MAGIC.to_le_bytes());
        output.extend_from_slice(&2u16.to_le_bytes());
        output.extend_from_slice(&(TEXTURE_TABLE_RECORD_BYTES as u16).to_le_bytes());
        assert_eq!(texture_table_records(&output), Some(&table[..]));
        assert_eq!(texture_table_entry(&output, 1).unwrap().atlas.x, 9);
        assert!(texture_table_entry(&output, 2).is_none());
    }
}
