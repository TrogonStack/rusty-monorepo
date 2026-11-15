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
