#![cfg(feature = "async")]

use fatsd::{
    AsyncAllocationAccess, AsyncBlockAccess, AsyncBlockWrite, AsyncChainAccess,
    AsyncDirectoryAccess, AsyncDirectoryWrite, AsyncFatFs, AsyncFileAccess, AsyncFileWrite,
    BasicDirectoryHandle, BasicFileHandle,
    format::Bpb,
    volume::{FatType, Volume},
};

#[derive(Debug, Eq, PartialEq)]
enum MemoryError {
    OutOfBounds,
}

struct MemoryDevice {
    bytes: Vec<u8>,
    block_size: usize,
}

impl AsyncBlockAccess for MemoryDevice {
    type Error = MemoryError;

    fn block_size(&self) -> usize {
        self.block_size
    }

    async fn read_block_at(
        &mut self,
        block: u64,
        offset: usize,
        out: &mut [u8],
    ) -> Result<(), Self::Error> {
        yield_once().await;
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

impl AsyncBlockWrite for MemoryDevice {
    async fn write_block_at(
        &mut self,
        block: u64,
        offset: usize,
        data: &[u8],
    ) -> Result<(), Self::Error> {
        yield_once().await;
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

impl AsyncBlockAccess for TestFs {
    type Error = MemoryError;

    fn block_size(&self) -> usize {
        self.device.block_size()
    }

    async fn read_block_at(
        &mut self,
        block: u64,
        offset: usize,
        out: &mut [u8],
    ) -> Result<(), Self::Error> {
        self.device.read_block_at(block, offset, out).await
    }
}

impl AsyncBlockWrite for TestFs {
    async fn write_block_at(
        &mut self,
        block: u64,
        offset: usize,
        data: &[u8],
    ) -> Result<(), Self::Error> {
        self.device.write_block_at(block, offset, data).await
    }
}

impl AsyncChainAccess for TestFs {
    fn volume(&self) -> &Volume {
        &self.volume
    }
}

impl AsyncDirectoryAccess for TestFs {
    type DirectoryHandle = BasicDirectoryHandle;
}

impl AsyncFileAccess for TestFs {
    type FileHandle = BasicFileHandle;
}

impl AsyncAllocationAccess for TestFs {}
impl AsyncDirectoryWrite for TestFs {}
impl AsyncFileWrite for TestFs {}
impl AsyncFatFs for TestFs {}

struct YieldOnce(bool);

impl core::future::Future for YieldOnce {
    type Output = ();

    fn poll(
        mut self: core::pin::Pin<&mut Self>,
        context: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        if self.0 {
            core::task::Poll::Ready(())
        } else {
            self.0 = true;
            context.waker().wake_by_ref();
            core::task::Poll::Pending
        }
    }
}

fn yield_once() -> YieldOnce {
    YieldOnce(false)
}

fn block_on<F: core::future::Future>(future: F) -> F::Output {
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    let mut future = std::pin::pin!(future);
    loop {
        if let std::task::Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
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

#[test]
fn mounts_writes_and_reads_after_pending_io() {
    block_on(async {
        let mut device = MemoryDevice {
            bytes: vec![0; 12 * 512],
            block_size: 128,
        };
        bpb().serialize_into(&mut device.bytes).unwrap();
        let volume = fatsd::read_volume_async(&mut device).await.unwrap();
        assert_eq!(volume.fat_type, FatType::Fat12);

        let mut fs = TestFs { device, volume };
        let mut file = fs.create_file("/ASYNC.BIN").await.unwrap();
        assert_eq!(fs.write_file_at(&mut file, 100, b"async").await.unwrap(), 5);

        let mut contents = [0xff; 105];
        assert_eq!(
            fs.read_file_at(&mut file, 0, &mut contents).await.unwrap(),
            contents.len()
        );
        assert_eq!(&contents[..100], &[0; 100]);
        assert_eq!(&contents[100..], b"async");
    });
}
