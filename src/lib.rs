//! Extensible, allocation-free access to FAT12, FAT16, and FAT32 volumes.
//!
//! Filesystem behavior is provided as trait default methods. Applications own the filesystem
//! type and explicitly implement the capability traits they need.

#![no_std]
#![warn(missing_docs)]

pub mod access;
pub mod format;
pub mod handle;
pub mod volume;
pub mod write;

pub use access::{
    AllocationAccess, BlockAccess, BlockWrite, ChainAccess, DirectoryAccess, Error, FileAccess,
    FoundEntry, read_volume,
};
pub use format::FatType;
pub use handle::{
    BasicDirectoryHandle, BasicFileHandle, DirectoryEntryLocation, DirectoryHandle, DirectoryInfo,
    DirectoryLocation, FileHandle, FileInfo, MutableFileHandle,
};
pub use volume::{Cluster, Volume};
pub use write::{DirectoryWrite, FatFs, FileWrite, FreeDirectoryEntries};
