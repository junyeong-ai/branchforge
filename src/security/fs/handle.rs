//! Secure file handle with TOCTOU protection.

#![allow(missing_docs)]

use std::ffi::CString;
use std::io::{Read, Write};
use std::mem::ManuallyDrop;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::{AsFd, AsRawFd, FromRawFd, OwnedFd};

use rustix::fs::{AtFlags, Mode, OFlags, openat, renameat, unlinkat};
use uuid::Uuid;

use super::super::SecurityError;
use super::super::path::SafePath;

const MAX_FILE_SIZE: u64 = 100 * 1024 * 1024;

fn with_borrowed_file<T>(fd: &OwnedFd, f: impl FnOnce(&mut std::fs::File) -> T) -> T {
    let mut file = ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd.as_raw_fd()) });
    f(&mut file)
}

pub struct SecureFileHandle {
    fd: OwnedFd,
    path: SafePath,
}

impl SecureFileHandle {
    pub fn open_read(path: SafePath) -> Result<Self, SecurityError> {
        let fd = path.open(OFlags::RDONLY)?;
        Ok(Self { fd, path })
    }

    pub fn open_write(path: SafePath) -> Result<Self, SecurityError> {
        path.create_parent_dirs()?;
        let fd = path.open(OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC)?;
        Ok(Self { fd, path })
    }

    pub fn open_append(path: SafePath) -> Result<Self, SecurityError> {
        path.create_parent_dirs()?;
        let fd = path.open(OFlags::WRONLY | OFlags::CREATE | OFlags::APPEND)?;
        Ok(Self { fd, path })
    }

    pub fn for_atomic_write(path: SafePath) -> Result<Self, SecurityError> {
        path.create_parent_dirs()?;
        let fd = path
            .open(OFlags::RDONLY)
            .or_else(|_| path.open(OFlags::WRONLY | OFlags::CREATE))?;
        Ok(Self { fd, path })
    }

    pub fn path(&self) -> &SafePath {
        &self.path
    }

    pub fn display_path(&self) -> String {
        self.path.as_path().display().to_string()
    }

    pub fn read_to_string(&self) -> Result<String, SecurityError> {
        self.check_file_size()?;
        let mut content = String::new();
        with_borrowed_file(&self.fd, |file| file.read_to_string(&mut content))?;
        Ok(content)
    }

    pub fn read_bytes(&self) -> Result<Vec<u8>, SecurityError> {
        self.check_file_size()?;
        let mut content = Vec::new();
        with_borrowed_file(&self.fd, |file| file.read_to_end(&mut content))?;
        Ok(content)
    }

    fn check_file_size(&self) -> Result<(), SecurityError> {
        let stat = rustix::fs::fstat(&self.fd)
            .map_err(|e| SecurityError::Io(std::io::Error::from_raw_os_error(e.raw_os_error())))?;

        let size = stat.st_size as u64;
        if size > MAX_FILE_SIZE {
            return Err(SecurityError::InvalidPath(format!(
                "File too large: {} bytes (max {} bytes)",
                size, MAX_FILE_SIZE
            )));
        }
        Ok(())
    }

    pub fn write_all(&self, content: &[u8]) -> Result<(), SecurityError> {
        with_borrowed_file(&self.fd, |file| -> std::io::Result<()> {
            file.write_all(content)?;
            file.sync_all()
        })?;
        Ok(())
    }

    pub fn atomic_write(&self, content: &[u8]) -> Result<(), SecurityError> {
        let filename = self
            .path
            .filename()
            .ok_or_else(|| SecurityError::InvalidPath("no filename".into()))?;

        let temp_name = format!(".{}.{}.tmp", filename.to_string_lossy(), Uuid::new_v4());
        let temp_cname = CString::new(temp_name.as_bytes())
            .map_err(|_| SecurityError::InvalidPath("invalid temp name".into()))?;

        let parent_fd = self.get_parent_fd()?;

        let temp_fd = openat(
            parent_fd.as_fd(),
            &temp_cname,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o644),
        )
        .map_err(|e| SecurityError::Io(std::io::Error::from_raw_os_error(e.raw_os_error())))?;

        let write_result = with_borrowed_file(&temp_fd, |file| file.write_all(content));

        if let Err(e) = write_result {
            let _ = unlinkat(parent_fd.as_fd(), &temp_cname, AtFlags::empty());
            return Err(SecurityError::Io(e));
        }

        rustix::fs::fsync(&temp_fd)
            .map_err(|e| SecurityError::Io(std::io::Error::from_raw_os_error(e.raw_os_error())))?;

