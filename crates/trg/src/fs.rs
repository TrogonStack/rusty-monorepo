use std::io;
use std::path::{Path, PathBuf};

pub trait FileSystem {
    fn read_to_string(&self, path: &Path) -> io::Result<String>;
    fn read_bytes(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn write(&self, path: &Path, contents: &str) -> io::Result<()>;
    fn exists(&self, path: &Path) -> bool;
    fn is_file(&self, path: &Path) -> bool;
    fn is_dir(&self, path: &Path) -> bool;
    fn is_symlink(&self, path: &Path) -> bool;
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>>;
}

pub struct RealFS;

impl FileSystem for RealFS {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn read_bytes(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    fn write(&self, path: &Path, contents: &str) -> io::Result<()> {
        std::fs::write(path, contents)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn is_symlink(&self, path: &Path) -> bool {
        std::fs::symlink_metadata(path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        std::fs::read_dir(path)?.map(|entry| entry.map(|e| e.path())).collect()
    }
}

#[cfg(test)]
pub mod testutil;

#[cfg(test)]
#[path = "fs_tests.rs"]
mod tests;
