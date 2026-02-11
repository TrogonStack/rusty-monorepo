use std::io;
use std::path::Path;

pub trait FileSystem {
    fn read_to_string(&self, path: &Path) -> io::Result<String>;
    fn write(&self, path: &Path, contents: &str) -> io::Result<()>;
    fn exists(&self, path: &Path) -> bool;
}

pub struct RealFS;

impl FileSystem for RealFS {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn write(&self, path: &Path, contents: &str) -> io::Result<()> {
        std::fs::write(path, contents)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

#[cfg(test)]
pub mod testutil {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    pub struct MemFS {
        files: std::cell::RefCell<HashMap<PathBuf, String>>,
    }

    impl MemFS {
        pub fn new() -> Self {
            MemFS {
                files: std::cell::RefCell::new(HashMap::new()),
            }
        }

        pub fn insert(&self, path: impl AsRef<Path>, content: impl Into<String>) {
            self.files
                .borrow_mut()
                .insert(path.as_ref().to_path_buf(), content.into());
        }
    }

    impl Default for MemFS {
        fn default() -> Self {
            Self::new()
        }
    }

    impl FileSystem for MemFS {
        fn read_to_string(&self, path: &Path) -> io::Result<String> {
            self.files
                .borrow()
                .get(path)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "file not found"))
        }

        fn write(&self, path: &Path, contents: &str) -> io::Result<()> {
            self.files.borrow_mut().insert(path.to_path_buf(), contents.to_string());
            Ok(())
        }

        fn exists(&self, path: &Path) -> bool {
            self.files.borrow().contains_key(path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::MemFS;
    use super::*;

    #[test]
    fn test_memfs_write_and_read() {
        let fs = MemFS::new();
        let path = Path::new("/test/file.txt");

        fs.write(path, "hello world").unwrap();
        let content = fs.read_to_string(path).unwrap();

        assert_eq!(content, "hello world");
    }

    #[test]
    fn test_memfs_exists() {
        let fs = MemFS::new();
        let path = Path::new("/test/file.txt");

        assert!(!fs.exists(path));
        fs.write(path, "content").unwrap();
        assert!(fs.exists(path));
    }

    #[test]
    fn test_memfs_not_found() {
        let fs = MemFS::new();
        let result = fs.read_to_string(Path::new("/nonexistent"));
        assert!(result.is_err());
    }

    #[test]
    fn test_memfs_multiple_files() {
        let fs = MemFS::new();

        fs.write(Path::new("/skill1/SKILL.md"), "skill 1").unwrap();
        fs.write(Path::new("/skill2/SKILL.md"), "skill 2").unwrap();
        fs.write(Path::new("/skill3/SKILL.md"), "skill 3").unwrap();

        assert_eq!(fs.read_to_string(Path::new("/skill1/SKILL.md")).unwrap(), "skill 1");
        assert_eq!(fs.read_to_string(Path::new("/skill2/SKILL.md")).unwrap(), "skill 2");
        assert_eq!(fs.read_to_string(Path::new("/skill3/SKILL.md")).unwrap(), "skill 3");
    }

    #[test]
    fn test_memfs_overwrite() {
        let fs = MemFS::new();
        let path = Path::new("/test.txt");

        fs.write(path, "v1").unwrap();
        assert_eq!(fs.read_to_string(path).unwrap(), "v1");

        fs.write(path, "v2").unwrap();
        assert_eq!(fs.read_to_string(path).unwrap(), "v2");
    }
}
