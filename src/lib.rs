//! Extensible, allocation-free access to FAT12, FAT16, and FAT32 volumes.
//!
//! Filesystem behavior is provided as trait default methods. Applications own the filesystem
//! type and explicitly implement the capability traits they need.
//!
//! Synchronous access is enabled by the default `sync` feature. The `async` feature adds the
//! corresponding `Async*` traits without requiring an executor or allocation.
//! Futures returned by the async traits are not required to be `Send`.

#![no_std]
#![warn(missing_docs)]
#![allow(async_fn_in_trait)]

pub mod access;
pub mod format;
pub mod handle;
pub mod volume;
pub mod write;

#[cfg(feature = "sync")]
pub use access::{
    AllocationAccess, BlockAccess, BlockWrite, ChainAccess, DirectoryAccess, FileAccess,
    read_volume,
};
#[cfg(feature = "async")]
pub use access::{
    AsyncAllocationAccess, AsyncBlockAccess, AsyncBlockWrite, AsyncChainAccess,
    AsyncDirectoryAccess, AsyncFileAccess, read_volume_async,
};
pub use access::{Error, FoundEntry};
pub use format::FatType;
pub use handle::{
    BasicDirectoryHandle, BasicFileHandle, DirectoryEntryLocation, DirectoryHandle, DirectoryInfo,
    DirectoryLocation, FileHandle, FileInfo, MutableFileHandle,
};
pub use volume::{Cluster, Volume};
pub use write::FreeDirectoryEntries;
#[cfg(feature = "async")]
pub use write::{AsyncDirectoryWrite, AsyncFatFs, AsyncFileWrite};
#[cfg(feature = "sync")]
pub use write::{DirectoryWrite, FatFs, FileWrite};
