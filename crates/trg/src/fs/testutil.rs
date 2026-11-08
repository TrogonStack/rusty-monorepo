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

    fn read_bytes(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.files
            .borrow()
            .get(path)
            .map(|content| content.as_bytes().to_vec())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "file not found"))
    }

    fn write(&self, path: &Path, contents: &str) -> io::Result<()> {
        self.files.borrow_mut().insert(path.to_path_buf(), contents.to_string());
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        self.files.borrow().contains_key(path)
    }

    fn is_file(&self, path: &Path) -> bool {
        self.files.borrow().contains_key(path)
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.files
            .borrow()
            .keys()
            .any(|candidate| candidate != path && candidate.starts_with(path))
    }

    fn is_symlink(&self, _path: &Path) -> bool {
        false
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let mut children: Vec<PathBuf> = self
            .files
            .borrow()
            .keys()
            .filter_map(|candidate| {
                let rest = candidate.strip_prefix(path).ok()?;
                let mut components = rest.components();
                let first = components.next()?;
                Some(path.join(first))
            })
            .collect();
        children.sort();
        children.dedup();
        Ok(children)
    }
}