        let filename_cstr = CString::new(filename.as_bytes())
            .map_err(|_| SecurityError::InvalidPath("invalid filename".into()))?;

        renameat(
            parent_fd.as_fd(),
            &temp_cname,
            parent_fd.as_fd(),
            &filename_cstr,
        )
        .map_err(|e| SecurityError::Io(std::io::Error::from_raw_os_error(e.raw_os_error())))?;

        rustix::fs::fsync(&parent_fd)
            .map_err(|e| SecurityError::Io(std::io::Error::from_raw_os_error(e.raw_os_error())))?;

        Ok(())
    }

    fn get_parent_fd(&self) -> Result<OwnedFd, SecurityError> {
        let parent_components = self.path.parent_components();

        if parent_components.is_empty() {
            return rustix::fs::openat(
                self.path.root_fd(),
                c".",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| SecurityError::Io(std::io::Error::from_raw_os_error(e.raw_os_error())));
        }

        let mut current_fd = self.path.root_fd();
        let mut owned_fds: Vec<OwnedFd> = Vec::new();

        for component in parent_components {
            let c_name = CString::new(component.as_bytes())
                .map_err(|_| SecurityError::InvalidPath("null byte".into()))?;

            let fd = openat(
                current_fd,
                &c_name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| SecurityError::Io(std::io::Error::from_raw_os_error(e.raw_os_error())))?;

            owned_fds.push(fd);
            current_fd = owned_fds.last().expect("just pushed").as_fd();
        }

        owned_fds
            .pop()
            .ok_or_else(|| SecurityError::InvalidPath("no parent".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn create_safe_path(dir: &Path, filename: &str) -> SafePath {
        let root = std::fs::canonicalize(dir).unwrap();
        let root_fd = Arc::new(std::fs::File::open(&root).unwrap().into());
        SafePath::resolve(root_fd, root, Path::new(filename), 10).unwrap()
    }

    #[test]
    fn test_read_file() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        fs::write(root.join("test.txt"), "hello world").unwrap();

        let path = create_safe_path(&root, "test.txt");
        let handle = SecureFileHandle::open_read(path).unwrap();
        let content = handle.read_to_string().unwrap();
        assert_eq!(content, "hello world");
    }

    #[test]
    fn test_write_file() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        let path = create_safe_path(&root, "output.txt");
        let handle = SecureFileHandle::open_write(path).unwrap();
        handle.write_all(b"test content").unwrap();

        let content = fs::read_to_string(root.join("output.txt")).unwrap();
        assert_eq!(content, "test content");
    }

    #[test]
    fn test_atomic_write() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        let path = create_safe_path(&root, "atomic.txt");
        let handle = SecureFileHandle::for_atomic_write(path.clone()).unwrap();
        handle.atomic_write(b"atomic content").unwrap();

        let content = fs::read_to_string(root.join("atomic.txt")).unwrap();
        assert_eq!(content, "atomic content");

        let entries: Vec<_> = fs::read_dir(&root).unwrap().collect();
        assert!(!entries.iter().any(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp")
        }));
    }

    #[test]
    fn test_atomic_write_preserves_original_on_new_file() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        let path = create_safe_path(&root, "new_atomic.txt");
        let handle = SecureFileHandle::for_atomic_write(path).unwrap();
        handle.atomic_write(b"new content").unwrap();

        let content = fs::read_to_string(root.join("new_atomic.txt")).unwrap();
        assert_eq!(content, "new content");
    }

    #[test]
    fn test_atomic_write_overwrites_existing() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        fs::write(root.join("existing.txt"), "original").unwrap();

        let path = create_safe_path(&root, "existing.txt");
        let handle = SecureFileHandle::for_atomic_write(path).unwrap();
        handle.atomic_write(b"updated").unwrap();

        let content = fs::read_to_string(root.join("existing.txt")).unwrap();
        assert_eq!(content, "updated");
    }

    #[test]
    fn test_for_atomic_write_does_not_truncate() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        fs::write(root.join("preserve.txt"), "original content").unwrap();

        let path = create_safe_path(&root, "preserve.txt");
        let _handle = SecureFileHandle::for_atomic_write(path).unwrap();

        let content = fs::read_to_string(root.join("preserve.txt")).unwrap();
        assert_eq!(content, "original content");
    }

    #[test]
    fn test_create_nested_dirs() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        let path = create_safe_path(&root, "a/b/c/file.txt");
        let handle = SecureFileHandle::open_write(path).unwrap();
        handle.write_all(b"nested").unwrap();

        let content = fs::read_to_string(root.join("a/b/c/file.txt")).unwrap();
        assert_eq!(content, "nested");
    }
}
