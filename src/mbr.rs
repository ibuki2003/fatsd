//! Master Boot Record partition-table parsing.

#[cfg(feature = "async")]
use crate::access::AsyncBlockAccess;
#[cfg(feature = "sync")]
use crate::access::BlockAccess;

/// Number of primary partition entries in an MBR.
pub const MBR_PARTITION_COUNT: usize = 4;

const MBR_SIZE: usize = 512;
const PARTITION_TABLE_OFFSET: usize = 446;
const PARTITION_ENTRY_SIZE: usize = 16;

/// An error produced while reading an MBR partition table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MbrError<E> {
    /// The underlying block device returned an error.
    Io(E),
    /// The block device reports a block size of zero.
    InvalidBlockSize,
    /// The first sector is not a valid MBR.
    InvalidFormat(MbrFormatError),
}

/// An error produced while parsing an MBR partition table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MbrFormatError {
    /// The supplied byte buffer is smaller than one MBR sector.
    BufferTooSmall,
    /// The MBR signature is missing.
    InvalidSignature,
}

/// One primary partition entry from an MBR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MbrPartitionEntry {
    /// Boot indicator byte.
    pub boot_indicator: u8,
    /// Raw three-byte CHS address of the first sector.
    pub first_chs: [u8; 3],
    /// Partition type byte.
    pub partition_type: u8,
    /// Raw three-byte CHS address of the last sector.
    pub last_chs: [u8; 3],
    /// First sector as a 512-byte-sector LBA.
    pub first_lba: u32,
    /// Partition length in 512-byte sectors.
    pub sector_count: u32,
}

impl MbrPartitionEntry {
    /// Returns whether the boot indicator marks this partition active.
    pub const fn is_bootable(&self) -> bool {
        self.boot_indicator == 0x80
    }

    /// Returns whether this entry has an unused partition type.
    pub const fn is_unused(&self) -> bool {
        self.partition_type == 0
    }

    fn parse(bytes: &[u8; PARTITION_ENTRY_SIZE]) -> Self {
        Self {
            boot_indicator: bytes[0],
            first_chs: [bytes[1], bytes[2], bytes[3]],
            partition_type: bytes[4],
            last_chs: [bytes[5], bytes[6], bytes[7]],
            first_lba: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            sector_count: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
        }
    }
}

/// The four primary partition entries stored in an MBR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MbrPartitionTable {
    /// Primary partition entries in on-disk order.
    pub entries: [MbrPartitionEntry; MBR_PARTITION_COUNT],
}

impl MbrPartitionTable {
    /// Parses the primary partition table from an MBR sector image.
    pub fn parse(mbr: &[u8]) -> Result<Self, MbrFormatError> {
        if mbr.len() < MBR_SIZE {
            return Err(MbrFormatError::BufferTooSmall);
        }
        if mbr[510..512] != [0x55, 0xaa] {
            return Err(MbrFormatError::InvalidSignature);
        }

        let entries = core::array::from_fn(|index| {
            let start = PARTITION_TABLE_OFFSET + index * PARTITION_ENTRY_SIZE;
            let bytes = (&mbr[start..start + PARTITION_ENTRY_SIZE])
                .try_into()
                .expect("fixed MBR partition entry range");
            MbrPartitionEntry::parse(bytes)
        });
        Ok(Self { entries })
    }
}

/// Reads the primary partition table from a block device.
#[maybe_async_cfg::maybe(
    idents(
        BlockAccess(sync, async = "AsyncBlockAccess"),
        read_mbr(fn, sync, async = "read_mbr_async")
    ),
    sync(feature = "sync"),
    async(feature = "async")
)]
pub async fn read_mbr<T: BlockAccess + ?Sized>(
    device: &mut T,
) -> Result<MbrPartitionTable, MbrError<T::Error>> {
    let block_size = device.block_size();
    if block_size == 0 {
        return Err(MbrError::InvalidBlockSize);
    }

    let mut mbr = [0; MBR_SIZE];
    let mut read = 0;
    while read < mbr.len() {
        let block = read / block_size;
        let offset = read % block_size;
        let length = core::cmp::min(block_size - offset, mbr.len() - read);
        device
            .read_block_at(block as u64, offset, &mut mbr[read..read + length])
            .await
            .map_err(MbrError::Io)?;
        read += length;
    }

    MbrPartitionTable::parse(&mbr).map_err(MbrError::InvalidFormat)
}
