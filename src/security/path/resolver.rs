//! TOCTOU-safe path resolution using openat() with O_NOFOLLOW.

#![allow(missing_docs)]

use std::ffi::{CString, OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use rustix::fs::{Mode, OFlags, openat};
use rustix::io::Errno;

use crate::security::SecurityError;

#[derive(Debug)]
pub struct SafePath {
    root_fd: Arc<OwnedFd>,
    root_path: PathBuf,
    components: Vec<OsString>,
    resolved_path: PathBuf,
    permissive: bool,
}

impl SafePath {
    pub fn resolve(
        root_fd: Arc<OwnedFd>,
        root_path: PathBuf,
        relative_path: &Path,
        max_symlink_depth: u8,
    ) -> Result<Self, SecurityError> {
        let mut components = Vec::new();
        let mut symlink_depth = 0u8;

        for component in relative_path.components() {
            match component {
                Component::ParentDir => {
                    if components.is_empty() {
                        return Err(SecurityError::PathEscape(relative_path.to_path_buf()));
                    }
                    components.pop();
                }
                Component::CurDir | Component::RootDir => {}
                Component::Normal(name) => {
                    components.push(name.to_os_string());
                }
                Component::Prefix(_) => {}
            }
        }

        let mut validated_components = Vec::new();
        let mut current_fd: BorrowedFd<'_> = root_fd.as_fd();
        let mut owned_fds: Vec<OwnedFd> = Vec::new();

        for (i, component) in components.iter().enumerate() {
            let is_last = i == components.len() - 1;

            let c_name = CString::new(component.as_bytes())
                .map_err(|_| SecurityError::InvalidPath("null byte in path".into()))?;

            let flags = if is_last {
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC
            } else {
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
            };

            match openat(current_fd, &c_name, flags, Mode::empty()) {
                Ok(fd) => {
                    validated_components.push(component.clone());
                    if !is_last {
                        owned_fds.push(fd);
                        // SAFETY: We just pushed to owned_fds, so last() is guaranteed to be Some
                        current_fd = owned_fds
                            .last()
                            .expect("owned_fds is non-empty after push")
                            .as_fd();
                    } else {
                        drop(fd);
                    }
                }
                Err(Errno::LOOP) | Err(Errno::MLINK) => {
                    symlink_depth += 1;
                    if symlink_depth > max_symlink_depth {
                        return Err(SecurityError::SymlinkDepthExceeded {
                            path: relative_path.to_path_buf(),
                            max: max_symlink_depth,
                        });
                    }

                    let target = rustix::fs::readlinkat(current_fd, &c_name, vec![0u8; 4096])
                        .map_err(|e| {
                            SecurityError::Io(std::io::Error::from_raw_os_error(e.raw_os_error()))
                        })?;

                    let target_path = PathBuf::from(OsStr::from_bytes(target.to_bytes()));
                    if target_path.is_absolute() {
                        if !target_path.starts_with(&root_path) {
                            return Err(SecurityError::AbsoluteSymlink(target_path));
                        }
                        let relative = target_path
                            .strip_prefix(&root_path)
                            .expect("path verified with starts_with");
                        return Self::resolve(
                            Arc::clone(&root_fd),
                            root_path,
                            relative,
                            max_symlink_depth - symlink_depth,
                        );
                    }

                    let mut remaining: Vec<OsString> = target_path
                        .components()
                        .filter_map(|c| match c {
                            Component::Normal(s) => Some(s.to_os_string()),
                            _ => None,
                        })
                        .collect();

                    remaining.extend(components.iter().skip(i + 1).cloned());

                    let new_path: PathBuf = remaining.iter().collect();
                    let current_path: PathBuf = validated_components.iter().collect();
                    let full_path = current_path.join(&new_path);

                    return Self::resolve(
                        Arc::clone(&root_fd),
                        root_path,
                        &full_path,
                        max_symlink_depth - symlink_depth,
                    );
                }
                Err(Errno::NOENT) => {
                    validated_components.push(component.clone());
                    validated_components.extend(components.iter().skip(i + 1).cloned());
                    break;
                }
                Err(e) => {
                    return Err(SecurityError::Io(std::io::Error::from_raw_os_error(
                        e.raw_os_error(),
                    )));
                }
            }
        }

        let resolved_path = root_path.join(validated_components.iter().collect::<PathBuf>());

        Ok(Self {
            root_fd,
            root_path,
            components: validated_components,
            resolved_path,
            permissive: false,
        })
    }

    /// Create a SafePath without validation (for permissive mode).
    /// This bypasses TOCTOU protection but allows symlinks.
    pub fn unchecked(root_fd: Arc<OwnedFd>, resolved_path: PathBuf) -> Self {
        let root_path = PathBuf::from("/");
        let components = resolved_path
            .strip_prefix("/")
            .unwrap_or(&resolved_path)
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => Some(s.to_os_string()),
                _ => None,
            })
            .collect();

        Self {
            root_fd,
            root_path,
            components,
            resolved_path,
            permissive: true,
        }
    }

    pub fn is_permissive(&self) -> bool {
        self.permissive
    }

    pub fn root_fd(&self) -> BorrowedFd<'_> {
        self.root_fd.as_fd()
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub fn components(&self) -> &[OsString] {
        &self.components
    }

    pub fn as_path(&self) -> &Path {
        &self.resolved_path
    }

    pub fn filename(&self) -> Option<&OsStr> {
        self.components.last().map(|s| s.as_os_str())
    }

    pub fn parent_components(&self) -> &[OsString] {
        if self.components.is_empty() {
            &[]
        } else {
            &self.components[..self.components.len() - 1]
        }
    }

    pub fn open(&self, flags: OFlags) -> Result<OwnedFd, SecurityError> {
        // In permissive mode, use standard library to handle symlinks
        if self.permissive {
            use std::fs::OpenOptions;
            use std::os::unix::fs::OpenOptionsExt;

            let mut opts = OpenOptions::new();

            if flags.contains(OFlags::RDONLY) && !flags.contains(OFlags::WRONLY) {
                opts.read(true);
            }
            if flags.contains(OFlags::WRONLY) || flags.contains(OFlags::RDWR) {
                opts.write(true);
            }
            if flags.contains(OFlags::RDWR) {
                opts.read(true);
            }
            if flags.contains(OFlags::CREATE) {
                opts.create(true);
            }
            if flags.contains(OFlags::TRUNC) {
                opts.truncate(true);
            }
            if flags.contains(OFlags::APPEND) {
                opts.append(true);
            }

            opts.mode(0o644);

            let file = opts.open(&self.resolved_path).map_err(SecurityError::Io)?;
            return Ok(file.into());
        }

        if self.components.is_empty() {
            let fd = rustix::fs::openat(
                self.root_fd.as_fd(),
                c".",
                flags | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| SecurityError::Io(std::io::Error::from_raw_os_error(e.raw_os_error())))?;
            return Ok(fd);
        }

        let mut current_fd: BorrowedFd<'_> = self.root_fd.as_fd();
        let mut owned_fds: Vec<OwnedFd> = Vec::new();

        for (i, component) in self.components.iter().enumerate() {
            let is_last = i == self.components.len() - 1;
            let c_name = CString::new(component.as_bytes())
                .map_err(|_| SecurityError::InvalidPath("null byte".into()))?;

            let open_flags = if is_last {
                flags | OFlags::NOFOLLOW | OFlags::CLOEXEC
            } else {
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
            };

            let fd = openat(current_fd, &c_name, open_flags, Mode::from_raw_mode(0o644)).map_err(
                |e| SecurityError::Io(std::io::Error::from_raw_os_error(e.raw_os_error())),
            )?;

            if is_last {
                return Ok(fd);
            }

            owned_fds.push(fd);
            // SAFETY: We just pushed to owned_fds, so last() is guaranteed to be Some
            current_fd = owned_fds
                .last()
                .expect("owned_fds is non-empty after push")
                .as_fd();
        }

        unreachable!("loop always returns on is_last")
    }

    pub fn create_parent_dirs(&self) -> Result<(), SecurityError> {
        if self.components.len() <= 1 {
            return Ok(());
        }

        // In permissive mode, use standard library to handle symlinks
        if self.permissive {
            if let Some(parent) = self.resolved_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            return Ok(());
        }

        let mut current_fd: BorrowedFd<'_> = self.root_fd.as_fd();
        let mut owned_fds: Vec<OwnedFd> = Vec::new();

        for component in self.parent_components() {
            let c_name = CString::new(component.as_bytes())
                .map_err(|_| SecurityError::InvalidPath("null byte".into()))?;

            match openat(
                current_fd,
                &c_name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(fd) => {
                    owned_fds.push(fd);
                    // SAFETY: We just pushed to owned_fds, so last() is guaranteed to be Some
                    current_fd = owned_fds
                        .last()
                        .expect("owned_fds is non-empty after push")
                        .as_fd();
                }
                Err(Errno::NOENT) => {
                    rustix::fs::mkdirat(current_fd, &c_name, Mode::from_raw_mode(0o755)).map_err(
                        |e| SecurityError::Io(std::io::Error::from_raw_os_error(e.raw_os_error())),
                    )?;

                    let fd = openat(
                        current_fd,
                        &c_name,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                        Mode::empty(),
                    )
                    .map_err(|e| {
                        SecurityError::Io(std::io::Error::from_raw_os_error(e.raw_os_error()))
                    })?;

                    owned_fds.push(fd);
                    // SAFETY: We just pushed to owned_fds, so last() is guaranteed to be Some
                    current_fd = owned_fds
                        .last()
                        .expect("owned_fds is non-empty after push")
                        .as_fd();
                }
                Err(e) => {
                    return Err(SecurityError::Io(std::io::Error::from_raw_os_error(
                        e.raw_os_error(),
                    )));
                }
            }
        }

        Ok(())
    }
}

