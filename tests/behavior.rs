#![cfg(feature = "sync")]

use fatsd::{
    AllocationAccess, BasicDirectoryHandle, BasicFileHandle, BlockAccess, BlockWrite, ChainAccess,
    DirectoryAccess, DirectoryHandle, DirectoryLocation, DirectoryWrite, FatFs, FileAccess,
    FileHandle, FileInfo, FileWrite,
    format::{Bpb, FatEntry, LfnEntry, RawDirEntry, ShortName},
    read_volume,
    volume::{Cluster, FatType, Volume},
};

#[derive(Debug, Eq, PartialEq)]
enum MemoryError {
    OutOfBounds,
}

struct MemoryDevice {
    bytes: Vec<u8>,
    block_size: usize,
}

impl BlockAccess for MemoryDevice {
    type Error = MemoryError;

    fn block_size(&self) -> usize {
        self.block_size
    }

    fn read_block_at(
        &mut self,
        block: u64,
        offset: usize,
        out: &mut [u8],
    ) -> Result<(), Self::Error> {
        if offset + out.len() > self.block_size {
            return Err(MemoryError::OutOfBounds);
        }
        let start = block as usize * self.block_size + offset;
        let source = self
            .bytes
            .get(start..start + out.len())
            .ok_or(MemoryError::OutOfBounds)?;
        out.copy_from_slice(source);
        Ok(())
    }
}

impl BlockWrite for MemoryDevice {
    fn write_block_at(
        &mut self,
        block: u64,
        offset: usize,
        data: &[u8],
    ) -> Result<(), Self::Error> {
        if offset + data.len() > self.block_size {
            return Err(MemoryError::OutOfBounds);
        }
        let start = block as usize * self.block_size + offset;
        let target = self
            .bytes
            .get_mut(start..start + data.len())
            .ok_or(MemoryError::OutOfBounds)?;
        target.copy_from_slice(data);
        Ok(())
    }
}

struct TestFs {
    device: MemoryDevice,
    volume: Volume,
}

impl BlockAccess for TestFs {
    type Error = MemoryError;

    fn block_size(&self) -> usize {
        self.device.block_size()
    }

    fn read_block_at(
        &mut self,
        block: u64,
        offset: usize,
        out: &mut [u8],
    ) -> Result<(), Self::Error> {
        self.device.read_block_at(block, offset, out)
    }
}

impl BlockWrite for TestFs {
    fn write_block_at(
        &mut self,
        block: u64,
        offset: usize,
        data: &[u8],
    ) -> Result<(), Self::Error> {
        self.device.write_block_at(block, offset, data)
    }
}

impl ChainAccess for TestFs {
    fn volume(&self) -> &Volume {
        &self.volume
    }
}

impl DirectoryAccess for TestFs {
    type DirectoryHandle = BasicDirectoryHandle;
}

impl FileAccess for TestFs {
    type FileHandle = BasicFileHandle;
}

impl AllocationAccess for TestFs {}
impl DirectoryWrite for TestFs {}
impl FileWrite for TestFs {}
impl FatFs for TestFs {}

struct ClmtHandle {
    info: FileInfo,
}

impl From<FileInfo> for ClmtHandle {
    fn from(info: FileInfo) -> Self {
        Self { info }
    }
}

impl FileHandle for ClmtHandle {
    fn file_info(&self) -> &FileInfo {
        &self.info
    }
}

struct ClmtFs {
    inner: TestFs,
    initial_lookups: usize,
    sequential_lookups: usize,
}

impl BlockAccess for ClmtFs {
    type Error = MemoryError;

    fn block_size(&self) -> usize {
        self.inner.block_size()
    }

    fn read_block_at(
        &mut self,
        block: u64,
        offset: usize,
        out: &mut [u8],
    ) -> Result<(), Self::Error> {
        self.inner.read_block_at(block, offset, out)
    }
}

