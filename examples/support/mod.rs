use fatsd::{
    AllocationAccess, BasicDirectoryHandle, BasicFileHandle, BlockAccess, BlockWrite, ChainAccess,
    DirectoryAccess, DirectoryWrite, Error, FatFs, FileAccess, FileWrite, Volume, read_volume,
};

pub struct Fs<D> {
    device: D,
    volume: Volume,
}

impl<D: BlockAccess> Fs<D> {
    pub fn mount(mut device: D) -> Result<Self, Error<D::Error>> {
        let volume = read_volume(&mut device)?;
        Ok(Self { device, volume })
    }

    pub fn volume_info(&self) -> &Volume {
        &self.volume
    }
}

impl<D: BlockAccess> BlockAccess for Fs<D> {
    type Error = D::Error;

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

impl<D: BlockWrite> BlockWrite for Fs<D> {
    fn write_block_at(
        &mut self,
        block: u64,
        offset: usize,
        data: &[u8],
    ) -> Result<(), Self::Error> {
        self.device.write_block_at(block, offset, data)
    }
}

impl<D: BlockAccess> ChainAccess for Fs<D> {
    fn volume(&self) -> &Volume {
        &self.volume
    }
}

impl<D: BlockAccess> DirectoryAccess for Fs<D> {
    type DirectoryHandle = BasicDirectoryHandle;
}

impl<D: BlockAccess> FileAccess for Fs<D> {
    type FileHandle = BasicFileHandle;
}

impl<D: BlockWrite> AllocationAccess for Fs<D> {}
impl<D: BlockWrite> DirectoryWrite for Fs<D> {}
impl<D: BlockWrite> FileWrite for Fs<D> {}
impl<D: BlockWrite> FatFs for Fs<D> {}
