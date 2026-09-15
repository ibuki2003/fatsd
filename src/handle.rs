use crate::volume::Cluster;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectoryLocation {
    FixedRoot,
    Cluster(Cluster),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectoryInfo {
    pub location: DirectoryLocation,
}

pub trait DirectoryHandle {
    fn directory_info(&self) -> &DirectoryInfo;
}

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileInfo {
    pub first_cluster: Option<Cluster>,
    pub length: u64,
}

pub trait FileHandle {
    fn file_info(&self) -> &FileInfo;
}

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
