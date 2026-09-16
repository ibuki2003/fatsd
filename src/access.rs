use core::cmp;

use crate::{
    format::{Bpb, DIRECTORY_ENTRY_SIZE, FatEntry, FatType, FormatError, LfnEntry, RawDirEntry},
    handle::{
        DirectoryEntryLocation, DirectoryHandle, DirectoryInfo, DirectoryLocation, FileHandle,
        FileInfo,
    },
    volume::{Cluster, Volume},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error<E> {
    Io(E),
    InvalidFormat(FormatError),
    CorruptChain,
    NotFound,
    NotAFile,
    NotADirectory,
    InvalidPath,
    NoSpace,
    AlreadyExists,
    DirectoryFull,
    NameTooLong,
    InvalidName,
    NotEmpty,
    ReadOnly,
    OutOfBounds,
}

impl<E> From<FormatError> for Error<E> {
    fn from(value: FormatError) -> Self {
        Self::InvalidFormat(value)
    }
}

pub trait BlockAccess {
    type Error;

    fn block_size(&self) -> usize;

    /// Reads a range contained in one physical block.
    ///
    /// Implementations may assume `offset + out.len() <= self.block_size()`.
    fn read_block_at(
        &mut self,
        block: u64,
        offset: usize,
        out: &mut [u8],
    ) -> Result<(), Self::Error>;
}

pub trait BlockWrite: BlockAccess {
    /// Writes a range contained in one physical block.
    ///
    /// Implementations may assume `offset + data.len() <= self.block_size()`.
    fn write_block_at(&mut self, block: u64, offset: usize, data: &[u8])
    -> Result<(), Self::Error>;
}

impl<T: BlockAccess + ?Sized> BlockAccess for &mut T {
    type Error = T::Error;

    fn block_size(&self) -> usize {
        (**self).block_size()
    }

    fn read_block_at(
        &mut self,
        block: u64,
        offset: usize,
        out: &mut [u8],
    ) -> Result<(), Self::Error> {
        (**self).read_block_at(block, offset, out)
    }
}

impl<T: BlockWrite + ?Sized> BlockWrite for &mut T {
    fn write_block_at(
        &mut self,
        block: u64,
        offset: usize,
        data: &[u8],
    ) -> Result<(), Self::Error> {
        (**self).write_block_at(block, offset, data)
    }
}

fn read_device_at<T: BlockAccess + ?Sized>(
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
            .map_err(Error::Io)?;
        byte_offset = byte_offset
            .checked_add(length as u64)
            .ok_or(Error::OutOfBounds)?;
        out = rest;
    }
    Ok(())
}

fn write_device_at<T: BlockWrite + ?Sized>(
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
            .map_err(Error::Io)?;
        byte_offset = byte_offset
            .checked_add(length as u64)
            .ok_or(Error::OutOfBounds)?;
        data = rest;
    }
    Ok(())
}

pub fn read_volume<T: BlockAccess + ?Sized>(device: &mut T) -> Result<Volume, Error<T::Error>> {
    let mut boot_sector = [0; 512];
    read_device_at(device, 0, &mut boot_sector)?;
    Ok(Volume::from_bpb(Bpb::parse(&boot_sector)?)?)
}

pub trait ChainAccess: BlockAccess {
    fn volume(&self) -> &Volume;

    fn read_volume_at(
        &mut self,
        byte_offset: u64,
        out: &mut [u8],
    ) -> Result<(), Error<Self::Error>> {
        read_device_at(self, byte_offset, out)
    }

    fn read_fat_entry(&mut self, cluster: Cluster) -> Result<FatEntry, Error<Self::Error>> {
        validate_cluster(self.volume(), cluster)?;
        let volume = *self.volume();
        let fat_start_sector =
            volume.fat_start_sector + volume.active_fat() as u32 * volume.fat_sectors();
        let fat_start = volume.sector_byte_offset(fat_start_sector);
        match volume.fat_type {
            FatType::Fat12 => {
                let mut bytes = [0; 2];
                let index = cluster.get() as u64;
                self.read_volume_at(fat_start + index + index / 2, &mut bytes)?;
                Ok(FatEntry::parse(FatType::Fat12, cluster.get(), &bytes)?)
            }
            FatType::Fat16 => {
                let mut bytes = [0; 2];
                self.read_volume_at(fat_start + cluster.get() as u64 * 2, &mut bytes)?;
                Ok(FatEntry::parse(FatType::Fat16, cluster.get(), &bytes)?)
            }
            FatType::Fat32 => {
                let mut bytes = [0; 4];
                self.read_volume_at(fat_start + cluster.get() as u64 * 4, &mut bytes)?;
                Ok(FatEntry::parse(FatType::Fat32, cluster.get(), &bytes)?)
            }
        }
    }

