//! Versioned Quake SPU-bank headers shared by the host cooker and PS1 guest.
//!
//! QSB1 v2 lays out `record_count` SDK `SoundEffect` records, then a table of
//! `record_count` little-endian u16 playback rates in Hz (one per record,
//! same order), then the ADPCM payload. Both tables are covered by the
//! content hash.

use crate::{CookedRecord, RecordSlice, SoundEffect};

/// Little-endian `QSB1`.
pub const SOUND_BANK_MAGIC: u32 = 0x3142_5351;
pub const SOUND_BANK_VERSION: u16 = 2;
pub const SOUND_BANK_HEADER_BYTES: usize = 40;
pub const SOUND_BANK_RECORD_BYTES: usize = SoundEffect::SIZE;
pub const SOUND_BANK_RATE_BYTES: usize = 2;
pub const SOUND_GLOBAL_EFFECTS: usize = 37;
pub const SOUND_MAX_EFFECTS: usize = 255;
pub const SOUND_SPU_BASE: u32 = 0x1100;
pub const SOUND_SPU_END: u32 = 0x80000;
pub const SOUND_HASH_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SoundBankKind {
    Global = 1,
    Local = 2,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SoundBankHeader {
    pub kind: SoundBankKind,
    pub record_count: u16,
    pub payload_base: u32,
    pub payload_bytes: u32,
    pub spu_high_water: u32,
    pub dependency_hash: u64,
    pub content_hash: u64,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SoundBankError {
    Truncated,
    BadMagic,
    BadVersion,
    BadKind,
    BadHeaderSize,
    Reserved,
    BadCount,
    BadRange,
    BadLength,
    BadHash,
    BadRecord,
    DuplicateId,
    BadRate,
}

/// The per-record playback rate table that follows the record table.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SoundRates<'a> {
    bytes: &'a [u8],
}

impl<'a> SoundRates<'a> {
    #[optimize(size)]
    pub fn new(bytes: &'a [u8]) -> Option<Self> {
        (bytes.len() % SOUND_BANK_RATE_BYTES == 0).then_some(Self { bytes })
    }

    #[optimize(size)]
    pub const fn len(self) -> usize {
        self.bytes.len() / SOUND_BANK_RATE_BYTES
    }

    #[optimize(size)]
    pub const fn is_empty(self) -> bool {
        self.bytes.is_empty()
    }

    #[optimize(size)]
    pub fn get(self, index: usize) -> Option<u32> {
        let start = index.checked_mul(SOUND_BANK_RATE_BYTES)?;
        let bytes = self.bytes.get(start..start + SOUND_BANK_RATE_BYTES)?;
        Some(u32::from(u16::from_le_bytes([bytes[0], bytes[1]])))
    }
}

impl SoundBankHeader {
    #[optimize(size)]
    pub fn encode(self) -> [u8; SOUND_BANK_HEADER_BYTES] {
        let mut bytes = [0; SOUND_BANK_HEADER_BYTES];
        bytes[0..4].copy_from_slice(&SOUND_BANK_MAGIC.to_le_bytes());
        bytes[4..6].copy_from_slice(&SOUND_BANK_VERSION.to_le_bytes());
        bytes[6] = self.kind as u8;
        bytes[7] = SOUND_BANK_HEADER_BYTES as u8;
        bytes[8..10].copy_from_slice(&self.record_count.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.payload_base.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.payload_bytes.to_le_bytes());
        bytes[20..24].copy_from_slice(&self.spu_high_water.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.dependency_hash.to_le_bytes());
        bytes[32..40].copy_from_slice(&self.content_hash.to_le_bytes());
        bytes
    }

    #[optimize(size)]
    pub fn decode(bytes: &[u8]) -> Result<Self, SoundBankError> {
        let bytes = bytes
            .get(..SOUND_BANK_HEADER_BYTES)
            .ok_or(SoundBankError::Truncated)?;
        if u32::from_le_bytes(bytes[0..4].try_into().unwrap()) != SOUND_BANK_MAGIC {
            return Err(SoundBankError::BadMagic);
        }
        if u16::from_le_bytes(bytes[4..6].try_into().unwrap()) != SOUND_BANK_VERSION {
            return Err(SoundBankError::BadVersion);
        }
        let kind = match bytes[6] {
            1 => SoundBankKind::Global,
            2 => SoundBankKind::Local,
            _ => return Err(SoundBankError::BadKind),
        };
        if bytes[7] as usize != SOUND_BANK_HEADER_BYTES {
            return Err(SoundBankError::BadHeaderSize);
        }
        if bytes[10] != 0 || bytes[11] != 0 {
            return Err(SoundBankError::Reserved);
        }
        let header = Self {
            kind,
            record_count: u16::from_le_bytes(bytes[8..10].try_into().unwrap()),
            payload_base: u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
            payload_bytes: u32::from_le_bytes(bytes[16..20].try_into().unwrap()),
            spu_high_water: u32::from_le_bytes(bytes[20..24].try_into().unwrap()),
            dependency_hash: u64::from_le_bytes(bytes[24..32].try_into().unwrap()),
            content_hash: u64::from_le_bytes(bytes[32..40].try_into().unwrap()),
        };
        if header.record_count as usize > SOUND_MAX_EFFECTS {
            return Err(SoundBankError::BadCount);
        }
        if header.payload_bytes & 15 != 0
            || header.payload_base < SOUND_SPU_BASE
            || header.payload_base & 15 != 0
            || header.spu_high_water
                != header
                    .payload_base
                    .checked_add(header.payload_bytes)
                    .ok_or(SoundBankError::BadRange)?
            || header.spu_high_water > SOUND_SPU_END
        {
            return Err(SoundBankError::BadRange);
        }
        match header.kind {
            SoundBankKind::Global
                if header.record_count as usize != SOUND_GLOBAL_EFFECTS
                    || header.payload_base != SOUND_SPU_BASE
                    || header.payload_bytes == 0
                    || header.dependency_hash != 0 =>
            {
                return Err(SoundBankError::BadRange)
            }
            SoundBankKind::Local if header.dependency_hash == 0 => {
                return Err(SoundBankError::BadHash)
            }
            SoundBankKind::Local if (header.record_count == 0) != (header.payload_bytes == 0) => {
                return Err(SoundBankError::BadRange)
            }
            _ => {}
        }
        Ok(header)
    }

    /// Bytes of the record table plus the rate table (the hashed table region).
    #[optimize(size)]
    pub fn table_bytes(self) -> Result<usize, SoundBankError> {
        usize::from(self.record_count)
            .checked_mul(SOUND_BANK_RECORD_BYTES + SOUND_BANK_RATE_BYTES)
            .ok_or(SoundBankError::BadLength)
    }

    /// Split the hashed table region into its record and rate views.
    #[optimize(size)]
    pub fn split_table<'a>(
        self,
        table: &'a [u8],
    ) -> Result<(RecordSlice<'a, SoundEffect>, SoundRates<'a>), SoundBankError> {
        let record_bytes = usize::from(self.record_count) * SOUND_BANK_RECORD_BYTES;
        if table.len() != self.table_bytes()? {
            return Err(SoundBankError::BadLength);
        }
        let records = RecordSlice::<SoundEffect>::new(&table[..record_bytes])
            .ok_or(SoundBankError::BadLength)?;
        let rates = SoundRates::new(&table[record_bytes..]).ok_or(SoundBankError::BadLength)?;
        Ok((records, rates))
    }

    #[optimize(size)]
    pub fn payload_offset(self) -> Result<usize, SoundBankError> {
        SOUND_BANK_HEADER_BYTES
            .checked_add(self.table_bytes()?)
            .ok_or(SoundBankError::BadLength)
    }

    #[optimize(size)]
    pub fn file_bytes(self) -> Result<usize, SoundBankError> {
        self.payload_offset()?
            .checked_add(self.payload_bytes as usize)
            .ok_or(SoundBankError::BadLength)
    }
}