impl ChainAccess for ClmtFs {
    fn volume(&self) -> &Volume {
        &self.inner.volume
    }
}

impl DirectoryAccess for ClmtFs {
    type DirectoryHandle = BasicDirectoryHandle;
}

impl FileAccess for ClmtFs {
    type FileHandle = ClmtHandle;

    fn resolve_file_cluster(
        &mut self,
        _file: &mut Self::FileHandle,
        index: u32,
    ) -> Result<Option<Cluster>, fatsd::Error<Self::Error>> {
        self.initial_lookups += 1;
        Ok(match index {
            0 => Cluster::new(2),
            1 => Cluster::new(3),
            _ => None,
        })
    }

    fn resolve_next_file_cluster(
        &mut self,
        _file: &mut Self::FileHandle,
        _current: Cluster,
        next_index: u32,
    ) -> Result<Option<Cluster>, fatsd::Error<Self::Error>> {
        self.sequential_lookups += 1;
        Ok((next_index == 1).then(|| Cluster::new(3).unwrap()))
    }
}

fn bpb() -> Bpb {
    Bpb {
        bytes_per_sector: 512,
        sectors_per_cluster: 1,
        reserved_sector_count: 1,
        fat_count: 2,
        root_entry_count: 16,
        total_sectors: 12,
        media: 0xf8,
        sectors_per_fat_16: 1,
        sectors_per_track: 32,
        head_count: 64,
        hidden_sectors: 0,
        sectors_per_fat_32: 0,
        extended_flags: 0,
        root_cluster: 0,
        fs_info_sector: 0,
        backup_boot_sector: 0,
    }
}

fn test_volume(fat_type: FatType) -> Volume {
    Volume {
        bpb: bpb(),
        fat_type,
        fat_start_sector: 1,
        root_directory_start_sector: 3,
        root_directory_sectors: 1,
        data_start_sector: 4,
        cluster_count: 8,
    }
}

fn test_fs(fat_type: FatType) -> TestFs {
    TestFs {
        device: MemoryDevice {
            bytes: vec![0; 12 * 512],
            block_size: 128,
        },
        volume: test_volume(fat_type),
    }
}

#[test]
fn bpb_round_trip_preserves_non_fat32_extension() {
    let original = bpb();
    let mut sector = [0xa5; 512];
    original.serialize_into(&mut sector).unwrap();

    assert_eq!(Bpb::parse(&sector).unwrap(), original);
    assert_eq!(&sector[36..64], &[0xa5; 28]);
}

#[test]
fn fat_entry_serialization_preserves_neighbor_bits() {
    let mut fat12 = [0xaa, 0xbb];
    FatEntry::Data(0x123)
        .serialize_into(FatType::Fat12, 2, &mut fat12)
        .unwrap();
    assert_eq!(fat12, [0x23, 0xb1]);
    assert_eq!(
        FatEntry::parse(FatType::Fat12, 2, &fat12).unwrap(),
        FatEntry::Data(0x123)
    );

    let mut fat32 = 0xa123_4567u32.to_le_bytes();
    FatEntry::EndOfChain
        .serialize_into(FatType::Fat32, 2, &mut fat32)
        .unwrap();
    assert_eq!(u32::from_le_bytes(fat32), 0xafff_ffff);
}

#[test]
fn filesystem_mounts_a_borrowed_partial_block_device() {
    let mut bytes = vec![0; 512];
    bpb().serialize_into(&mut bytes).unwrap();
    let mut device = MemoryDevice {
        bytes,
        block_size: 128,
    };

    let volume = read_volume(&mut device).unwrap();
    assert_eq!(volume.fat_type, FatType::Fat12);
}

