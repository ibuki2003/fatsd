//! Block, cluster-chain, directory, file, and allocation access traits.

use core::cmp;

use crate::{
    format::{
        Bpb, DIRECTORY_ENTRY_SIZE, FatEntry, FatType, FormatError, LfnEntry, RawDirEntry, ShortName,
    },
    handle::{
        DirectoryEntryLocation, DirectoryHandle, DirectoryInfo, DirectoryLocation, FileHandle,
        FileInfo,
    },
    volume::{Cluster, Volume},
};

/// An error produced while accessing a FAT volume.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error<E> {
    /// The underlying block device returned an error.
    Io(E),
    /// On-disk data is not a valid FAT representation.
    InvalidFormat(FormatError),
    /// A cluster chain is inconsistent.
    CorruptChain,
    /// The requested entry does not exist.
    NotFound,
    /// The requested entry is not a file.
    NotAFile,
    /// The requested entry is not a directory.
    NotADirectory,
    /// The path cannot identify an entry.
    InvalidPath,
    /// No free cluster is available.
    NoSpace,
    /// An entry with the requested name already exists.
    AlreadyExists,
    /// The directory cannot hold another entry.
    DirectoryFull,
    /// The requested name exceeds the FAT limit.
    NameTooLong,
    /// The caller-provided name buffer is too small.
    NameBufferTooSmall,
    /// The requested name is not valid for FAT.
    InvalidName,
    /// The directory still contains entries.
    NotEmpty,
    /// The entry is marked read-only.
    ReadOnly,
    /// An offset or length is outside the supported range.
    OutOfBounds,
}

impl<E> From<FormatError> for Error<E> {
    fn from(value: FormatError) -> Self {
        Self::InvalidFormat(value)
    }
}

/// Provides partial reads from fixed-size physical blocks.
#[maybe_async_cfg::maybe(
    idents(BlockAccess(sync, async = "AsyncBlockAccess")),
    sync(feature = "sync"),
    async(feature = "async")
)]
pub trait BlockAccess {
    /// Error returned by the block device.
    type Error;

    /// Returns the physical block size in bytes.
    fn block_size(&self) -> usize;

    /// Reads a range contained in one physical block.
    ///
    /// Implementations may assume `offset + out.len() <= self.block_size()`.
    async fn read_block_at(
        &mut self,
        block: u64,
        offset: usize,
        out: &mut [u8],
    ) -> Result<(), Self::Error>;
}

/// Provides partial writes to fixed-size physical blocks.
#[maybe_async_cfg::maybe(
    idents(
        BlockAccess(sync, async = "AsyncBlockAccess"),
        BlockWrite(sync, async = "AsyncBlockWrite")
    ),
    sync(feature = "sync"),
    async(feature = "async")
)]
pub trait BlockWrite: BlockAccess {
    /// Writes a range contained in one physical block.
    ///
    /// Implementations may assume `offset + data.len() <= self.block_size()`.
    async fn write_block_at(
        &mut self,
        block: u64,
        offset: usize,
        data: &[u8],
    ) -> Result<(), Self::Error>;
}

#[maybe_async_cfg::maybe(
    idents(BlockAccess(sync, async = "AsyncBlockAccess")),
    sync(feature = "sync"),
    async(feature = "async")
)]
impl<T: BlockAccess + ?Sized> BlockAccess for &mut T {
    type Error = T::Error;

    fn block_size(&self) -> usize {
        (**self).block_size()
    }

    async fn read_block_at(
        &mut self,
        block: u64,
        offset: usize,
        out: &mut [u8],
    ) -> Result<(), Self::Error> {
        (**self).read_block_at(block, offset, out).await
    }
}

#[maybe_async_cfg::maybe(
    idents(
        BlockAccess(sync, async = "AsyncBlockAccess"),
        BlockWrite(sync, async = "AsyncBlockWrite")
    ),
    sync(feature = "sync"),
    async(feature = "async")
)]
impl<T: BlockWrite + ?Sized> BlockWrite for &mut T {
    async fn write_block_at(
        &mut self,
        block: u64,
        offset: usize,
        data: &[u8],
    ) -> Result<(), Self::Error> {
        (**self).write_block_at(block, offset, data).await
    }
}

