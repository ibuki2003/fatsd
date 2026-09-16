//! Lists one directory in a FAT image.
//!
//! Usage: `cargo run --example list_image -- IMAGE [DIRECTORY]`

use std::{
    env,
    fs::File,
    io::{Read, Seek, SeekFrom},
    process,
};

use fatsd::{BlockAccess, DirectoryAccess};

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
        .ok_or_else(|| "usage: list_image IMAGE [DIRECTORY]".to_owned())?;
    let directory_path = args.next().unwrap_or_else(|| "/".to_owned());
    if args.next().is_some() {
        return Err("usage: list_image IMAGE [DIRECTORY]".to_owned());
    }

    let image = Image {
        file: File::open(image_path).map_err(|error| error.to_string())?,
        block_size: 128,
    };
    let mut fs = Fs::mount(image).map_err(|error| format!("mount failed: {error:?}"))?;
    let directory = fs
        .open_directory(&directory_path)
        .map_err(|error| format!("open directory failed: {error:?}"))?;
    eprintln!("listing {directory_path} ({:?})", fs.volume_info().fat_type);
    let mut cursor = 0;
    let mut name = [0; 260];
    while let Some(entry) = fs
        .read_next_directory_entry(&directory, &mut cursor, &mut name)
        .map_err(|error| format!("read directory failed: {error:?}"))?
    {
        println!(
            "{}\t{}\t{}",
            if entry.raw.is_directory() {
                "dir"
            } else {
                "file"
            },
            entry.raw.file_size(),
            String::from_utf16_lossy(&name[..entry.name_length]),
        );
    }
    Ok(())
}
