#![cfg(feature = "sync")]

use fatsd::{BlockAccess, MbrError, MbrFormatError, read_mbr};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReadError;

struct MemoryDevice {
    bytes: [u8; 512],
    block_size: usize,
}

impl BlockAccess for MemoryDevice {
    type Error = ReadError;

    fn block_size(&self) -> usize {
        self.block_size
    }

    fn read_block_at(
        &mut self,
        block: u64,
        offset: usize,
        out: &mut [u8],
    ) -> Result<(), Self::Error> {
        let start = block as usize * self.block_size + offset;
        out.copy_from_slice(self.bytes.get(start..start + out.len()).ok_or(ReadError)?);
        Ok(())
    }
}

#[test]
fn reads_primary_partition_table_across_device_blocks() {
    let mut bytes = [0; 512];
    let entry = &mut bytes[446..462];
    entry[0] = 0x80;
    entry[1..4].copy_from_slice(&[1, 2, 3]);
    entry[4] = 0x0c;
    entry[5..8].copy_from_slice(&[4, 5, 6]);
    entry[8..12].copy_from_slice(&2048_u32.to_le_bytes());
    entry[12..16].copy_from_slice(&4096_u32.to_le_bytes());
    bytes[510..512].copy_from_slice(&[0x55, 0xaa]);
    let mut device = MemoryDevice {
        bytes,
        block_size: 128,
    };

    let table = read_mbr(&mut device).unwrap();

    assert_eq!(table.entries[0].boot_indicator, 0x80);
    assert_eq!(table.entries[0].first_chs, [1, 2, 3]);
    assert_eq!(table.entries[0].partition_type, 0x0c);
    assert_eq!(table.entries[0].last_chs, [4, 5, 6]);
    assert_eq!(table.entries[0].first_lba, 2048);
    assert_eq!(table.entries[0].sector_count, 4096);
    assert!(table.entries[0].is_bootable());
    assert!(table.entries[1].is_unused());
}

#[test]
fn rejects_missing_signature() {
    let mut device = MemoryDevice {
        bytes: [0; 512],
        block_size: 512,
    };

    assert_eq!(
        read_mbr(&mut device),
        Err(MbrError::InvalidFormat(MbrFormatError::InvalidSignature))
    );
}

#[test]
fn rejects_zero_block_size() {
    let mut device = MemoryDevice {
        bytes: [0; 512],
        block_size: 0,
    };

    assert_eq!(read_mbr(&mut device), Err(MbrError::InvalidBlockSize));
}