#[test]
fn reads_fat12_fat16_and_fat32_entries() {
    let mut fat12 = test_fs(FatType::Fat12);
    fat12.device.bytes[512 + 3..512 + 6].copy_from_slice(&[0x23, 0xf1, 0xff]);
    assert_eq!(
        fat12.read_fat_entry(Cluster::new(2).unwrap()).unwrap(),
        FatEntry::Data(0x123)
    );
    assert_eq!(
        fat12.read_fat_entry(Cluster::new(3).unwrap()).unwrap(),
        FatEntry::EndOfChain
    );

    let mut fat16 = test_fs(FatType::Fat16);
    fat16.device.bytes[512 + 4..512 + 6].copy_from_slice(&0xfff7u16.to_le_bytes());
    assert_eq!(
        fat16.read_fat_entry(Cluster::new(2).unwrap()).unwrap(),
        FatEntry::Bad
    );

    let mut fat32 = test_fs(FatType::Fat32);
    fat32.device.bytes[512 + 8..512 + 12].copy_from_slice(&0xafff_ffffu32.to_le_bytes());
    assert_eq!(
        fat32.read_fat_entry(Cluster::new(2).unwrap()).unwrap(),
        FatEntry::EndOfChain
    );
}

#[test]
fn opens_lfn_and_short_name_and_reads_across_clusters() {
    let mut fs = test_fs(FatType::Fat16);
    set_fat16(&mut fs.device.bytes, 1, 2, 3);
    set_fat16(&mut fs.device.bytes, 1, 3, 0xffff);

    let short_name = ShortName(*b"LONGFI~1TXT");
    let mut characters = [0xffff; 13];
    for (target, source) in characters.iter_mut().zip("Long File.txt".encode_utf16()) {
        *target = source;
    }
    let lfn = LfnEntry {
        ordinal: 1,
        is_last: true,
        checksum: short_name.checksum(),
        characters,
    }
    .serialize()
    .unwrap();
    let root = 3 * 512;
    fs.device.bytes[root..root + 32].copy_from_slice(&lfn.serialize());
    let mut short = regular_entry(short_name, 2, 600).serialize();
    short[20..22].copy_from_slice(&0x1234u16.to_le_bytes());
    fs.device.bytes[root + 32..root + 64].copy_from_slice(&short);

    let mut subdirectory = regular_entry(ShortName(*b"SUBDIR     "), 4, 0).serialize();
    subdirectory[11] = RawDirEntry::ATTR_DIRECTORY;
    fs.device.bytes[root + 64..root + 96].copy_from_slice(&subdirectory);
    fs.device.bytes[root + 96] = 0;

    set_fat16(&mut fs.device.bytes, 1, 4, 0xffff);
    set_fat16(&mut fs.device.bytes, 1, 5, 0xffff);
    let subdirectory_offset = 6 * 512;
    let inner = regular_entry(ShortName(*b"INNER   TXT"), 5, 4);
    fs.device.bytes[subdirectory_offset..subdirectory_offset + 32]
        .copy_from_slice(&inner.serialize());
    fs.device.bytes[subdirectory_offset + 32] = 0;
    fs.device.bytes[7 * 512..7 * 512 + 4].copy_from_slice(b"test");

    fs.device.bytes[4 * 512..5 * 512].fill(b'a');
    fs.device.bytes[5 * 512..6 * 512].fill(b'b');

    let mut file = fs.open_file("/Long File.txt").unwrap();
    let mut output = [0; 128];
    assert_eq!(fs.read_file_at(&mut file, 500, &mut output).unwrap(), 100);
    assert_eq!(&output[..12], &[b'a'; 12]);
    assert_eq!(&output[12..100], &[b'b'; 88]);

    let mut short_file = fs.open_file("longfi~1.txt").unwrap();
    assert_eq!(
        fs.read_file_at(&mut short_file, 600, &mut output).unwrap(),
        0
    );

    let mut inner_file = fs.open_file("/SUBDIR/INNER.TXT").unwrap();
    assert_eq!(fs.read_file_at(&mut inner_file, 0, &mut output).unwrap(), 4);
    assert_eq!(&output[..4], b"test");
}