#[maybe_async_cfg::maybe(
    idents(
        BlockAccess(sync, async = "AsyncBlockAccess"),
        read_device_at(fn, sync, async = "read_device_at_async")
    ),
    sync(feature = "sync"),
    async(feature = "async")
)]
async fn read_device_at<T: BlockAccess + ?Sized>(
    device: &mut T,
    mut byte_offset: u64,
    mut out: &mut [u8],
) -> Result<(), Error<T::Error>> {
    let block_size = device.block_size();
    if block_size == 0 {
        return Err(Error::OutOfBounds);
    }
    while !out.is_empty() {
        let block = byte_offset / block_size as u64;
        let offset = (byte_offset % block_size as u64) as usize;
        let length = cmp::min(block_size - offset, out.len());
        let (part, rest) = out.split_at_mut(length);
        device
            .read_block_at(block, offset, part)
            .await
            .map_err(Error::Io)?;
        byte_offset = byte_offset
            .checked_add(length as u64)
            .ok_or(Error::OutOfBounds)?;
        out = rest;
    }
    Ok(())
}

#[maybe_async_cfg::maybe(
    idents(
        BlockWrite(sync, async = "AsyncBlockWrite"),
        write_device_at(fn, sync, async = "write_device_at_async")
    ),
    sync(feature = "sync"),
    async(feature = "async")
)]
async fn write_device_at<T: BlockWrite + ?Sized>(
    device: &mut T,
    mut byte_offset: u64,
    mut data: &[u8],
) -> Result<(), Error<T::Error>> {
    let block_size = device.block_size();
    if block_size == 0 {
        return Err(Error::OutOfBounds);
    }
    while !data.is_empty() {
        let block = byte_offset / block_size as u64;
        let offset = (byte_offset % block_size as u64) as usize;
        let length = cmp::min(block_size - offset, data.len());
        let (part, rest) = data.split_at(length);
        device
            .write_block_at(block, offset, part)
            .await
            .map_err(Error::Io)?;
        byte_offset = byte_offset
            .checked_add(length as u64)
            .ok_or(Error::OutOfBounds)?;
        data = rest;
    }
    Ok(())
}

/// Reads and validates the volume metadata from a block device.
#[maybe_async_cfg::maybe(
    idents(
        BlockAccess(sync, async = "AsyncBlockAccess"),
        read_device_at(fn, sync, async = "read_device_at_async"),
        read_volume(fn, sync, async = "read_volume_async")
    ),
    sync(feature = "sync"),
    async(feature = "async")
)]
pub async fn read_volume<T: BlockAccess + ?Sized>(
    device: &mut T,
) -> Result<Volume, Error<T::Error>> {
    let mut boot_sector = [0; 512];
    read_device_at(device, 0, &mut boot_sector).await?;
    Ok(Volume::from_bpb(Bpb::parse(&boot_sector)?)?)
}

/// Provides FAT cluster-chain access.
#[maybe_async_cfg::maybe(
    idents(
        BlockAccess(sync, async = "AsyncBlockAccess"),
        ChainAccess(sync, async = "AsyncChainAccess"),
        read_device_at(fn, sync, async = "read_device_at_async")
    ),
    sync(feature = "sync"),
    async(feature = "async")
)]
pub trait ChainAccess: BlockAccess {
    /// Returns the mounted volume metadata.
    fn volume(&self) -> &Volume;

    /// Reads bytes at an absolute volume offset.
    async fn read_volume_at(
        &mut self,
        byte_offset: u64,
        out: &mut [u8],
    ) -> Result<(), Error<Self::Error>> {
        read_device_at(self, byte_offset, out).await
    }

    /// Reads the FAT entry for a cluster.
    async fn read_fat_entry(&mut self, cluster: Cluster) -> Result<FatEntry, Error<Self::Error>> {
        validate_cluster(self.volume(), cluster)?;
        let volume = *self.volume();
        let fat_start_sector =
            volume.fat_start_sector + volume.active_fat() as u32 * volume.fat_sectors();
        let fat_start = volume.sector_byte_offset(fat_start_sector);
        match volume.fat_type {
            FatType::Fat12 => {
                let mut bytes = [0; 2];
                let index = cluster.get() as u64;
                self.read_volume_at(fat_start + index + index / 2, &mut bytes)
                    .await?;
                Ok(FatEntry::parse(FatType::Fat12, cluster.get(), &bytes)?)
            }
            FatType::Fat16 => {
                let mut bytes = [0; 2];
                self.read_volume_at(fat_start + cluster.get() as u64 * 2, &mut bytes)
                    .await?;
                Ok(FatEntry::parse(FatType::Fat16, cluster.get(), &bytes)?)
            }
            FatType::Fat32 => {
                let mut bytes = [0; 4];
                self.read_volume_at(fat_start + cluster.get() as u64 * 4, &mut bytes)
                    .await?;
                Ok(FatEntry::parse(FatType::Fat32, cluster.get(), &bytes)?)
            }
        }
    }

