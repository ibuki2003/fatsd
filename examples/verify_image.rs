//! Compares a file in a FAT image with a host file.
//!
//! Usage: `cargo run --example verify_image -- IMAGE FAT_PATH EXPECTED_FILE`

use std::{
    env,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    process,
};

use fatsd::{BlockAccess, FileAccess, FileHandle};

mod support;
use support::Fs;

struct Image {
    file: File,
    block_size: usize,
}

impl BlockAccess for Image {
    type Error = std::io::Error;

    fn block_size(&self) -> usize {
        self.block_size
    }

    fn read_block_at(
        &mut self,
        block: u64,
        offset: usize,
        out: &mut [u8],
    ) -> Result<(), Self::Error> {
        assert!(offset + out.len() <= self.block_size);
        self.file.seek(SeekFrom::Start(
            block * self.block_size as u64 + offset as u64,
        ))?;
        self.file.read_exact(out)
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let image_path = args
        .next()
        .ok_or_else(|| "usage: verify_image IMAGE FAT_PATH EXPECTED_FILE".to_owned())?;
    let fat_path = args
        .next()
        .ok_or_else(|| "usage: verify_image IMAGE FAT_PATH EXPECTED_FILE".to_owned())?;
    let expected_path = args
        .next()
        .ok_or_else(|| "usage: verify_image IMAGE FAT_PATH EXPECTED_FILE".to_owned())?;
    if args.next().is_some() {
        return Err("usage: verify_image IMAGE FAT_PATH EXPECTED_FILE".to_owned());
    }

    let expected = fs::read(expected_path).map_err(|error| error.to_string())?;
    let image = Image {
        file: File::open(image_path).map_err(|error| error.to_string())?,
        // The test intentionally uses blocks smaller than a FAT sector to exercise splitting.
        block_size: 128,
    };
    let mut fs = Fs::mount(image).map_err(|error| format!("mount failed: {error:?}"))?;
    let mut file = fs
        .open_file(&fat_path)
        .map_err(|error| format!("open failed: {error:?}"))?;
    if file.file_info().length != expected.len() as u64 {
        return Err(format!(
            "length mismatch: FAT={} expected={}",
            file.file_info().length,
            expected.len()
        ));
    }

    let mut actual = vec![0; expected.len()];
    let mut offset = 0;
    while offset < actual.len() {
        let end = (offset + 4093).min(actual.len());
        let read = fs
            .read_file_at(&mut file, offset as u64, &mut actual[offset..end])
            .map_err(|error| format!("read at {offset} failed: {error:?}"))?;
        if read != end - offset {
            return Err(format!("short read at {offset}: {read}"));
        }
        offset = end;
    }
    if actual != expected {
        return Err("sequential content mismatch".to_owned());
    }

    for offset in [0, 1, 127, 128, 511, 512, 4095, 65_537, 262_141] {
        if offset >= expected.len() {
            continue;
        }
        let end = (offset + 257).min(expected.len());
        let mut probe = vec![0; end - offset];
        let read = fs
            .read_file_at(&mut file, offset as u64, &mut probe)
            .map_err(|error| format!("probe at {offset} failed: {error:?}"))?;
        if read != probe.len() || probe != expected[offset..end] {
            return Err(format!("probe mismatch at {offset}"));
        }
    }

    println!(
        "verified {} bytes from {} ({:?})",
        expected.len(),
        fat_path,
        fs.volume_info().fat_type
    );
    Ok(())
}