#[test]
fn allocation_updates_all_fat_copies_and_frees_the_chain() {
    let mut fs = test_fs(FatType::Fat16);
    for fat_sector in [1, 2] {
        set_fat16(&mut fs.device.bytes, fat_sector, 2, 3);
        set_fat16(&mut fs.device.bytes, fat_sector, 3, 0xffff);
    }

    let allocated = fs.allocate_cluster(Some(Cluster::new(3).unwrap())).unwrap();
    assert_eq!(allocated, Cluster::new(4).unwrap());
    for fat_sector in [1, 2] {
        assert_eq!(get_fat16(&fs.device.bytes, fat_sector, 3), 4);
        assert_eq!(get_fat16(&fs.device.bytes, fat_sector, 4), 0xffff);
    }

    fs.free_chain(Cluster::new(2).unwrap()).unwrap();
    for fat_sector in [1, 2] {
        for cluster in 2..=4 {
            assert_eq!(get_fat16(&fs.device.bytes, fat_sector, cluster), 0);
        }
    }
}

#[test]
fn custom_file_handle_can_resolve_all_clusters_without_fat_reads() {
    let mut inner = test_fs(FatType::Fat16);
    let root = 3 * 512;
    let entry = regular_entry(ShortName(*b"INDEXED BIN"), 2, 600);
    inner.device.bytes[root..root + 32].copy_from_slice(&entry.serialize());
    inner.device.bytes[root + 32] = 0;
    inner.device.bytes[4 * 512..5 * 512].fill(b'a');
    inner.device.bytes[5 * 512..6 * 512].fill(b'b');
    let mut fs = ClmtFs {
        inner,
        initial_lookups: 0,
        sequential_lookups: 0,
    };

    let mut file = fs.open_file("INDEXED.BIN").unwrap();
    let mut output = [0; 100];
    assert_eq!(fs.read_file_at(&mut file, 500, &mut output).unwrap(), 100);
    assert_eq!(fs.initial_lookups, 1);
    assert_eq!(fs.sequential_lookups, 1);
    assert_eq!(&output[..12], &[b'a'; 12]);
    assert_eq!(&output[12..], &[b'b'; 88]);
}

#[test]
fn creates_writes_extends_and_truncates_a_file() {
    let mut fs = test_fs(FatType::Fat16);
    fs.device.bytes[4 * 512..].fill(0xa5);
    let mut file = fs.create_file("/created long name.bin").unwrap();
    let payload = [0x5a; 600];

    assert_eq!(fs.write_file_at(&mut file, 100, &payload).unwrap(), 600);
    assert_eq!(file.file_info().length, 700);
    let mut contents = [0xff; 700];
    assert_eq!(fs.read_file_at(&mut file, 0, &mut contents).unwrap(), 700);
    assert_eq!(&contents[..100], &[0; 100]);
    assert_eq!(&contents[100..], &payload);
    assert!(fs.open_file("/created long name.bin").is_ok());

    fs.truncate_file(&mut file, 200).unwrap();
    assert_eq!(file.file_info().length, 200);
    assert_eq!(
        fs.read_fat_entry(Cluster::new(3).unwrap()).unwrap(),
        FatEntry::Free
    );

    fs.truncate_file(&mut file, 700).unwrap();
    let mut regrown = [0xff; 500];
    assert_eq!(fs.read_file_at(&mut file, 200, &mut regrown).unwrap(), 500);
    assert_eq!(regrown, [0; 500]);
}