    /// Returns the next cluster, or `None` at the end of the chain.
    async fn next_cluster(
        &mut self,
        cluster: Cluster,
    ) -> Result<Option<Cluster>, Error<Self::Error>> {
        match self.read_fat_entry(cluster).await? {
            FatEntry::Data(next) => {
                let next = Cluster::new(next).ok_or(Error::CorruptChain)?;
                validate_cluster(self.volume(), next)?;
                Ok(Some(next))
            }
            FatEntry::EndOfChain => Ok(None),
            FatEntry::Free | FatEntry::Bad | FatEntry::Reserved(_) => Err(Error::CorruptChain),
        }
    }

    /// Resolves the cluster at `index` in a chain.
    ///
    /// The default implementation walks the FAT chain from `first`.
    async fn cluster_at(
        &mut self,
        first: Cluster,
        index: u32,
    ) -> Result<Option<Cluster>, Error<Self::Error>> {
        validate_cluster(self.volume(), first)?;
        let mut cluster = first;
        for _ in 0..index {
            let next = self.next_cluster(cluster).await?;
            let Some(next) = next else {
                return Ok(None);
            };
            cluster = next;
        }
        Ok(Some(cluster))
    }

    /// Reads bytes at an offset within a cluster chain.
    async fn read_chain_at(
        &mut self,
        first: Cluster,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize, Error<Self::Error>> {
        let cluster_size = self.volume().cluster_size() as u64;
        let cluster_index = offset / cluster_size;
        let mut within_cluster = (offset % cluster_size) as usize;
        let cluster = self
            .cluster_at(
                first,
                u32::try_from(cluster_index).map_err(|_| Error::OutOfBounds)?,
            )
            .await?;
        let Some(mut cluster) = cluster else {
            return Ok(0);
        };
        let mut read = 0;
        while read < out.len() {
            let length = cmp::min(
                self.volume().cluster_size() - within_cluster,
                out.len() - read,
            );
            let byte_offset = self
                .volume()
                .cluster_byte_offset(cluster)
                .ok_or(Error::CorruptChain)?
                + within_cluster as u64;
            self.read_volume_at(byte_offset, &mut out[read..read + length])
                .await?;
            read += length;
            within_cluster = 0;
            if read < out.len() {
                let next = self.next_cluster(cluster).await?;
                let Some(next) = next else {
                    break;
                };
                cluster = next;
            }
        }
        Ok(read)
    }
}

fn validate_cluster<E>(volume: &Volume, cluster: Cluster) -> Result<(), Error<E>> {
    if cluster.get() > volume.max_cluster() {
        Err(Error::CorruptChain)
    } else {
        Ok(())
    }
}

/// A directory entry together with its on-disk location.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FoundEntry {
    /// Raw short directory entry.
    pub raw: RawDirEntry,
    /// Location of the short directory entry.
    pub location: DirectoryEntryLocation,
    /// Index of the first associated long-name entry, if any.
    pub lfn_start_index: Option<u32>,
}

/// A visible directory entry returned during directory enumeration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectoryEntry {
    /// Raw short directory entry.
    pub raw: RawDirEntry,
    /// Location of the short directory entry.
    pub location: DirectoryEntryLocation,
    /// Index of the first associated long-name entry, if the name buffer contains an LFN.
    pub lfn_start_index: Option<u32>,
    /// Number of UTF-16 code units written to the caller-provided name buffer.
    pub name_length: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct LfnSequence {
    active: bool,
    expected_ordinal: u8,
    checksum: u8,
    start_index: u32,
    name_length: usize,
}

impl LfnSequence {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn push(&mut self, entry: LfnEntry, index: u32, name: &mut [u16]) {
        if entry.is_last {
            self.active = true;
            self.expected_ordinal = entry.ordinal;
            self.checksum = entry.checksum;
            self.start_index = index;
            self.name_length = entry.ordinal as usize * 13;
        }
        if !self.active || entry.ordinal != self.expected_ordinal || entry.checksum != self.checksum
        {
            self.reset();
            return;
        }

        let start = (entry.ordinal as usize - 1) * 13;
        for (offset, character) in entry.characters.into_iter().enumerate() {
            let position = start + offset;
            if matches!(character, 0 | 0xffff) {
                self.name_length = cmp::min(self.name_length, position);
            }
            if let Some(target) = name.get_mut(position) {
                *target = character;
            }
        }
        self.expected_ordinal -= 1;
    }

    fn finish(&mut self, short_name: ShortName) -> Option<(u32, usize)> {
        let result = (self.active
            && self.expected_ordinal == 0
            && self.name_length != 0
            && short_name.checksum() == self.checksum)
            .then_some((self.start_index, self.name_length));
        self.reset();
        result
    }
}

