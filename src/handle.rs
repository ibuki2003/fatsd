//! Handle metadata and minimal handle traits.

use crate::volume::Cluster;

/// Physical representation of a directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectoryLocation {
    /// Fixed root-directory area used by FAT12 and FAT16.
    FixedRoot,
    /// Directory stored in a cluster chain.
    Cluster(Cluster),
}

/// Metadata required to access a directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectoryInfo {
    /// On-disk directory location.
    pub location: DirectoryLocation,
}

/// Location of a directory entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectoryEntryLocation {
    /// Directory containing the entry.
    pub directory: DirectoryLocation,
    /// Zero-based entry index.
    pub index: u32,
}

/// Exposes directory metadata to access traits.
pub trait DirectoryHandle {
    /// Returns the directory metadata.
    fn directory_info(&self) -> &DirectoryInfo;
}

/// Minimal directory handle containing only [`DirectoryInfo`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BasicDirectoryHandle(pub DirectoryInfo);

impl From<DirectoryInfo> for BasicDirectoryHandle {
    fn from(value: DirectoryInfo) -> Self {
        Self(value)
    }
}

impl DirectoryHandle for BasicDirectoryHandle {
    fn directory_info(&self) -> &DirectoryInfo {
        &self.0
    }
}

/// Metadata required to access a file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileInfo {
    /// First data cluster, or `None` for an empty file.
    pub first_cluster: Option<Cluster>,
    /// File length in bytes.
    pub length: u64,
    /// Directory-entry attributes.
    pub attributes: u8,
    /// Location of the file's directory entry.
    pub entry: DirectoryEntryLocation,
}

/// Exposes file metadata to access traits.
pub trait FileHandle {
    /// Returns the file metadata.
    fn file_info(&self) -> &FileInfo;
}

/// Exposes mutable file metadata to write traits.
pub trait MutableFileHandle: FileHandle {
    /// Returns mutable file metadata.
    fn file_info_mut(&mut self) -> &mut FileInfo;
}

/// Minimal file handle containing only [`FileInfo`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BasicFileHandle(pub FileInfo);

impl From<FileInfo> for BasicFileHandle {
    fn from(value: FileInfo) -> Self {
        Self(value)
    }
}

impl FileHandle for BasicFileHandle {
    fn file_info(&self) -> &FileInfo {
        &self.0
    }
}

impl MutableFileHandle for BasicFileHandle {
    fn file_info_mut(&mut self) -> &mut FileInfo {
        &mut self.0
    }
}