#[test]
fn creates_moves_and_removes_directories_and_files() {
    let mut fs = test_fs(FatType::Fat16);
    fs.create_directory("/Parent").unwrap();
    fs.create_directory("/Parent/Child").unwrap();
    let mut file = fs.create_file("/Parent/Child/data.bin").unwrap();
    fs.write_file_at(&mut file, 0, b"payload").unwrap();

    assert!(matches!(
        fs.remove_directory("/Parent"),
        Err(fatsd::Error::NotEmpty)
    ));
    assert!(matches!(
        fs.rename("/Parent", "/Parent/Child/Loop"),
        Err(fatsd::Error::InvalidPath)
    ));

    fs.rename("/Parent/Child/data.bin", "/Parent/renamed long name.bin")
        .unwrap();
    assert!(matches!(
        fs.open_file("/Parent/Child/data.bin"),
        Err(fatsd::Error::NotFound)
    ));
    let mut renamed = fs.open_file("/Parent/renamed long name.bin").unwrap();
    let mut contents = [0; 7];
    fs.read_file_at(&mut renamed, 0, &mut contents).unwrap();
    assert_eq!(&contents, b"payload");

    fs.rename("/Parent/Child", "/Moved").unwrap();
    assert_eq!(
        fs.open_directory("/Moved/..").unwrap().directory_info(),
        fs.root_directory().directory_info()
    );

    fs.remove_directory("/Moved").unwrap();
    fs.remove_file("/Parent/renamed long name.bin").unwrap();
    fs.remove_directory("/Parent").unwrap();
    assert!(matches!(
        fs.open_directory("/Parent"),
        Err(fatsd::Error::NotFound)
    ));
}

#[test]
fn removing_by_short_alias_also_deletes_the_lfn_entries() {
    let mut fs = test_fs(FatType::Fat16);
    let file = fs.create_file("/remove long name.bin").unwrap();
    let short_index = file.file_info().entry.index;

    fs.remove_file("/REMOVE~1.BIN").unwrap();
    let root = fs.root_directory();
    for index in 0..=short_index {
        assert!(
            fs.read_directory_entry(&root, index)
                .unwrap()
                .unwrap()
                .is_deleted()
        );
    }
}

#[test]
fn grows_a_directory_chain_when_entries_fill_a_cluster() {
    let mut fs = test_fs(FatType::Fat16);
    let directory = fs.create_directory("/D").unwrap();
    for index in 0..20 {
        fs.create_file(&format!("/D/F{index:02}.TXT")).unwrap();
    }
    assert!(fs.open_file("/D/F19.TXT").is_ok());
    let DirectoryLocation::Cluster(first) = directory.directory_info().location else {
        panic!("new directory must use a cluster");
    };
    assert_eq!(fs.chain_tail_and_length(first).unwrap().1, 2);
}

#[test]
fn rejects_mutation_of_a_read_only_file() {
    let mut fs = test_fs(FatType::Fat16);
    let file = fs.create_file("/READONLY.BIN").unwrap();
    let mut raw = fs.read_directory_entry_at(file.file_info().entry).unwrap();
    raw.0[11] |= RawDirEntry::ATTR_READ_ONLY;
    fs.write_directory_entry(file.file_info().entry, raw)
        .unwrap();
    let mut file = fs.open_file("/READONLY.BIN").unwrap();

    assert!(matches!(
        fs.write_file_at(&mut file, 0, b"x"),
        Err(fatsd::Error::ReadOnly)
    ));
    assert!(matches!(
        fs.truncate_file(&mut file, 1),
        Err(fatsd::Error::ReadOnly)
    ));
    assert!(matches!(
        fs.remove_file("/READONLY.BIN"),
        Err(fatsd::Error::ReadOnly)
    ));
}

fn set_fat16(bytes: &mut [u8], fat_sector: usize, cluster: usize, value: u16) {
    let offset = fat_sector * 512 + cluster * 2;
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn get_fat16(bytes: &[u8], fat_sector: usize, cluster: usize) -> u16 {
    let offset = fat_sector * 512 + cluster * 2;
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn regular_entry(name: ShortName, first_cluster: u16, length: u32) -> RawDirEntry {
    let mut raw = [0; 32];
    raw[..11].copy_from_slice(&name.0);
    raw[11] = 0x20;
    raw[26..28].copy_from_slice(&first_cluster.to_le_bytes());
    raw[28..32].copy_from_slice(&length.to_le_bytes());
    RawDirEntry(raw)
}