/// Provides directory traversal and lookup.
#[maybe_async_cfg::maybe(
    idents(
        BlockAccess(sync, async = "AsyncBlockAccess"),
        ChainAccess(sync, async = "AsyncChainAccess"),
        DirectoryAccess(sync, async = "AsyncDirectoryAccess")
    ),
    sync(feature = "sync"),
    async(feature = "async")
)]
pub trait DirectoryAccess: ChainAccess {
    /// Application-owned directory handle type.
    type DirectoryHandle: DirectoryHandle + From<DirectoryInfo>;

    /// Returns a handle for the root directory.
    fn root_directory(&self) -> Self::DirectoryHandle {
        DirectoryInfo {
            location: self.volume().root_directory(),
        }
        .into()
    }

    /// Reads bytes at an offset within a directory.
    async fn read_directory_at(
        &mut self,
        directory: &Self::DirectoryHandle,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize, Error<Self::Error>> {
        match directory.directory_info().location {
            DirectoryLocation::FixedRoot => {
                let volume = *self.volume();
                let length = volume.root_directory_sectors as u64 * volume.sector_size() as u64;
                if offset >= length {
                    return Ok(0);
                }
                let read = cmp::min(out.len() as u64, length - offset) as usize;
                let start = volume.sector_byte_offset(volume.root_directory_start_sector) + offset;
                self.read_volume_at(start, &mut out[..read]).await?;
                Ok(read)
            }
            DirectoryLocation::Cluster(first) => self.read_chain_at(first, offset, out).await,
        }
    }

    /// Reads a directory entry by index.
    async fn read_directory_entry(
        &mut self,
        directory: &Self::DirectoryHandle,
        index: u32,
    ) -> Result<Option<RawDirEntry>, Error<Self::Error>> {
        let mut bytes = [0; DIRECTORY_ENTRY_SIZE];
        let offset = index as u64 * DIRECTORY_ENTRY_SIZE as u64;
        if self
            .read_directory_at(directory, offset, &mut bytes)
            .await?
            != bytes.len()
        {
            return Ok(None);
        }
        let entry = RawDirEntry(bytes);
        Ok((!entry.is_end()).then_some(entry))
    }

    /// Reads the next visible entry and its display name from a directory.
    ///
    /// `cursor` is the raw entry index at which scanning starts and is updated to the entry after
    /// the returned short entry. Deleted entries, volume labels, and LFN entries are not returned.
    /// A structurally invalid LFN sequence falls back to the associated short name. If `name` is
    /// too small, the cursor is left unchanged. Short-name bytes are widened to UTF-16, with the
    /// ASCII lowercase flags applied.
    async fn read_next_directory_entry(
        &mut self,
        directory: &Self::DirectoryHandle,
        cursor: &mut u32,
        name: &mut [u16],
    ) -> Result<Option<DirectoryEntry>, Error<Self::Error>> {
        let mut index = *cursor;
        let mut lfn = LfnSequence::default();
        loop {
            let raw = self.read_directory_entry(directory, index).await?;
            let Some(raw) = raw else {
                *cursor = index;
                return Ok(None);
            };
            let next_index = index.checked_add(1).ok_or(Error::OutOfBounds)?;
            if raw.is_deleted() {
                lfn.reset();
                index = next_index;
                continue;
            }
            if raw.is_lfn() {
                match LfnEntry::parse(raw) {
                    Ok(entry) => lfn.push(entry, index, name),
                    Err(_) => lfn.reset(),
                }
                index = next_index;
                continue;
            }

            let long_name = lfn.finish(raw.short_name());
            if raw.is_volume_label() {
                index = next_index;
                continue;
            }
            let (lfn_start_index, name_length) = if let Some((start, length)) = long_name {
                if length > name.len() {
                    return Err(Error::NameBufferTooSmall);
                }
                (Some(start), length)
            } else {
                (None, write_short_name(&raw, name)?)
            };
            *cursor = next_index;
            return Ok(Some(DirectoryEntry {
                raw,
                location: DirectoryEntryLocation {
                    directory: directory.directory_info().location,
                    index,
                },
                lfn_start_index,
                name_length,
            }));
        }
    }

    /// Finds a named entry directly inside a directory.
    async fn find_entry(
        &mut self,
        directory: &Self::DirectoryHandle,
        name: &str,
    ) -> Result<FoundEntry, Error<Self::Error>> {
        if name.is_empty() || name.contains('/') {
            return Err(Error::InvalidPath);
        }
        let mut byte_offset = 0u64;
        let mut batch = [0; 512];
        let mut long_name = [0xffff; 260];
        let mut lfn = LfnSequence::default();

        loop {
            let read = self
                .read_directory_at(directory, byte_offset, &mut batch)
                .await?;
            if read == 0 {
                return Err(Error::NotFound);
            }
            let (entries, _) = batch[..read].as_chunks::<DIRECTORY_ENTRY_SIZE>();
            for (batch_index, raw_bytes) in entries.iter().enumerate() {
                let entry_index =
                    u32::try_from(byte_offset / DIRECTORY_ENTRY_SIZE as u64 + batch_index as u64)
                        .map_err(|_| Error::OutOfBounds)?;
                let raw = RawDirEntry(*raw_bytes);
                if raw.is_end() {
                    return Err(Error::NotFound);
                }
                if raw.is_deleted() {
                    lfn.reset();
                    continue;
                }
                if raw.is_lfn() {
                    match LfnEntry::parse(raw) {
                        Ok(entry) => lfn.push(entry, entry_index, &mut long_name),
                        Err(_) => lfn.reset(),
                    }
                    continue;
                }

                let valid_lfn = lfn.finish(raw.short_name());
                let lfn_matches = valid_lfn
                    .is_some_and(|(_, length)| long_name_matches(&long_name[..length], name));
                let short_matches = raw.short_name().matches(name);
                if !raw.is_volume_label() && (lfn_matches || short_matches) {
                    return Ok(FoundEntry {
                        raw,
                        location: DirectoryEntryLocation {
                            directory: directory.directory_info().location,
                            index: entry_index,
                        },
                        lfn_start_index: valid_lfn.map(|(start, _)| start),
                    });
                }
            }
            let consumed = entries.len() * DIRECTORY_ENTRY_SIZE;
            if consumed == 0 {
                return Err(Error::NotFound);
            }
            byte_offset = byte_offset
                .checked_add(consumed as u64)
                .ok_or(Error::OutOfBounds)?;
            if read < batch.len() {
                return Err(Error::NotFound);
            }
        }
    }

    /// Finds an entry by path from the root directory.
    async fn find_entry_by_path(&mut self, path: &str) -> Result<FoundEntry, Error<Self::Error>> {
        let mut components = path
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".");
        let first = components.next().ok_or(Error::InvalidPath)?;
        let mut directory = self.root_directory();
        let mut entry = self.find_entry(&directory, first).await?;
        for component in components {
            if !entry.raw.is_directory() {
                return Err(Error::NotADirectory);
            }
            directory = DirectoryInfo {
                location: directory_location(self.volume(), &entry.raw)?,
            }
            .into();
            entry = self.find_entry(&directory, component).await?;
        }
        Ok(entry)
    }

