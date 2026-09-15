use crate::{
    format::{Bpb, FormatError},
    handle::DirectoryLocation,
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Cluster(u32);

impl Cluster {
    pub const fn new(value: u32) -> Option<Self> {
        if value >= 2 { Some(Self(value)) } else { None }
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

pub use crate::format::FatType;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Volume {
    pub bpb: Bpb,
    pub fat_type: FatType,
    pub fat_start_sector: u32,
    pub root_directory_start_sector: u32,
    pub root_directory_sectors: u32,
    pub data_start_sector: u32,
    pub cluster_count: u32,
}

impl Volume {
    pub fn from_bpb(bpb: Bpb) -> Result<Self, FormatError> {
        let fat_sectors = if bpb.sectors_per_fat_16 != 0 {
            bpb.sectors_per_fat_16 as u32
        } else {
            bpb.sectors_per_fat_32
        };
        let root_bytes = bpb.root_entry_count as u32 * 32;
        let root_directory_sectors = root_bytes.div_ceil(bpb.bytes_per_sector as u32);
        let fat_start_sector = bpb.reserved_sector_count as u32;
        let root_directory_start_sector = fat_start_sector
            .checked_add(
                fat_sectors
                    .checked_mul(bpb.fat_count as u32)
                    .ok_or(FormatError::InvalidBpb)?,
            )
            .ok_or(FormatError::InvalidBpb)?;
        let data_start_sector = root_directory_start_sector
            .checked_add(root_directory_sectors)
            .ok_or(FormatError::InvalidBpb)?;
        let data_sectors = bpb
            .total_sectors
            .checked_sub(data_start_sector)
            .ok_or(FormatError::InvalidBpb)?;
        let cluster_count = data_sectors / bpb.sectors_per_cluster as u32;
        let fat_type = if cluster_count < 4_085 {
            FatType::Fat12
        } else if cluster_count < 65_525 {
            FatType::Fat16
        } else {
            FatType::Fat32
        };
        if fat_type == FatType::Fat32
            && (Cluster::new(bpb.root_cluster).is_none()
                || bpb.sectors_per_fat_16 != 0
                || bpb.sectors_per_fat_32 == 0
                || bpb.root_entry_count != 0)
        {
            return Err(FormatError::InvalidBpb);
        }
        if fat_type != FatType::Fat32 && (bpb.root_entry_count == 0 || bpb.sectors_per_fat_16 == 0)
        {
            return Err(FormatError::InvalidBpb);
        }
        if bpb.extended_flags & 0x80 != 0 && (bpb.extended_flags & 0x0f) as u8 >= bpb.fat_count {
            return Err(FormatError::InvalidBpb);
        }
        let fat_bytes = fat_sectors as u64 * bpb.bytes_per_sector as u64;
        let fat_entry_capacity = match fat_type {
            FatType::Fat12 => fat_bytes * 2 / 3,
            FatType::Fat16 => fat_bytes / 2,
            FatType::Fat32 => fat_bytes / 4,
        };
        if fat_entry_capacity < cluster_count as u64 + 2 {
            return Err(FormatError::InvalidBpb);
        }
        let first_reserved_cluster = match fat_type {
            FatType::Fat12 => 0x0ff0,
            FatType::Fat16 => 0xfff0,
            FatType::Fat32 => 0x0fff_fff0,
        };
        if cluster_count
            .checked_add(1)
            .is_none_or(|max_cluster| max_cluster >= first_reserved_cluster)
        {
            return Err(FormatError::InvalidBpb);
        }
        Ok(Self {
            bpb,
            fat_type,
            fat_start_sector,
            root_directory_start_sector,
            root_directory_sectors,
            data_start_sector,
            cluster_count,
        })
    }

    pub const fn sector_size(&self) -> usize {
        self.bpb.bytes_per_sector as usize
    }

    pub const fn cluster_size(&self) -> usize {
        self.sector_size() * self.bpb.sectors_per_cluster as usize
    }

    pub const fn fat_sectors(&self) -> u32 {
        if self.bpb.sectors_per_fat_16 != 0 {
            self.bpb.sectors_per_fat_16 as u32
        } else {
            self.bpb.sectors_per_fat_32
        }
    }

    pub const fn max_cluster(&self) -> u32 {
        self.cluster_count + 1
    }

    pub const fn sector_byte_offset(&self, sector: u32) -> u64 {
        sector as u64 * self.bpb.bytes_per_sector as u64
    }

    pub fn cluster_byte_offset(&self, cluster: Cluster) -> Option<u64> {
        let index = cluster.get().checked_sub(2)?;
        (index < self.cluster_count).then(|| {
            let sector =
                self.data_start_sector as u64 + index as u64 * self.bpb.sectors_per_cluster as u64;
            sector * self.bpb.bytes_per_sector as u64
        })
    }

    pub fn root_directory(&self) -> DirectoryLocation {
        match self.fat_type {
            FatType::Fat32 => DirectoryLocation::Cluster(
                Cluster::new(self.bpb.root_cluster).expect("validated FAT32 root cluster"),
            ),
            FatType::Fat12 | FatType::Fat16 => DirectoryLocation::FixedRoot,
        }
    }

    pub fn fat_mirroring_enabled(&self) -> bool {
        self.fat_type != FatType::Fat32 || self.bpb.extended_flags & 0x80 == 0
    }

    pub fn active_fat(&self) -> u8 {
        if self.fat_mirroring_enabled() {
            0
        } else {
            (self.bpb.extended_flags & 0x0f) as u8
        }
    }
}
