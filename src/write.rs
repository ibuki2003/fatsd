use core::cmp;

use crate::{
    access::{AllocationAccess, BlockWrite, DirectoryAccess, Error, FileAccess, FileSystem},
    format::{DIRECTORY_ENTRY_SIZE, LfnEntry, RawDirEntry, ShortName},
    handle::{
        DirectoryEntryLocation, DirectoryHandle, DirectoryInfo, DirectoryLocation, FileHandle,
        FileInfo, MutableFileHandle,
    },
    volume::Cluster,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FreeDirectoryEntries {
    pub start_index: u32,
    pub reached_end: bool,
}

pub trait DirectoryWrite: DirectoryAccess + AllocationAccess {
    fn write_directory_at(
        &mut self,
        directory: DirectoryLocation,
        offset: u64,
        data: &[u8],
    ) -> Result<(), Error<Self::Error>> {
        if data.is_empty() {
            return Ok(());
        }
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(Error::OutOfBounds)?;
        match directory {
            DirectoryLocation::FixedRoot => {
                let volume = *self.volume();
                let capacity = volume.root_directory_sectors as u64 * volume.sector_size() as u64;
                if end > capacity {
                    return Err(Error::DirectoryFull);
                }
                let start = volume.sector_byte_offset(volume.root_directory_start_sector) + offset;
                self.write_volume_at(start, data)
            }
            DirectoryLocation::Cluster(first) => {
                let cluster_size = self.volume().cluster_size() as u64;
                let required =
                    u32::try_from(end.div_ceil(cluster_size)).map_err(|_| Error::OutOfBounds)?;
                let mut chain = Some(first);
                self.ensure_chain_length(&mut chain, required)?;
                if self.write_chain_at(first, offset, data)? != data.len() {
                    return Err(Error::CorruptChain);
                }
                Ok(())
            }
        }
    }

    fn write_directory_entry(
        &mut self,
        location: DirectoryEntryLocation,
        entry: RawDirEntry,
    ) -> Result<(), Error<Self::Error>> {
        self.write_directory_at(
            location.directory,
            location.index as u64 * DIRECTORY_ENTRY_SIZE as u64,
            &entry.serialize(),
        )
    }

    fn read_directory_entry_at(
        &mut self,
        location: DirectoryEntryLocation,
    ) -> Result<RawDirEntry, Error<Self::Error>> {
        let directory = DirectoryInfo {
            location: location.directory,
        }
        .into();
        self.read_directory_entry(&directory, location.index)?
            .ok_or(Error::NotFound)
    }

    fn find_free_directory_entries(
        &mut self,
        directory: &Self::DirectoryHandle,
        count: u32,
    ) -> Result<FreeDirectoryEntries, Error<Self::Error>> {
        if count == 0 {
            return Err(Error::OutOfBounds);
        }
        let mut index = 0u32;
        let mut run_start = 0u32;
        let mut run_length = 0u32;
        loop {
            let mut bytes = [0; DIRECTORY_ENTRY_SIZE];
            let read = self.read_directory_at(
                directory,
                index as u64 * DIRECTORY_ENTRY_SIZE as u64,
                &mut bytes,
            )?;
            let reached_end = read == 0 || bytes[0] == 0;
            if reached_end {
                let start_index = if run_length == 0 { index } else { run_start };
                self.check_directory_capacity(directory, start_index, count)?;
                return Ok(FreeDirectoryEntries {
                    start_index,
                    reached_end: true,
                });
            }
            if bytes[0] == 0xe5 {
                if run_length == 0 {
                    run_start = index;
                }
                run_length += 1;
                if run_length == count {
                    return Ok(FreeDirectoryEntries {
                        start_index: run_start,
                        reached_end: false,
                    });
                }
            } else {
                run_length = 0;
            }
            index = index.checked_add(1).ok_or(Error::DirectoryFull)?;
        }
    }

    fn check_directory_capacity(
        &self,
        directory: &Self::DirectoryHandle,
        start_index: u32,
        count: u32,
    ) -> Result<(), Error<Self::Error>> {
        if directory.directory_info().location == DirectoryLocation::FixedRoot
            && start_index
                .checked_add(count)
                .is_none_or(|end| end > self.volume().bpb.root_entry_count as u32)
        {
            return Err(Error::DirectoryFull);
        }
        Ok(())
    }

    fn short_name_exists(
        &mut self,
        directory: &Self::DirectoryHandle,
        name: ShortName,
    ) -> Result<bool, Error<Self::Error>> {
        let mut index = 0u32;
        loop {
            let Some(entry) = self.read_directory_entry(directory, index)? else {
                return Ok(false);
            };
            if !entry.is_deleted() && !entry.is_lfn() && entry.short_name() == name {
                return Ok(true);
            }
            index = index.checked_add(1).ok_or(Error::DirectoryFull)?;
        }
    }

    fn create_file(&mut self, path: &str) -> Result<Self::FileHandle, Error<Self::Error>>
    where
        Self: FileAccess,
    {
        let (directory, name) = self.open_parent_directory(path)?;
        match self.find_entry(&directory, name) {
            Ok(_) => return Err(Error::AlreadyExists),
            Err(Error::NotFound) => {}
            Err(error) => return Err(error),
        }
        let (short_name, needs_lfn) = self.select_short_name(&directory, name)?;
        let mut utf16 = [0; 255];
        let utf16_length = encode_name(name, &mut utf16)?;
        let lfn_count = if needs_lfn {
            utf16_length.div_ceil(13)
        } else {
            0
        };
        let slots = self.find_free_directory_entries(&directory, (lfn_count + 1) as u32)?;
        let checksum = short_name.checksum();
        for disk_index in 0..lfn_count {
            let ordinal = lfn_count - disk_index;
            let start = (ordinal - 1) * 13;
            let mut characters = [0xffff; 13];
            let available = cmp::min(13, utf16_length - start);
            characters[..available].copy_from_slice(&utf16[start..start + available]);
            if available < 13 {
                characters[available] = 0;
            }
            let lfn = LfnEntry {
                ordinal: ordinal as u8,
                is_last: ordinal == lfn_count,
                checksum,
                characters,
            }
            .serialize()?;
            self.write_directory_entry(
                DirectoryEntryLocation {
                    directory: directory.directory_info().location,
                    index: slots.start_index + disk_index as u32,
                },
                lfn,
            )?;
        }
        let short_location = DirectoryEntryLocation {
            directory: directory.directory_info().location,
            index: slots.start_index + lfn_count as u32,
        };
        self.write_directory_entry(short_location, RawDirEntry::new(short_name, 0x20))?;
        if slots.reached_end {
            let next = short_location.index + 1;
            if self.check_directory_capacity(&directory, next, 1).is_ok() {
                self.write_directory_entry(
                    DirectoryEntryLocation {
                        directory: short_location.directory,
                        index: next,
                    },
                    RawDirEntry([0; DIRECTORY_ENTRY_SIZE]),
                )?;
            }
        }
        Ok(FileInfo {
            first_cluster: None,
            length: 0,
            entry: short_location,
        }
        .into())
    }

    fn open_parent_directory<'a>(
        &mut self,
        path: &'a str,
    ) -> Result<(Self::DirectoryHandle, &'a str), Error<Self::Error>> {
        let path = path.trim_start_matches('/');
        if path.is_empty() || path.ends_with('/') {
            return Err(Error::InvalidPath);
        }
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        validate_name(name)?;
        let directory = if parent.is_empty() {
            self.root_directory()
        } else {
            self.open_directory(parent)?
        };
        Ok((directory, name))
    }

    fn select_short_name(
        &mut self,
        directory: &Self::DirectoryHandle,
        name: &str,
    ) -> Result<(ShortName, bool), Error<Self::Error>> {
        if let Some(short) = canonical_short_name(name)
            && !self.short_name_exists(directory, short)?
        {
            return Ok((short, false));
        }
        for ordinal in 1..=999_999 {
            let alias = short_alias(name, ordinal);
            if !self.short_name_exists(directory, alias)? {
                return Ok((alias, true));
            }
        }
        Err(Error::DirectoryFull)
    }
}

