//! Mutates files in a FAT image for interoperability testing.
//!
//! Usage: `cargo run --example write_image -- IMAGE PAYLOAD_FILE`

use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    process,
};

use fatsd::{BlockAccess, BlockWrite, DirectoryWrite, FileAccess, FileWrite};

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

impl BlockWrite for Image {
    fn write_block_at(
        &mut self,
        block: u64,
        offset: usize,
        data: &[u8],
    ) -> Result<(), Self::Error> {
        assert!(offset + data.len() <= self.block_size);
        self.file.seek(SeekFrom::Start(
            block * self.block_size as u64 + offset as u64,
        ))?;
        self.file.write_all(data)
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
        .ok_or_else(|| "usage: write_image IMAGE PAYLOAD_FILE".to_owned())?;
    let payload_path = args
        .next()
        .ok_or_else(|| "usage: write_image IMAGE PAYLOAD_FILE".to_owned())?;
    if args.next().is_some() {
        return Err("usage: write_image IMAGE PAYLOAD_FILE".to_owned());
    }
    let payload = fs::read(payload_path).map_err(|error| error.to_string())?;
    let image = Image {
        file: OpenOptions::new()
            .read(true)
            .write(true)
            .open(image_path)
            .map_err(|error| error.to_string())?,
        block_size: 128,
    };
    let mut fs = Fs::mount(image).map_err(|error| format!("mount failed: {error:?}"))?;

    let mut existing = fs
        .open_file("/Existing.bin")
        .map_err(|error| format!("open existing failed: {error:?}"))?;
    fs.write_file_at(&mut existing, 13, &payload)
        .map_err(|error| format!("overwrite failed: {error:?}"))?;
    fs.truncate_file(&mut existing, 13 + payload.len() as u64 / 2)
        .map_err(|error| format!("shrink failed: {error:?}"))?;
    fs.truncate_file(&mut existing, 13 + payload.len() as u64 + 257)
        .map_err(|error| format!("regrow failed: {error:?}"))?;

    let mut created = fs
        .create_file("/Nested/Created by fatsd.bin")
        .map_err(|error| format!("create failed: {error:?}"))?;
    fs.write_file_at(&mut created, 257, &payload)
        .map_err(|error| format!("write created file failed: {error:?}"))?;

    fs.create_directory("/Source directory")
        .map_err(|error| format!("create source directory failed: {error:?}"))?;
    fs.create_directory("/Source directory/Child")
        .map_err(|error| format!("create child directory failed: {error:?}"))?;
    let mut moving = fs
        .create_file("/Source directory/Child/Moving payload.bin")
        .map_err(|error| format!("create moving file failed: {error:?}"))?;
    fs.write_file_at(&mut moving, 0, &payload)
        .map_err(|error| format!("write moving file failed: {error:?}"))?;
    fs.rename(
        "/Source directory/Child/Moving payload.bin",
        "/Nested/Moved payload.bin",
    )
    .map_err(|error| format!("move file failed: {error:?}"))?;
    fs.rename("/Source directory/Child", "/Moved child")
        .map_err(|error| format!("move directory failed: {error:?}"))?;
    fs.remove_directory("/Moved child")
        .map_err(|error| format!("remove moved directory failed: {error:?}"))?;
    fs.remove_directory("/Source directory")
        .map_err(|error| format!("remove source directory failed: {error:?}"))?;

    let mut removed = fs
        .create_file("/Removed file.bin")
        .map_err(|error| format!("create removed file failed: {error:?}"))?;
    fs.write_file_at(&mut removed, 0, &payload)
        .map_err(|error| format!("write removed file failed: {error:?}"))?;
    fs.remove_file("/REMOVE~1.BIN")
        .map_err(|error| format!("remove file failed: {error:?}"))?;

    fs.create_directory("/Expanded directory")
        .map_err(|error| format!("create expanded directory failed: {error:?}"))?;
    for index in 0..20 {
        fs.create_file(&format!("/Expanded directory/F{index:02}.TXT"))
            .map_err(|error| format!("grow directory failed: {error:?}"))?;
    }

    println!("updated {:?} image", fs.volume_info().fat_type);
    Ok(())
}