    /// Opens a directory by path from the root directory.
    async fn open_directory(
        &mut self,
        path: &str,
    ) -> Result<Self::DirectoryHandle, Error<Self::Error>> {
        if path.split('/').all(|part| part.is_empty() || part == ".") {
            return Ok(self.root_directory());
        }
        let entry = self.find_entry_by_path(path).await?;
        if !entry.raw.is_directory() {
            return Err(Error::NotADirectory);
        }
        Ok(DirectoryInfo {
            location: directory_location(self.volume(), &entry.raw)?,
        }
        .into())
    }
}

fn long_name_matches(buffer: &[u16], name: &str) -> bool {
    name.encode_utf16().eq(buffer.iter().copied())
}

fn write_short_name<E>(raw: &RawDirEntry, out: &mut [u16]) -> Result<usize, Error<E>> {
    let short = raw.short_name().0;
    let base_length = short[..8]
        .iter()
        .rposition(|byte| *byte != b' ')
        .map_or(0, |index| index + 1);
    let extension_length = short[8..]
        .iter()
        .rposition(|byte| *byte != b' ')
        .map_or(0, |index| index + 1);
    let length = base_length + usize::from(extension_length != 0) + extension_length;
    if length > out.len() {
        return Err(Error::NameBufferTooSmall);
    }

    let lowercase_base = raw.0[12] & 0x08 != 0;
    let lowercase_extension = raw.0[12] & 0x10 != 0;
    for (index, byte) in short[..base_length].iter().copied().enumerate() {
        let byte = if index == 0 && byte == 0x05 {
            0xe5
        } else {
            byte
        };
        out[index] = short_name_code_unit(byte, lowercase_base);
    }
    if extension_length != 0 {
        out[base_length] = b'.' as u16;
        for (index, byte) in short[8..8 + extension_length].iter().copied().enumerate() {
            out[base_length + 1 + index] = short_name_code_unit(byte, lowercase_extension);
        }
    }
    Ok(length)
}