impl<D: BlockWrite> DirectoryWrite for FileSystem<D> {}

pub trait FileWrite: FileAccess + DirectoryWrite
where
    Self::FileHandle: MutableFileHandle,
{
    fn file_chain_changed(&mut self, _file: &mut Self::FileHandle) {}

    fn persist_file_info(&mut self, file: &Self::FileHandle) -> Result<(), Error<Self::Error>> {
        let info = *file.file_info();
        let mut entry = self.read_directory_entry_at(info.entry)?;
        entry.set_first_cluster(self.volume().fat_type, info.first_cluster.map(Cluster::get));
        entry.set_file_size(u32::try_from(info.length).map_err(|_| Error::OutOfBounds)?);
        self.write_directory_entry(info.entry, entry)
    }

    fn write_file_data_at(
        &mut self,
        file: &mut Self::FileHandle,
        offset: u64,
        data: &[u8],
    ) -> Result<(), Error<Self::Error>> {
        if data.is_empty() {
            return Ok(());
        }
        let cluster_size = self.volume().cluster_size() as u64;
        let mut cluster_index =
            u32::try_from(offset / cluster_size).map_err(|_| Error::OutOfBounds)?;
        let mut within_cluster = (offset % cluster_size) as usize;
        let mut cluster = self
            .resolve_file_cluster(file, cluster_index)?
            .ok_or(Error::CorruptChain)?;
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
                cluster_index = cluster_index.checked_add(1).ok_or(Error::OutOfBounds)?;
                cluster = self
                    .resolve_next_file_cluster(file, cluster, cluster_index)?
                    .ok_or(Error::CorruptChain)?;
            }
        }
        Ok(())
    }

    fn write_zeros(
        &mut self,
        file: &mut Self::FileHandle,
        mut offset: u64,
        mut length: u64,
    ) -> Result<(), Error<Self::Error>> {
        let zeros = [0; 512];
        while length != 0 {
            let part = cmp::min(length, zeros.len() as u64) as usize;
            self.write_file_data_at(file, offset, &zeros[..part])?;
            offset += part as u64;
            length -= part as u64;
        }
        Ok(())
    }

    fn write_file_at(
        &mut self,
        file: &mut Self::FileHandle,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, Error<Self::Error>> {
        if data.is_empty() {
            return Ok(0);
        }
        let old = *file.file_info();
        let end = offset
            .checked_add(data.len() as u64)
            .filter(|end| *end <= u32::MAX as u64)
            .ok_or(Error::OutOfBounds)?;
        let required = u32::try_from(end.div_ceil(self.volume().cluster_size() as u64))
            .map_err(|_| Error::OutOfBounds)?;
        let mut first = old.first_cluster;
        self.ensure_chain_length(&mut first, required)?;
        if first != old.first_cluster {
            file.file_info_mut().first_cluster = first;
            self.persist_file_info(file)?;
        }
        if end > old.length {
            self.file_chain_changed(file);
        }
        if offset > old.length {
            self.write_zeros(file, old.length, offset - old.length)?;
        }
        self.write_file_data_at(file, offset, data)?;
        if end > old.length {
            file.file_info_mut().length = end;
            self.persist_file_info(file)?;
        }
        Ok(data.len())
    }

    fn truncate_file(
        &mut self,
        file: &mut Self::FileHandle,
        new_length: u64,
    ) -> Result<(), Error<Self::Error>> {
        if new_length > u32::MAX as u64 {
            return Err(Error::OutOfBounds);
        }
        let old = *file.file_info();
        if new_length == old.length {
            return Ok(());
        }
        let cluster_size = self.volume().cluster_size() as u64;
        let required =
            u32::try_from(new_length.div_ceil(cluster_size)).map_err(|_| Error::OutOfBounds)?;
        if new_length > old.length {
            let mut first = old.first_cluster;
            self.ensure_chain_length(&mut first, required)?;
            file.file_info_mut().first_cluster = first;
            if first != old.first_cluster {
                self.persist_file_info(file)?;
            }
            self.file_chain_changed(file);
            self.write_zeros(file, old.length, new_length - old.length)?;
            file.file_info_mut().length = new_length;
            return self.persist_file_info(file);
        }

        file.file_info_mut().length = new_length;
        if required == 0 {
            file.file_info_mut().first_cluster = None;
        }
        self.persist_file_info(file)?;
        self.truncate_chain(old.first_cluster, required)?;
        self.file_chain_changed(file);
        Ok(())
    }
}