    fn next_cluster(&mut self, cluster: Cluster) -> Result<Option<Cluster>, Error<Self::Error>> {
        match self.read_fat_entry(cluster)? {
            FatEntry::Data(next) => {
                let next = Cluster::new(next).ok_or(Error::CorruptChain)?;
                validate_cluster(self.volume(), next)?;
                Ok(Some(next))
            }
            FatEntry::EndOfChain => Ok(None),
            FatEntry::Free | FatEntry::Bad | FatEntry::Reserved(_) => Err(Error::CorruptChain),
        }
    }

    /// Resolves a cluster by walking a chain. Override this to use a shared chain index.
    fn cluster_at(
        &mut self,
        first: Cluster,
        index: u32,
    ) -> Result<Option<Cluster>, Error<Self::Error>> {
        validate_cluster(self.volume(), first)?;
        let mut cluster = first;
        for _ in 0..index {
            let Some(next) = self.next_cluster(cluster)? else {
                return Ok(None);
            };
            cluster = next;
        }
        Ok(Some(cluster))
    }

    fn read_chain_at(
        &mut self,
        first: Cluster,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize, Error<Self::Error>> {
        let cluster_size = self.volume().cluster_size() as u64;
        let cluster_index = offset / cluster_size;
        let mut within_cluster = (offset % cluster_size) as usize;
        let Some(mut cluster) = self.cluster_at(
            first,
            u32::try_from(cluster_index).map_err(|_| Error::OutOfBounds)?,
        )?
        else {
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
            self.read_volume_at(byte_offset, &mut out[read..read + length])?;
            read += length;
            within_cluster = 0;
            if read < out.len() {
                let Some(next) = self.next_cluster(cluster)? else {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FoundEntry {
    pub raw: RawDirEntry,
    pub location: DirectoryEntryLocation,
    pub lfn_start_index: Option<u32>,
}

pub trait DirectoryAccess: ChainAccess {
    type DirectoryHandle: DirectoryHandle + From<DirectoryInfo>;

    fn root_directory(&self) -> Self::DirectoryHandle {
        DirectoryInfo {
            location: self.volume().root_directory(),
        }
        .into()
    }

    fn read_directory_at(
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
                self.read_volume_at(start, &mut out[..read])?;
                Ok(read)
            }
            DirectoryLocation::Cluster(first) => self.read_chain_at(first, offset, out),
        }
    }

    fn read_directory_entry(
        &mut self,
        directory: &Self::DirectoryHandle,
        index: u32,
    ) -> Result<Option<RawDirEntry>, Error<Self::Error>> {
        let mut bytes = [0; DIRECTORY_ENTRY_SIZE];
        let offset = index as u64 * DIRECTORY_ENTRY_SIZE as u64;
        if self.read_directory_at(directory, offset, &mut bytes)? != bytes.len() {
            return Ok(None);
        }
        let entry = RawDirEntry(bytes);
        Ok((!entry.is_end()).then_some(entry))
    }

    fn find_entry(
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
        let mut expected_ordinal = 0u8;
        let mut checksum = 0u8;
        let mut lfn_active = false;
        let mut lfn_start_index = None;

        loop {
            let read = self.read_directory_at(directory, byte_offset, &mut batch)?;
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
                    lfn_active = false;
                    lfn_start_index = None;
                    continue;
                }
                if raw.is_lfn() {
                    let Ok(lfn) = LfnEntry::parse(raw) else {
                        lfn_active = false;
                        continue;
                    };
                    if lfn.is_last {
                        long_name.fill(0xffff);
                        expected_ordinal = lfn.ordinal;
                        checksum = lfn.checksum;
                        lfn_active = true;
                        lfn_start_index = Some(entry_index);
                    }
                    if !lfn_active || lfn.ordinal != expected_ordinal || lfn.checksum != checksum {
                        lfn_active = false;
                        lfn_start_index = None;
                        continue;
                    }
                    let start = (lfn.ordinal as usize - 1) * 13;
                    long_name[start..start + 13].copy_from_slice(&lfn.characters);
                    expected_ordinal -= 1;
                    continue;
                }

                let valid_lfn =
                    lfn_active && expected_ordinal == 0 && raw.short_name().checksum() == checksum;
                let lfn_matches = valid_lfn && long_name_matches(&long_name, name);
                let short_matches = raw.short_name().matches(name);
                lfn_active = false;
                if !raw.is_volume_label() && (lfn_matches || short_matches) {
                    return Ok(FoundEntry {
                        raw,
                        location: DirectoryEntryLocation {
                            directory: directory.directory_info().location,
                            index: entry_index,
                        },
                        lfn_start_index: valid_lfn.then_some(lfn_start_index).flatten(),
                    });
                }
                lfn_start_index = None;
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

    fn find_entry_by_path(&mut self, path: &str) -> Result<FoundEntry, Error<Self::Error>> {
        let mut components = path
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".");
        let first = components.next().ok_or(Error::InvalidPath)?;
        let mut directory = self.root_directory();
        let mut entry = self.find_entry(&directory, first)?;
        for component in components {
            if !entry.raw.is_directory() {
                return Err(Error::NotADirectory);
            }
            directory = DirectoryInfo {
                location: directory_location(self.volume(), &entry.raw)?,
            }
            .into();
            entry = self.find_entry(&directory, component)?;
        }
        Ok(entry)
    }

    fn open_directory(&mut self, path: &str) -> Result<Self::DirectoryHandle, Error<Self::Error>> {
        if path.split('/').all(|part| part.is_empty() || part == ".") {
            return Ok(self.root_directory());
        }
        let entry = self.find_entry_by_path(path)?;
        if !entry.raw.is_directory() {
            return Err(Error::NotADirectory);
        }
        Ok(DirectoryInfo {
            location: directory_location(self.volume(), &entry.raw)?,
        }
        .into())
    }
}

fn long_name_matches(buffer: &[u16; 260], name: &str) -> bool {
    let length = buffer
        .iter()
        .position(|character| matches!(*character, 0 | 0xffff))
        .unwrap_or(buffer.len());
    name.encode_utf16().eq(buffer[..length].iter().copied())
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

pub trait FileAccess: DirectoryAccess {
    type FileHandle: FileHandle + From<FileInfo>;

    fn open_file(&mut self, path: &str) -> Result<Self::FileHandle, Error<Self::Error>> {
        let entry = self.find_entry_by_path(path)?;
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

    /// Resolves a file cluster. Override this method for per-handle CLMT lookup.
    fn resolve_file_cluster(
        &mut self,
        file: &mut Self::FileHandle,
        index: u32,
    ) -> Result<Option<Cluster>, Error<Self::Error>> {
        let Some(first) = file.file_info().first_cluster else {
            return Ok(None);
        };
        self.cluster_at(first, index)
    }

    /// Resolves the cluster following `current` during a sequential read.
    /// Override this together with `resolve_file_cluster` to keep all lookups in a CLMT.
    fn resolve_next_file_cluster(
        &mut self,
        _file: &mut Self::FileHandle,
        current: Cluster,
        _next_index: u32,
    ) -> Result<Option<Cluster>, Error<Self::Error>> {
        self.next_cluster(current)
    }

    fn read_file_at(
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
            .resolve_file_cluster(file, cluster_index)?
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
            self.read_volume_at(byte_offset, &mut out[read..read + length])?;
            read += length;
            within_cluster = 0;
            if read < requested {
                cluster_index = cluster_index.checked_add(1).ok_or(Error::OutOfBounds)?;
                cluster = self
                    .resolve_next_file_cluster(file, cluster, cluster_index)?
                    .ok_or(Error::CorruptChain)?;
            }
        }
        Ok(read)
    }
}

pub trait AllocationAccess: ChainAccess + BlockWrite {
    fn write_volume_at(&mut self, byte_offset: u64, data: &[u8]) -> Result<(), Error<Self::Error>> {
        write_device_at(self, byte_offset, data)
    }

    fn write_fat_entry(
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
            self.write_one_fat_entry(start, cluster, value)?;
        }
        self.invalidate_fs_info()?;
        Ok(())
    }

    fn invalidate_fs_info(&mut self) -> Result<(), Error<Self::Error>> {
        let volume = *self.volume();
        let sector = volume.bpb.fs_info_sector;
        if volume.fat_type == FatType::Fat32
            && sector != 0
            && sector != u16::MAX
            && sector < volume.bpb.reserved_sector_count
        {
            let offset = volume.sector_byte_offset(sector as u32) + 488;
            self.write_volume_at(offset, &[0xff; 8])?;
        }
        Ok(())
    }

    fn write_one_fat_entry(
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
                self.read_volume_at(offset, &mut bytes)?;
                value.serialize_into(fat_type, cluster.get(), &mut bytes)?;
                self.write_volume_at(offset, &bytes)
            }
            FatType::Fat16 => {
                let mut bytes = [0; 2];
                value.serialize_into(fat_type, cluster.get(), &mut bytes)?;
                self.write_volume_at(fat_start + cluster.get() as u64 * 2, &bytes)
            }
            FatType::Fat32 => {
                let offset = fat_start + cluster.get() as u64 * 4;
                let mut bytes = [0; 4];
                self.read_volume_at(offset, &mut bytes)?;
                value.serialize_into(fat_type, cluster.get(), &mut bytes)?;
                self.write_volume_at(offset, &bytes)
            }
        }
    }

    fn find_free_cluster(&mut self, start: Option<Cluster>) -> Result<Cluster, Error<Self::Error>> {
        let max = self.volume().max_cluster();
        let start = start.map_or(2, Cluster::get);
        if start > max {
            return Err(Error::OutOfBounds);
        }
        for raw in (start..=max).chain(2..start) {
            let cluster = Cluster::new(raw).expect("range starts at two");
            if self.read_fat_entry(cluster)? == FatEntry::Free {
                return Ok(cluster);
            }
        }
        Err(Error::NoSpace)
    }

    fn allocate_cluster(&mut self, after: Option<Cluster>) -> Result<Cluster, Error<Self::Error>> {
        if let Some(previous) = after
            && self.read_fat_entry(previous)? != FatEntry::EndOfChain
        {
            return Err(Error::CorruptChain);
        }
        let cluster = self.find_free_cluster(after)?;
        self.write_fat_entry(cluster, FatEntry::EndOfChain)?;
        self.clear_cluster(cluster)?;
        if let Some(previous) = after {
            self.write_fat_entry(previous, FatEntry::Data(cluster.get()))?;
        }
        Ok(cluster)
    }

    fn clear_cluster(&mut self, cluster: Cluster) -> Result<(), Error<Self::Error>> {
        let start = self
            .volume()
            .cluster_byte_offset(cluster)
            .ok_or(Error::CorruptChain)?;
        let mut offset = 0;
        let zeros = [0; 512];
        while offset < self.volume().cluster_size() {
            let length = cmp::min(zeros.len(), self.volume().cluster_size() - offset);
            self.write_volume_at(start + offset as u64, &zeros[..length])?;
            offset += length;
        }
        Ok(())
    }

    fn chain_tail_and_length(
        &mut self,
        first: Cluster,
    ) -> Result<(Cluster, u32), Error<Self::Error>> {
        let mut current = first;
        for length in 1..=self.volume().cluster_count {
            let Some(next) = self.next_cluster(current)? else {
                return Ok((current, length));
            };
            current = next;
        }
        Err(Error::CorruptChain)
    }

    fn ensure_chain_length(
        &mut self,
        first: &mut Option<Cluster>,
        required: u32,
    ) -> Result<(), Error<Self::Error>> {
        if required == 0 {
            return Ok(());
        }
        let (mut tail, mut length) = if let Some(first) = *first {
            self.chain_tail_and_length(first)?
        } else {
            let allocated = self.allocate_cluster(None)?;
            *first = Some(allocated);
            (allocated, 1)
        };
        while length < required {
            tail = self.allocate_cluster(Some(tail))?;
            length += 1;
        }
        Ok(())
    }

    fn truncate_chain(
        &mut self,
        first: Option<Cluster>,
        keep_clusters: u32,
    ) -> Result<Option<Cluster>, Error<Self::Error>> {
        let Some(first) = first else {
            return Ok(None);
        };
        if keep_clusters == 0 {
            self.free_chain(first)?;
            return Ok(None);
        }
        let last = self
            .cluster_at(first, keep_clusters - 1)?
            .ok_or(Error::CorruptChain)?;
        let tail = self.next_cluster(last)?;
        if let Some(tail) = tail {
            self.write_fat_entry(last, FatEntry::EndOfChain)?;
            self.free_chain(tail)?;
        }
        Ok(Some(first))
    }

    fn write_chain_at(
        &mut self,
        first: Cluster,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, Error<Self::Error>> {
        let cluster_size = self.volume().cluster_size() as u64;
        let cluster_index = offset / cluster_size;
        let mut within_cluster = (offset % cluster_size) as usize;
        let Some(mut cluster) = self.cluster_at(
            first,
            u32::try_from(cluster_index).map_err(|_| Error::OutOfBounds)?,
        )?
        else {
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
            self.write_volume_at(byte_offset, &data[written..written + length])?;
            written += length;
            within_cluster = 0;
            if written < data.len() {
                let Some(next) = self.next_cluster(cluster)? else {
                    break;
                };
                cluster = next;
            }
        }
        Ok(written)
    }

    fn free_chain(&mut self, first: Cluster) -> Result<(), Error<Self::Error>> {
        let mut current = first;
        for _ in 0..self.volume().cluster_count {
            let next = self.next_cluster(current)?;
            self.write_fat_entry(current, FatEntry::Free)?;
            let Some(next) = next else {
                return Ok(());
            };
            current = next;
        }
        Err(Error::CorruptChain)
    }
}
