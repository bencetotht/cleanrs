//! Generate a deterministic benchmark fixture at an explicitly supplied path.

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

fn main() -> io::Result<()> {
    let mut args = env::args_os().skip(1);
    let root = PathBuf::from(args.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: synthetic_tree ROOT [FILES]",
        )
    })?);
    let file_count = args
        .next()
        .map(|value| value.to_string_lossy().parse::<usize>())
        .transpose()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?
        .unwrap_or(20_000);
    if root.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "benchmark root already exists; refusing to overwrite it",
        ));
    }

    for index in 0..file_count {
        let directory = root.join(format!("d{:04}", index / 100));
        fs::create_dir_all(&directory)?;
        let mut file = fs::File::create(directory.join(format!("f{index:08}")))?;
        file.write_all(&index.to_le_bytes())?;
    }
    println!("created {file_count} files beneath {}", root.display());
    Ok(())
}