impl<D: BlockWrite> FileWrite for FileSystem<D> {}

fn validate_name<E>(name: &str) -> Result<(), Error<E>> {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.ends_with([' ', '.'])
        || name.chars().any(|character| {
            character <= '\u{1f}'
                || matches!(
                    character,
                    '"' | '*' | '/' | ':' | '<' | '>' | '?' | '\\' | '|'
                )
        })
    {
        return Err(Error::InvalidName);
    }
    if name.encode_utf16().count() > 255 {
        return Err(Error::NameTooLong);
    }
    Ok(())
}

fn encode_name<E>(name: &str, target: &mut [u16; 255]) -> Result<usize, Error<E>> {
    let mut length = 0;
    for character in name.encode_utf16() {
        let slot = target.get_mut(length).ok_or(Error::NameTooLong)?;
        *slot = character;
        length += 1;
    }
    Ok(length)
}

fn canonical_short_name(name: &str) -> Option<ShortName> {
    let mut split = name.split('.');
    let base = split.next()?;
    let extension = split.next().unwrap_or("");
    if split.next().is_some()
        || base.is_empty()
        || base.len() > 8
        || extension.len() > 3
        || !base
            .bytes()
            .chain(extension.bytes())
            .all(is_short_character)
        || name.bytes().any(|byte| byte.is_ascii_lowercase())
    {
        return None;
    }
    let mut raw = [b' '; 11];
    raw[..base.len()].copy_from_slice(base.as_bytes());
    raw[8..8 + extension.len()].copy_from_slice(extension.as_bytes());
    Some(ShortName(raw))
}

