//! On-disk FAT values and their parsers and serializers.

use core::fmt;

pub const DIRECTORY_ENTRY_SIZE: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormatError {
    BufferTooSmall,
    InvalidBootSignature,
    InvalidBytesPerSector,
    InvalidSectorsPerCluster,
    InvalidBpb,
    InvalidDirectoryEntry,
    InvalidFatEntry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Bpb {
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub reserved_sector_count: u16,
    pub fat_count: u8,
    pub root_entry_count: u16,
    pub total_sectors: u32,
    pub media: u8,
    pub sectors_per_fat_16: u16,
    pub sectors_per_track: u16,
    pub head_count: u16,
    pub hidden_sectors: u32,
    pub sectors_per_fat_32: u32,
    pub extended_flags: u16,
    pub root_cluster: u32,
    pub fs_info_sector: u16,
    pub backup_boot_sector: u16,
}

impl Bpb {
    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        if bytes.len() < 512 {
            return Err(FormatError::BufferTooSmall);
        }
        if bytes[510..512] != [0x55, 0xaa] {
            return Err(FormatError::InvalidBootSignature);
        }

        let bytes_per_sector = le16(bytes, 11);
        if !matches!(bytes_per_sector, 512 | 1024 | 2048 | 4096) {
            return Err(FormatError::InvalidBytesPerSector);
        }
        let sectors_per_cluster = bytes[13];
        if sectors_per_cluster == 0 || !sectors_per_cluster.is_power_of_two() {
            return Err(FormatError::InvalidSectorsPerCluster);
        }

        let total_sectors_16 = le16(bytes, 19) as u32;
        let total_sectors = if total_sectors_16 == 0 {
            le32(bytes, 32)
        } else {
            total_sectors_16
        };
        let sectors_per_fat_16 = le16(bytes, 22);
        let is_fat32_bpb = sectors_per_fat_16 == 0;
        let bpb = Self {
            bytes_per_sector,
            sectors_per_cluster,
            reserved_sector_count: le16(bytes, 14),
            fat_count: bytes[16],
            root_entry_count: le16(bytes, 17),
            total_sectors,
            media: bytes[21],
            sectors_per_fat_16,
            sectors_per_track: le16(bytes, 24),
            head_count: le16(bytes, 26),
            hidden_sectors: le32(bytes, 28),
            sectors_per_fat_32: if is_fat32_bpb { le32(bytes, 36) } else { 0 },
            extended_flags: if is_fat32_bpb { le16(bytes, 40) } else { 0 },
            root_cluster: if is_fat32_bpb { le32(bytes, 44) } else { 0 },
            fs_info_sector: if is_fat32_bpb { le16(bytes, 48) } else { 0 },
            backup_boot_sector: if is_fat32_bpb { le16(bytes, 50) } else { 0 },
        };
        if bpb.reserved_sector_count == 0
            || bpb.fat_count == 0
            || bpb.total_sectors == 0
            || (bpb.sectors_per_fat_16 == 0 && bpb.sectors_per_fat_32 == 0)
        {
            return Err(FormatError::InvalidBpb);
        }
        Ok(bpb)
    }

    /// Updates the BPB fields in an existing boot-sector image.
    ///
    /// Fields outside the BPB, such as jump code and OEM name, are preserved.
    pub fn serialize_into(&self, bytes: &mut [u8]) -> Result<(), FormatError> {
        if bytes.len() < 512 {
            return Err(FormatError::BufferTooSmall);
        }
        put16(bytes, 11, self.bytes_per_sector);
        bytes[13] = self.sectors_per_cluster;
        put16(bytes, 14, self.reserved_sector_count);
        bytes[16] = self.fat_count;
        put16(bytes, 17, self.root_entry_count);
        let total16 = u16::try_from(self.total_sectors).unwrap_or(0);
        put16(bytes, 19, total16);
        bytes[21] = self.media;
        put16(bytes, 22, self.sectors_per_fat_16);
        put16(bytes, 24, self.sectors_per_track);
        put16(bytes, 26, self.head_count);
        put32(bytes, 28, self.hidden_sectors);
        put32(bytes, 32, if total16 == 0 { self.total_sectors } else { 0 });
        if self.sectors_per_fat_16 == 0 {
            put32(bytes, 36, self.sectors_per_fat_32);
            put16(bytes, 40, self.extended_flags);
            put32(bytes, 44, self.root_cluster);
            put16(bytes, 48, self.fs_info_sector);
            put16(bytes, 50, self.backup_boot_sector);
        }
        bytes[510..512].copy_from_slice(&[0x55, 0xaa]);
        Ok(())
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct RawDirEntry(pub [u8; DIRECTORY_ENTRY_SIZE]);

impl RawDirEntry {
    pub const ATTR_VOLUME_ID: u8 = 0x08;
    pub const ATTR_DIRECTORY: u8 = 0x10;
    pub const ATTR_LONG_NAME: u8 = 0x0f;

    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let data = bytes
            .get(..DIRECTORY_ENTRY_SIZE)
            .ok_or(FormatError::BufferTooSmall)?
            .try_into()
            .map_err(|_| FormatError::BufferTooSmall)?;
        Ok(Self(data))
    }

    pub const fn serialize(self) -> [u8; DIRECTORY_ENTRY_SIZE] {
        self.0
    }

    pub const fn is_end(&self) -> bool {
        self.0[0] == 0
    }

    pub const fn is_deleted(&self) -> bool {
        self.0[0] == 0xe5
    }

    pub const fn attributes(&self) -> u8 {
        self.0[11]
    }

    pub const fn is_lfn(&self) -> bool {
        self.attributes() == Self::ATTR_LONG_NAME
    }

    pub const fn is_directory(&self) -> bool {
        self.attributes() & Self::ATTR_DIRECTORY != 0
    }

    pub const fn is_volume_label(&self) -> bool {
        self.attributes() & Self::ATTR_VOLUME_ID != 0
    }

    pub fn short_name(&self) -> ShortName {
        ShortName(self.0[..11].try_into().expect("fixed slice length"))
    }

    pub fn first_cluster_high(&self) -> u16 {
        le16(&self.0, 20)
    }

    pub fn first_cluster_low(&self) -> u16 {
        le16(&self.0, 26)
    }

    pub fn first_cluster(&self) -> u32 {
        ((self.first_cluster_high() as u32) << 16) | self.first_cluster_low() as u32
    }

    pub fn file_size(&self) -> u32 {
        le32(&self.0, 28)
    }

    pub fn new(name: ShortName, attributes: u8) -> Self {
        let mut raw = [0; DIRECTORY_ENTRY_SIZE];
        raw[..11].copy_from_slice(&name.0);
        raw[11] = attributes;
        Self(raw)
    }

    pub fn set_first_cluster(&mut self, fat_type: FatType, cluster: Option<u32>) {
        let cluster = cluster.unwrap_or(0);
        let high = if fat_type == FatType::Fat32 {
            ((cluster & 0x0fff_ffff) >> 16) as u16
        } else {
            0
        };
        put16(&mut self.0, 20, high);
        put16(&mut self.0, 26, cluster as u16);
    }

    pub fn set_file_size(&mut self, size: u32) {
        put32(&mut self.0, 28, size);
    }
}

impl fmt::Debug for RawDirEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("RawDirEntry").field(&self.0).finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LfnEntry {
    pub ordinal: u8,
    pub is_last: bool,
    pub checksum: u8,
    pub characters: [u16; 13],
}

impl LfnEntry {
    pub fn parse(raw: RawDirEntry) -> Result<Self, FormatError> {
        if !raw.is_lfn()
            || raw.0[0] & 0x1f == 0
            || raw.0[0] & 0x1f > 20
            || raw.0[12] != 0
            || le16(&raw.0, 26) != 0
        {
            return Err(FormatError::InvalidDirectoryEntry);
        }
        let mut characters = [0; 13];
        for (out, offset) in characters.iter_mut().zip(LFN_CHARACTER_OFFSETS) {
            *out = le16(&raw.0, offset);
        }
        Ok(Self {
            ordinal: raw.0[0] & 0x1f,
            is_last: raw.0[0] & 0x40 != 0,
            checksum: raw.0[13],
            characters,
        })
    }

    pub fn serialize(self) -> Result<RawDirEntry, FormatError> {
        if self.ordinal == 0 || self.ordinal > 20 {
            return Err(FormatError::InvalidDirectoryEntry);
        }
        let mut raw = [0xff; DIRECTORY_ENTRY_SIZE];
        raw[0] = self.ordinal | if self.is_last { 0x40 } else { 0 };
        raw[11] = RawDirEntry::ATTR_LONG_NAME;
        raw[12] = 0;
        raw[13] = self.checksum;
        put16(&mut raw, 26, 0);
        for (character, offset) in self.characters.into_iter().zip(LFN_CHARACTER_OFFSETS) {
            put16(&mut raw, offset, character);
        }
        Ok(RawDirEntry(raw))
    }
}

const LFN_CHARACTER_OFFSETS: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShortName(pub [u8; 11]);

impl ShortName {
    pub fn checksum(self) -> u8 {
        self.0
            .into_iter()
            .fold(0u8, |sum, byte| sum.rotate_right(1).wrapping_add(byte))
    }

    pub fn matches(&self, name: &str) -> bool {
        if matches!(name, "." | "..") {
            return component_matches(&self.0[..8], name)
                && self.0[8..].iter().all(|byte| *byte == b' ');
        }
        let (base, extension) = name.rsplit_once('.').unwrap_or((name, ""));
        component_matches(&self.0[..8], base) && component_matches(&self.0[8..], extension)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FatType {
    Fat12,
    Fat16,
    Fat32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FatEntry {
    Free,
    Data(u32),
    Bad,
    EndOfChain,
    Reserved(u32),
}

impl FatEntry {
    pub fn parse(fat_type: FatType, cluster: u32, bytes: &[u8]) -> Result<Self, FormatError> {
        let value = match fat_type {
            FatType::Fat12 => {
                let pair = u16::from_le_bytes(
                    bytes
                        .get(..2)
                        .ok_or(FormatError::BufferTooSmall)?
                        .try_into()
                        .map_err(|_| FormatError::BufferTooSmall)?,
                );
                if cluster & 1 == 0 {
                    pair as u32 & 0x0fff
                } else {
                    pair as u32 >> 4
                }
            }
            FatType::Fat16 => u16::from_le_bytes(
                bytes
                    .get(..2)
                    .ok_or(FormatError::BufferTooSmall)?
                    .try_into()
                    .map_err(|_| FormatError::BufferTooSmall)?,
            ) as u32,
            FatType::Fat32 => {
                u32::from_le_bytes(
                    bytes
                        .get(..4)
                        .ok_or(FormatError::BufferTooSmall)?
                        .try_into()
                        .map_err(|_| FormatError::BufferTooSmall)?,
                ) & 0x0fff_ffff
            }
        };
        let (bad, eoc, reserved) = fat_markers(fat_type);
        Ok(match value {
            0 => Self::Free,
            value if (2..reserved).contains(&value) => Self::Data(value),
            value if value == bad => Self::Bad,
            value if value >= eoc => Self::EndOfChain,
            value => Self::Reserved(value),
        })
    }

    /// Serializes an entry into bytes read from the FAT.
    ///
    /// The adjacent nibble in FAT12 and the reserved high nibble in FAT32 are preserved.
    pub fn serialize_into(
        self,
        fat_type: FatType,
        cluster: u32,
        bytes: &mut [u8],
    ) -> Result<(), FormatError> {
        let (bad, _eoc, reserved) = fat_markers(fat_type);
        let mask = match fat_type {
            FatType::Fat12 => 0x0fff,
            FatType::Fat16 => 0xffff,
            FatType::Fat32 => 0x0fff_ffff,
        };
        let value = match self {
            Self::Free => 0,
            Self::Data(value) if (2..reserved).contains(&value) => value,
            Self::Bad => bad,
            Self::EndOfChain => mask,
            Self::Reserved(value) if value <= mask => value,
            Self::Data(_) | Self::Reserved(_) => return Err(FormatError::InvalidFatEntry),
        };
        match fat_type {
            FatType::Fat12 => {
                let target = bytes.get_mut(..2).ok_or(FormatError::BufferTooSmall)?;
                let old = u16::from_le_bytes(
                    target
                        .as_ref()
                        .try_into()
                        .map_err(|_| FormatError::BufferTooSmall)?,
                );
                let merged = if cluster & 1 == 0 {
                    (old & 0xf000) | value as u16
                } else {
                    (old & 0x000f) | (value as u16) << 4
                };
                target.copy_from_slice(&merged.to_le_bytes());
            }
            FatType::Fat16 => bytes
                .get_mut(..2)
                .ok_or(FormatError::BufferTooSmall)?
                .copy_from_slice(&(value as u16).to_le_bytes()),
            FatType::Fat32 => {
                let target = bytes.get_mut(..4).ok_or(FormatError::BufferTooSmall)?;
                let old = u32::from_le_bytes(
                    target
                        .as_ref()
                        .try_into()
                        .map_err(|_| FormatError::BufferTooSmall)?,
                );
                target.copy_from_slice(&((old & 0xf000_0000) | value).to_le_bytes());
            }
        }
        Ok(())
    }
}

const fn fat_markers(fat_type: FatType) -> (u32, u32, u32) {
    match fat_type {
        FatType::Fat12 => (0x0ff7, 0x0ff8, 0x0ff0),
        FatType::Fat16 => (0xfff7, 0xfff8, 0xfff0),
        FatType::Fat32 => (0x0fff_fff7, 0x0fff_fff8, 0x0fff_fff0),
    }
}

fn component_matches(raw: &[u8], value: &str) -> bool {
    let raw = &raw[..raw
        .iter()
        .position(|byte| *byte == b' ')
        .unwrap_or(raw.len())];
    raw.len() == value.len()
        && raw
            .iter()
            .copied()
            .zip(value.bytes())
            .all(|(left, right)| left.eq_ignore_ascii_case(&right))
}

pub(crate) fn le16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

pub(crate) fn le32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
}

pub(crate) fn put16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