impl Clone for SafePath {
    fn clone(&self) -> Self {
        Self {
            root_fd: Arc::clone(&self.root_fd),
            root_path: self.root_path.clone(),
            components: self.components.clone(),
            resolved_path: self.resolved_path.clone(),
            permissive: self.permissive,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn open_dir(path: &Path) -> Arc<OwnedFd> {
        let fd = std::fs::File::open(path).unwrap();
        Arc::new(fd.into())
    }

    #[test]
    fn test_resolve_simple() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        fs::write(root.join("test.txt"), "content").unwrap();

        let root_fd = open_dir(&root);
        let path = SafePath::resolve(root_fd, root.clone(), Path::new("test.txt"), 10).unwrap();

        assert_eq!(path.as_path(), root.join("test.txt"));
    }

    #[test]
    fn test_resolve_nonexistent() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        let root_fd = open_dir(&root);
        let path = SafePath::resolve(root_fd, root.clone(), Path::new("newfile.txt"), 10).unwrap();

        assert_eq!(path.as_path(), root.join("newfile.txt"));
    }

    #[test]
    fn test_path_traversal_blocked() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        let root_fd = open_dir(&root);
        let result = SafePath::resolve(root_fd, root, Path::new("../../../etc/passwd"), 10);

        assert!(matches!(result, Err(SecurityError::PathEscape(_))));
    }

    #[test]
    fn test_symlink_within_sandbox() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        fs::write(root.join("target.txt"), "content").unwrap();
        std::os::unix::fs::symlink("target.txt", root.join("link.txt")).unwrap();

        let root_fd = open_dir(&root);
        let path = SafePath::resolve(root_fd, root.clone(), Path::new("link.txt"), 10).unwrap();

        assert_eq!(path.as_path(), root.join("target.txt"));
    }

    #[test]
    fn test_symlink_depth_limit() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        for i in 0..15 {
            let target = if i == 14 {
                "final.txt".to_string()
            } else {
                format!("link{}.txt", i + 1)
            };
            std::os::unix::fs::symlink(&target, root.join(format!("link{}.txt", i))).unwrap();
        }
        fs::write(root.join("final.txt"), "content").unwrap();

        let root_fd = open_dir(&root);
        let result = SafePath::resolve(root_fd, root, Path::new("link0.txt"), 10);

        assert!(matches!(
            result,
            Err(SecurityError::SymlinkDepthExceeded { .. })
        ));
    }
}