fn short_alias(name: &str, ordinal: u32) -> ShortName {
    let (base, extension) = name.rsplit_once('.').unwrap_or((name, ""));
    let mut digits = [0; 6];
    let mut value = ordinal;
    let mut digit_start = digits.len();
    while value != 0 {
        digit_start -= 1;
        digits[digit_start] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    let suffix_length = 1 + digits.len() - digit_start;
    let prefix_length = 8 - suffix_length;
    let mut raw = [b' '; 11];
    let mut written = 0;
    for character in base.chars() {
        if written == prefix_length {
            break;
        }
        raw[written] = sanitized_short_character(character);
        written += 1;
    }
    if written == 0 {
        raw[0] = b'_';
    }
    raw[prefix_length] = b'~';
    raw[prefix_length + 1..8].copy_from_slice(&digits[digit_start..]);
    for (index, character) in extension.chars().take(3).enumerate() {
        raw[8 + index] = sanitized_short_character(character);
    }
    ShortName(raw)
}

fn sanitized_short_character(character: char) -> u8 {
    if character.is_ascii() && is_short_character(character as u8) {
        (character as u8).to_ascii_uppercase()
    } else {
        b'_'
    }
}

fn is_short_character(byte: u8) -> bool {
    byte.is_ascii_uppercase()
        || byte.is_ascii_digit()
        || matches!(
            byte,
            b'$' | b'%'
                | b'\''
                | b'-'
                | b'_'
                | b'@'
                | b'~'
                | b'`'
                | b'!'
                | b'('
                | b')'
                | b'{'
                | b'}'
                | b'^'
                | b'#'
                | b'&'
        )
}