fn short_name_code_unit(byte: u8, lowercase: bool) -> u16 {
    if lowercase && byte.is_ascii_uppercase() {
        byte.to_ascii_lowercase() as u16
    } else {
        byte as u16
    }
}

fn directory_location<E>(
    volume: &Volume,
    entry: &RawDirEntry,
) -> Result<DirectoryLocation, Error<E>> {
    let raw_cluster = entry_first_cluster(volume, entry);
    if raw_cluster == 0 {
        if entry.short_name().matches("..") {
            Ok(volume.root_directory())
        } else {
            Err(Error::CorruptChain)
        }
    } else {
        Cluster::new(raw_cluster)
            .map(DirectoryLocation::Cluster)
            .ok_or(Error::CorruptChain)
    }
}

fn entry_first_cluster(volume: &Volume, entry: &RawDirEntry) -> u32 {
    match volume.fat_type {
        FatType::Fat12 | FatType::Fat16 => entry.first_cluster_low() as u32,
        FatType::Fat32 => entry.first_cluster() & 0x0fff_ffff,
    }
}

/// Provides file lookup and reading.
#[maybe_async_cfg::maybe(
    idents(
        BlockAccess(sync, async = "AsyncBlockAccess"),
        ChainAccess(sync, async = "AsyncChainAccess"),
        DirectoryAccess(sync, async = "AsyncDirectoryAccess"),
        FileAccess(sync, async = "AsyncFileAccess")
    ),
    sync(feature = "sync"),
    async(feature = "async")
)]
pub trait FileAccess: DirectoryAccess {
    /// Application-owned file handle type.
    type FileHandle: FileHandle + From<FileInfo>;

    /// Opens a file by path from the root directory.
    async fn open_file(&mut self, path: &str) -> Result<Self::FileHandle, Error<Self::Error>> {
        let entry = self.find_entry_by_path(path).await?;
        if entry.raw.is_directory() || entry.raw.is_volume_label() {
            return Err(Error::NotAFile);
        }
        let length = entry.raw.file_size() as u64;
        let first_cluster = Cluster::new(entry_first_cluster(self.volume(), &entry.raw));
        if length != 0 && first_cluster.is_none() {
            return Err(Error::CorruptChain);
        }
        Ok(FileInfo {
            first_cluster,
            length,
            attributes: entry.raw.attributes(),
            entry: entry.location,
        }
        .into())
    }

    /// Resolves the cluster at `index` for a file handle.
    ///
    /// The default implementation delegates to the chain-level `cluster_at` method.
    async fn resolve_file_cluster(
        &mut self,
        file: &mut Self::FileHandle,
        index: u32,
    ) -> Result<Option<Cluster>, Error<Self::Error>> {
        let Some(first) = file.file_info().first_cluster else {
            return Ok(None);
        };
        self.cluster_at(first, index).await
    }

    /// Resolves the cluster following `current` during sequential file access.
    ///
    /// The default implementation delegates to the chain-level `next_cluster` method.
    async fn resolve_next_file_cluster(
        &mut self,
        _file: &mut Self::FileHandle,
        current: Cluster,
        _next_index: u32,
    ) -> Result<Option<Cluster>, Error<Self::Error>> {
        self.next_cluster(current).await
    }

    /// Reads file data at an absolute file offset.
    async fn read_file_at(
        &mut self,
        file: &mut Self::FileHandle,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize, Error<Self::Error>> {
        let info = *file.file_info();
        if offset >= info.length || out.is_empty() {
            return Ok(0);
        }
        let requested = cmp::min(out.len() as u64, info.length - offset) as usize;
        let cluster_size = self.volume().cluster_size() as u64;
        let mut cluster_index =
            u32::try_from(offset / cluster_size).map_err(|_| Error::OutOfBounds)?;
        let mut within_cluster = (offset % cluster_size) as usize;
        let mut read = 0;
        let mut cluster = self
            .resolve_file_cluster(file, cluster_index)
            .await?
            .ok_or(Error::CorruptChain)?;
        while read < requested {
            let length = cmp::min(
                self.volume().cluster_size() - within_cluster,
                requested - read,
            );
            let byte_offset = self
                .volume()
                .cluster_byte_offset(cluster)
                .ok_or(Error::CorruptChain)?
                + within_cluster as u64;
            self.read_volume_at(byte_offset, &mut out[read..read + length])
                .await?;
            read += length;
            within_cluster = 0;
            if read < requested {
                cluster_index = cluster_index.checked_add(1).ok_or(Error::OutOfBounds)?;
                cluster = self
                    .resolve_next_file_cluster(file, cluster, cluster_index)
                    .await?
                    .ok_or(Error::CorruptChain)?;
            }
        }
        Ok(read)
    }
}

/// Provides cluster allocation and chain mutation.
#[maybe_async_cfg::maybe(
    idents(
        BlockAccess(sync, async = "AsyncBlockAccess"),
        BlockWrite(sync, async = "AsyncBlockWrite"),
        ChainAccess(sync, async = "AsyncChainAccess"),
        AllocationAccess(sync, async = "AsyncAllocationAccess"),
        write_device_at(fn, sync, async = "write_device_at_async")
    ),
    sync(feature = "sync"),
    async(feature = "async")
)]
pub trait AllocationAccess: ChainAccess + BlockWrite {
    /// Writes bytes at an absolute volume offset.
    async fn write_volume_at(
        &mut self,
        byte_offset: u64,
        data: &[u8],
    ) -> Result<(), Error<Self::Error>> {
        write_device_at(self, byte_offset, data).await
    }