#[optimize(size)]
pub fn sound_hash_extend(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[optimize(size)]
pub fn sound_content_hash(table: &[u8], payload: &[u8]) -> u64 {
    sound_hash_extend(sound_hash_extend(SOUND_HASH_OFFSET, table), payload)
}

/// Validate a complete bank image and return its header, records, rates, and payload.
#[optimize(size)]
#[allow(clippy::type_complexity)]
pub fn decode_sound_bank(
    bytes: &[u8],
) -> Result<
    (
        SoundBankHeader,
        RecordSlice<'_, SoundEffect>,
        SoundRates<'_>,
        &[u8],
    ),
    SoundBankError,
> {
    let header = SoundBankHeader::decode(bytes)?;
    if bytes.len() != header.file_bytes()? {
        return Err(SoundBankError::BadLength);
    }
    let payload_offset = header.payload_offset()?;
    let table = &bytes[SOUND_BANK_HEADER_BYTES..payload_offset];
    let payload = &bytes[payload_offset..];
    if sound_content_hash(table, payload) != header.content_hash {
        return Err(SoundBankError::BadHash);
    }
    let (records, rates) = header.split_table(table)?;
    validate_sound_records(header, records, rates)?;
    Ok((header, records, rates, payload))
}

#[optimize(size)]
pub fn validate_sound_records(
    header: SoundBankHeader,
    records: RecordSlice<'_, SoundEffect>,
    rates: SoundRates<'_>,
) -> Result<(), SoundBankError> {
    if records.len() != header.record_count as usize || rates.len() != records.len() {
        return Err(SoundBankError::BadCount);
    }
    if (0..rates.len()).any(|index| rates.get(index).unwrap_or(0) == 0) {
        return Err(SoundBankError::BadRate);
    }
    let mut previous_address = None;
    for (index, effect) in records.iter().enumerate() {
        if effect.id <= 0
            || effect.frames == 0
            || effect.spu_address < header.payload_base
            || effect.spu_address >= header.spu_high_water
            || effect.spu_address & 15 != 0
            || (index == 0 && effect.spu_address != header.payload_base)
            || previous_address.is_some_and(|address| effect.spu_address <= address)
        {
            return Err(SoundBankError::BadRecord);
        }
        for prior in records.iter().take(index) {
            if prior.id == effect.id {
                return Err(SoundBankError::DuplicateId);
            }
        }
        previous_address = Some(effect.spu_address);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    #[optimize(size)]
    fn bank(kind: SoundBankKind, dependency_hash: u64) -> Vec<u8> {
        let mut table = Vec::new();
        for (id, address) in [(1i16, SOUND_SPU_BASE), (2, SOUND_SPU_BASE + 16)] {
            table.extend_from_slice(&id.to_le_bytes());
            table.extend_from_slice(&1u16.to_le_bytes());
            table.extend_from_slice(&address.to_le_bytes());
        }
        let payload = [0x5a; 32];
        let count = if kind == SoundBankKind::Global {
            SOUND_GLOBAL_EFFECTS as u16
        } else {
            2
        };
        if kind == SoundBankKind::Global {
            while table.len() < SOUND_GLOBAL_EFFECTS * SOUND_BANK_RECORD_BYTES {
                let id = table.len() as i16 / SOUND_BANK_RECORD_BYTES as i16 + 1;
                table.extend_from_slice(&id.to_le_bytes());
                table.extend_from_slice(&1u16.to_le_bytes());
                table.extend_from_slice(&SOUND_SPU_BASE.to_le_bytes());
            }
        }
        for _ in 0..count {
            table.extend_from_slice(&11_025u16.to_le_bytes());
        }
        let header = SoundBankHeader {
            kind,
            record_count: count,
            payload_base: SOUND_SPU_BASE,
            payload_bytes: payload.len() as u32,
            spu_high_water: SOUND_SPU_BASE + payload.len() as u32,
            dependency_hash,
            content_hash: sound_content_hash(&table, &payload),
        };
        let mut bytes = header.encode().to_vec();
        bytes.extend_from_slice(&table);
        bytes.extend_from_slice(&payload);
        bytes
    }

    #[optimize(size)]
    #[test]
    fn headers_reject_legacy_unknown_and_truncated_banks() {
        assert_eq!(
            SoundBankHeader::decode(&[2, 0, 0, 0]),
            Err(SoundBankError::Truncated)
        );
        let mut bytes = bank(SoundBankKind::Local, 7);
        bytes[0] = 0;
        assert!(matches!(
            decode_sound_bank(&bytes),
            Err(SoundBankError::BadMagic)
        ));
        let mut bytes = bank(SoundBankKind::Local, 7);
        bytes[4] = 1;
        assert!(matches!(
            decode_sound_bank(&bytes),
            Err(SoundBankError::BadVersion)
        ));
    }

    #[optimize(size)]
    #[test]
    fn rates_follow_the_records_and_must_be_nonzero() {
        let bytes = bank(SoundBankKind::Local, 7);
        let (_, records, rates, _) = decode_sound_bank(&bytes).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(rates.len(), 2);
        assert_eq!(rates.get(1), Some(11_025));
        let mut bytes = bank(SoundBankKind::Local, 7);
        let rate = SOUND_BANK_HEADER_BYTES + 2 * SOUND_BANK_RECORD_BYTES;
        bytes[rate..rate + 2].fill(0);
        let header = SoundBankHeader::decode(&bytes).unwrap();
        let payload_offset = header.payload_offset().unwrap();
        let hash = sound_content_hash(
            &bytes[SOUND_BANK_HEADER_BYTES..payload_offset],
            &bytes[payload_offset..],
        );
        bytes[32..40].copy_from_slice(&hash.to_le_bytes());
        assert!(matches!(
            decode_sound_bank(&bytes),
            Err(SoundBankError::BadRate)
        ));
    }

    #[optimize(size)]
    #[test]
    fn content_hash_and_dependency_are_fail_closed() {
        let mut bytes = bank(SoundBankKind::Local, 7);
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        assert!(matches!(
            decode_sound_bank(&bytes),
            Err(SoundBankError::BadHash)
        ));
        let mut bytes = bank(SoundBankKind::Local, 7);
        bytes[24..32].fill(0);
        assert!(matches!(
            decode_sound_bank(&bytes),
            Err(SoundBankError::BadHash)
        ));
    }

    #[optimize(size)]
    #[test]
    fn global_only_maps_use_a_valid_empty_local_suffix() {
        let header = SoundBankHeader {
            kind: SoundBankKind::Local,
            record_count: 0,
            payload_base: 0x2800,
            payload_bytes: 0,
            spu_high_water: 0x2800,
            dependency_hash: 7,
            content_hash: SOUND_HASH_OFFSET,
        };
        let bytes = header.encode();
        let (decoded, records, rates, payload) =
            decode_sound_bank(&bytes).expect("empty local suffix is self-contained");
        assert_eq!(decoded, header);
        assert_eq!(records.len(), 0);
        assert!(rates.is_empty());
        assert!(payload.is_empty());
    }

    #[optimize(size)]
    #[test]
    fn duplicate_ids_fail_even_when_the_content_hash_is_self_consistent() {
        let mut bytes = bank(SoundBankKind::Local, 7);
        let second_id = SOUND_BANK_HEADER_BYTES + SOUND_BANK_RECORD_BYTES;
        bytes[second_id..second_id + 2].copy_from_slice(&1i16.to_le_bytes());
        let header = SoundBankHeader::decode(&bytes).unwrap();
        let payload_offset = header.payload_offset().unwrap();
        let hash = sound_content_hash(
            &bytes[SOUND_BANK_HEADER_BYTES..payload_offset],
            &bytes[payload_offset..],
        );
        bytes[32..40].copy_from_slice(&hash.to_le_bytes());
        assert!(matches!(
            decode_sound_bank(&bytes),
            Err(SoundBankError::DuplicateId)
        ));
    }
}
