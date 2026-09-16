#![cfg(feature = "async")]

use core::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};

use fatsd::{AsyncBlockAccess, read_mbr_async};

struct MemoryDevice([u8; 512]);

impl AsyncBlockAccess for MemoryDevice {
    type Error = ();

    fn block_size(&self) -> usize {
        512
    }

    async fn read_block_at(
        &mut self,
        block: u64,
        offset: usize,
        out: &mut [u8],
    ) -> Result<(), Self::Error> {
        assert_eq!(block, 0);
        out.copy_from_slice(&self.0[offset..offset + out.len()]);
        Ok(())
    }
}

#[test]
fn reads_partition_table_asynchronously() {
    let mut bytes = [0; 512];
    bytes[450] = 0x0b;
    bytes[454..458].copy_from_slice(&63_u32.to_le_bytes());
    bytes[458..462].copy_from_slice(&1000_u32.to_le_bytes());
    bytes[510..512].copy_from_slice(&[0x55, 0xaa]);
    let mut device = MemoryDevice(bytes);

    let table = block_on(read_mbr_async(&mut device)).unwrap();

    assert_eq!(table.entries[0].partition_type, 0x0b);
    assert_eq!(table.entries[0].first_lba, 63);
    assert_eq!(table.entries[0].sector_count, 1000);
}

fn block_on<T>(future: impl Future<Output = T>) -> T {
    let mut future = pin!(future);
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => {}
        }
    }
}