    /// Writes a FAT entry to every active FAT copy.
    async fn write_fat_entry(
        &mut self,
        cluster: Cluster,
        value: FatEntry,
    ) -> Result<(), Error<Self::Error>> {
        validate_cluster(self.volume(), cluster)?;
        let volume = *self.volume();
        let first_fat = if volume.fat_mirroring_enabled() {
            0
        } else {
            volume.active_fat()
        };
        let fat_count = if volume.fat_mirroring_enabled() {
            volume.bpb.fat_count
        } else {
            first_fat + 1
        };
        for fat in first_fat..fat_count {
            let start_sector = volume.fat_start_sector + fat as u32 * volume.fat_sectors();
            let start = volume.sector_byte_offset(start_sector);
            self.write_one_fat_entry(start, cluster, value).await?;
        }
        self.invalidate_fs_info().await?;
        Ok(())
    }

    /// Invalidates free-space hints in a FAT32 FSInfo sector.
    async fn invalidate_fs_info(&mut self) -> Result<(), Error<Self::Error>> {
        let volume = *self.volume();
        let sector = volume.bpb.fs_info_sector;
        if volume.fat_type == FatType::Fat32
            && sector != 0
            && sector != u16::MAX
            && sector < volume.bpb.reserved_sector_count
        {
            let offset = volume.sector_byte_offset(sector as u32) + 488;
            self.write_volume_at(offset, &[0xff; 8]).await?;
        }
        Ok(())
    }

    /// Writes an entry to one FAT copy.
    async fn write_one_fat_entry(
        &mut self,
        fat_start: u64,
        cluster: Cluster,
        value: FatEntry,
    ) -> Result<(), Error<Self::Error>> {
        let fat_type = self.volume().fat_type;
        match fat_type {
            FatType::Fat12 => {
                let index = cluster.get() as u64;
                let offset = fat_start + index + index / 2;
                let mut bytes = [0; 2];
                self.read_volume_at(offset, &mut bytes).await?;
                value.serialize_into(fat_type, cluster.get(), &mut bytes)?;
                self.write_volume_at(offset, &bytes).await
            }
            FatType::Fat16 => {
                let mut bytes = [0; 2];
                value.serialize_into(fat_type, cluster.get(), &mut bytes)?;
                self.write_volume_at(fat_start + cluster.get() as u64 * 2, &bytes)
                    .await
            }
            FatType::Fat32 => {
                let offset = fat_start + cluster.get() as u64 * 4;
                let mut bytes = [0; 4];
                self.read_volume_at(offset, &mut bytes).await?;
                value.serialize_into(fat_type, cluster.get(), &mut bytes)?;
                self.write_volume_at(offset, &bytes).await
            }
        }
    }

    /// Finds a free cluster, beginning after an optional hint.
    async fn find_free_cluster(
        &mut self,
        start: Option<Cluster>,
    ) -> Result<Cluster, Error<Self::Error>> {
        let max = self.volume().max_cluster();
        let start = start.map_or(2, Cluster::get);
        if start > max {
            return Err(Error::OutOfBounds);
        }
        for raw in (start..=max).chain(2..start) {
            let cluster = Cluster::new(raw).expect("range starts at two");
            if self.read_fat_entry(cluster).await? == FatEntry::Free {
                return Ok(cluster);
            }
        }
        Err(Error::NoSpace)
    }

    /// Allocates and clears a cluster, optionally appending it to a chain.
    async fn allocate_cluster(
        &mut self,
        after: Option<Cluster>,
    ) -> Result<Cluster, Error<Self::Error>> {
        if let Some(previous) = after
            && self.read_fat_entry(previous).await? != FatEntry::EndOfChain
        {
            return Err(Error::CorruptChain);
        }
        let cluster = self.find_free_cluster(after).await?;
        self.write_fat_entry(cluster, FatEntry::EndOfChain).await?;
        self.clear_cluster(cluster).await?;
        if let Some(previous) = after {
            self.write_fat_entry(previous, FatEntry::Data(cluster.get()))
                .await?;
        }
        Ok(cluster)
    }

    /// Fills a cluster with zeroes.
    async fn clear_cluster(&mut self, cluster: Cluster) -> Result<(), Error<Self::Error>> {
        let start = self
            .volume()
            .cluster_byte_offset(cluster)
            .ok_or(Error::CorruptChain)?;
        let mut offset = 0;
        let zeros = [0; 512];
        while offset < self.volume().cluster_size() {
            let length = cmp::min(zeros.len(), self.volume().cluster_size() - offset);
            self.write_volume_at(start + offset as u64, &zeros[..length])
                .await?;
            offset += length;
        }
        Ok(())
    }

    /// Returns the tail and length of a cluster chain.
    async fn chain_tail_and_length(
        &mut self,
        first: Cluster,
    ) -> Result<(Cluster, u32), Error<Self::Error>> {
        let mut current = first;
        for length in 1..=self.volume().cluster_count {
            let next = self.next_cluster(current).await?;
            let Some(next) = next else {
                return Ok((current, length));
            };
            current = next;
        }
        Err(Error::CorruptChain)
    }

    /// Extends a chain to contain at least `required` clusters.
    async fn ensure_chain_length(
        &mut self,
        first: &mut Option<Cluster>,
        required: u32,
    ) -> Result<(), Error<Self::Error>> {
        if required == 0 {
            return Ok(());
        }
        let (mut tail, mut length) = if let Some(first) = *first {
            self.chain_tail_and_length(first).await?
        } else {
            let allocated = self.allocate_cluster(None).await?;
            *first = Some(allocated);
            (allocated, 1)
        };
        while length < required {
            tail = self.allocate_cluster(Some(tail)).await?;
            length += 1;
        }
        Ok(())
    }

    /// Truncates a chain and returns its retained first cluster.
    async fn truncate_chain(
        &mut self,
        first: Option<Cluster>,
        keep_clusters: u32,
    ) -> Result<Option<Cluster>, Error<Self::Error>> {
        let Some(first) = first else {
            return Ok(None);
        };
        if keep_clusters == 0 {
            self.free_chain(first).await?;
            return Ok(None);
        }
        let last = self
            .cluster_at(first, keep_clusters - 1)
            .await?
            .ok_or(Error::CorruptChain)?;
        let tail = self.next_cluster(last).await?;
        if let Some(tail) = tail {
            self.write_fat_entry(last, FatEntry::EndOfChain).await?;
            self.free_chain(tail).await?;
        }
        Ok(Some(first))
    }

    /// Writes bytes at an offset within an existing cluster chain.
    async fn write_chain_at(
        &mut self,
        first: Cluster,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, Error<Self::Error>> {
        let cluster_size = self.volume().cluster_size() as u64;
        let cluster_index = offset / cluster_size;
        let mut within_cluster = (offset % cluster_size) as usize;
        let cluster = self
            .cluster_at(
                first,
                u32::try_from(cluster_index).map_err(|_| Error::OutOfBounds)?,
            )
            .await?;
        let Some(mut cluster) = cluster else {
            return Ok(0);
        };
        let mut written = 0;
        while written < data.len() {
            let length = cmp::min(
                self.volume().cluster_size() - within_cluster,
                data.len() - written,
            );
            let byte_offset = self
                .volume()
                .cluster_byte_offset(cluster)
                .ok_or(Error::CorruptChain)?
                + within_cluster as u64;
            self.write_volume_at(byte_offset, &data[written..written + length])
                .await?;
            written += length;
            within_cluster = 0;
            if written < data.len() {
                let next = self.next_cluster(cluster).await?;
                let Some(next) = next else {
                    break;
                };
                cluster = next;
            }
        }
        Ok(written)
    }

    /// Releases every cluster in a chain.
    async fn free_chain(&mut self, first: Cluster) -> Result<(), Error<Self::Error>> {
        let mut current = first;
        for _ in 0..self.volume().cluster_count {
            let next = self.next_cluster(current).await?;
            self.write_fat_entry(current, FatEntry::Free).await?;
            let Some(next) = next else {
                return Ok(());
            };
            current = next;
        }
        Err(Error::CorruptChain)
    }
}
