//! Secure filesystem operations with TOCTOU protection.

mod handle;

pub use handle::SecureFileHandle;

use std::os::unix::io::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use glob::Pattern;

use super::SecurityError;
use super::path::{SafePath, normalize_path};
use crate::authorization::ToolLimits;

#[derive(Clone)]
pub struct SecureFs {
    root_fd: Arc<OwnedFd>,
    root_path: PathBuf,
    allowed_paths: Vec<PathBuf>,
    denied_patterns: Vec<Pattern>,
    max_symlink_depth: u8,
    permissive: bool,
}

impl SecureFs {
    pub fn new(
        root: &Path,
        allowed_paths: Vec<PathBuf>,
        denied_patterns: &[String],
        max_symlink_depth: u8,
    ) -> Result<Self, SecurityError> {
        let root_path = if root.exists() {
            std::fs::canonicalize(root)?
        } else {
            normalize_path(root)
        };

        let root_fd = std::fs::File::open(&root_path)?;

        let compiled_patterns = denied_patterns
            .iter()
            .filter_map(|p| Pattern::new(p).ok())
            .collect();

        Ok(Self {
            root_fd: Arc::new(root_fd.into()),
            root_path,
            allowed_paths: allowed_paths
                .into_iter()
                .filter_map(|p| {
                    if p.exists() {
                        std::fs::canonicalize(&p).ok()
                    } else {
                        Some(normalize_path(&p))
                    }
                })
                .collect(),
            denied_patterns: compiled_patterns,
            max_symlink_depth,
            permissive: false,
        })
    }

    /// Create a permissive SecureFs that allows all operations.
    ///
    /// # Panics
    /// Panics if "/" cannot be opened, which indicates a fundamentally broken system.
    pub fn permissive() -> Self {
        let root_fd = std::fs::File::open("/").expect("failed to open root directory");
        Self {
            root_fd: Arc::new(root_fd.into()),
            root_path: PathBuf::from("/"),
            allowed_paths: Vec::new(),
            denied_patterns: Vec::new(),
            max_symlink_depth: 255,
            permissive: true,
        }
    }

    pub fn is_permissive(&self) -> bool {
        self.permissive
    }

    pub fn root(&self) -> &Path {
        &self.root_path
    }

    pub fn resolve(&self, input_path: &str) -> Result<SafePath, SecurityError> {
        if input_path.contains('\0') {
            return Err(SecurityError::InvalidPath("null byte in path".into()));
        }

        if input_path.is_empty() {
            return Err(SecurityError::InvalidPath("empty path".into()));
        }

        if self.permissive {
            let resolved = if input_path.starts_with('/') {
                PathBuf::from(input_path)
            } else {
                self.root_path.join(input_path)
            };
            let normalized = if resolved.exists() {
                std::fs::canonicalize(&resolved)?
            } else if let Some(parent) = resolved.parent() {
                if parent.exists() {
                    std::fs::canonicalize(parent)?.join(resolved.file_name().unwrap_or_default())
                } else {
                    normalize_path(&resolved)
                }
            } else {
                normalize_path(&resolved)
            };
            return Ok(SafePath::unchecked(Arc::clone(&self.root_fd), normalized));
        }

        let relative = if input_path.starts_with('/') {
            let input = PathBuf::from(input_path);
            let normalized_input = if input.exists() {
                std::fs::canonicalize(&input)?
            } else if let Some(parent) = input.parent() {
                if parent.exists() {
                    std::fs::canonicalize(parent)?.join(input.file_name().unwrap_or_default())
                } else {
                    normalize_path(&input)
                }
            } else {
                normalize_path(&input)
            };

            if normalized_input.starts_with(&self.root_path) {
                normalized_input
                    .strip_prefix(&self.root_path)
                    .map(|p| p.to_path_buf())
                    .unwrap_or_default()
            } else {
                let mut found = None;
                for allowed in &self.allowed_paths {
                    if normalized_input.starts_with(allowed) {
                        found = Some(
                            normalized_input
                                .strip_prefix(allowed)
                                .map(|p| p.to_path_buf())
                                .unwrap_or_default(),
                        );
                        break;
                    }
                }
                match found {
                    Some(rel) => rel,
                    None => return Err(SecurityError::PathEscape(normalized_input)),
                }
            }
        } else {
            normalize_path(&PathBuf::from(input_path))
                .strip_prefix("/")
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|_| PathBuf::from(input_path))
        };

        let expected_path = self.root_path.join(&relative);
        if self.is_path_denied(&expected_path) {
            return Err(SecurityError::DeniedPath(expected_path));
        }

        let safe_path = SafePath::resolve(
            Arc::clone(&self.root_fd),
            self.root_path.clone(),
            &relative,
            self.max_symlink_depth,
        )?;

        let resolved = safe_path.as_path();
        if !self.is_within(resolved) {
            return Err(SecurityError::PathEscape(resolved.to_path_buf()));
        }
        if self.is_path_denied(resolved) {
            return Err(SecurityError::DeniedPath(resolved.to_path_buf()));
        }

        Ok(safe_path)
    }

    pub fn resolve_with_limits(
        &self,
        input_path: &str,
        limits: &ToolLimits,
    ) -> Result<SafePath, SecurityError> {
        let path = self.resolve(input_path)?;
        let full_path = path.as_path();
        let path_str = full_path.to_string_lossy();

        if let Some(ref allowed) = limits.allowed_paths
            && !allowed.is_empty()
        {
            let allowed_patterns: Vec<Pattern> = allowed
                .iter()
                .filter_map(|p| Pattern::new(p).ok())
                .collect();
            if !allowed_patterns.iter().any(|p| p.matches(&path_str)) {
                return Err(SecurityError::DeniedPath(full_path.to_path_buf()));
            }
        }

        if let Some(ref denied) = limits.denied_paths {
            let denied_patterns: Vec<Pattern> =
                denied.iter().filter_map(|p| Pattern::new(p).ok()).collect();
            if denied_patterns.iter().any(|p| p.matches(&path_str)) {
                return Err(SecurityError::DeniedPath(full_path.to_path_buf()));
            }
        }

        Ok(path)
    }

    pub fn open_read(&self, input_path: &str) -> Result<SecureFileHandle, SecurityError> {
        let path = self.resolve(input_path)?;
        SecureFileHandle::open_read(path)
    }

    pub fn open_write(&self, input_path: &str) -> Result<SecureFileHandle, SecurityError> {
        let path = self.resolve(input_path)?;
        SecureFileHandle::open_write(path)
    }

    pub fn is_within(&self, path: &Path) -> bool {
        if self.permissive {
            return true;
        }

        let canonical = self.resolve_to_canonical(path);
        canonical.starts_with(&self.root_path)
            || self.allowed_paths.iter().any(|p| canonical.starts_with(p))
    }

    fn resolve_to_canonical(&self, path: &Path) -> PathBuf {
        if let Ok(p) = std::fs::canonicalize(path) {
            return p;
        }

        let mut current = path.to_path_buf();
        let mut components_to_append = Vec::new();

        while let Some(parent) = current.parent() {
            if let Ok(canonical_parent) = std::fs::canonicalize(parent) {
                let mut result = canonical_parent;
                if let Some(name) = current.file_name() {
                    result = result.join(name);
                }
                for component in components_to_append.into_iter().rev() {
                    result = result.join(component);
                }
                return result;
            }
            if let Some(name) = current.file_name() {
                components_to_append.push(name.to_os_string());
            }
            current = parent.to_path_buf();
        }

        normalize_path(path)
    }

    fn is_path_denied(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy();
        self.denied_patterns
            .iter()
            .any(|pattern| pattern.matches(&path_str))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_secure_fs_new() {
        let dir = tempdir().unwrap();
        let fs = SecureFs::new(dir.path(), vec![], &[], 10).unwrap();
        assert_eq!(fs.root(), std::fs::canonicalize(dir.path()).unwrap());
    }

    #[test]
    fn test_resolve_valid_path() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        fs::write(root.join("test.txt"), "content").unwrap();

        let secure_fs = SecureFs::new(&root, vec![], &[], 10).unwrap();
        let path = secure_fs.resolve("test.txt").unwrap();
        assert_eq!(path.as_path(), root.join("test.txt"));
    }

    #[test]
    fn test_resolve_path_escape_blocked() {
        let dir = tempdir().unwrap();
        let secure_fs = SecureFs::new(dir.path(), vec![], &[], 10).unwrap();
        let result = secure_fs.resolve("../../../etc/passwd");
        assert!(matches!(result, Err(SecurityError::PathEscape(_))));
    }

    #[test]
    fn test_denied_patterns() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        fs::write(root.join("secret.key"), "secret").unwrap();

        let patterns = vec!["*.key".to_string()];
        let secure_fs = SecureFs::new(&root, vec![], &patterns, 10).unwrap();
        let result = secure_fs.resolve("secret.key");
        assert!(matches!(result, Err(SecurityError::DeniedPath(_))));
    }

    #[test]
    fn test_allowed_paths() {
        let dir1 = tempdir().unwrap();
        let dir2 = tempdir().unwrap();
        let root1 = std::fs::canonicalize(dir1.path()).unwrap();
        let root2 = std::fs::canonicalize(dir2.path()).unwrap();
        fs::write(root2.join("file.txt"), "content").unwrap();

        let secure_fs = SecureFs::new(&root1, vec![root2.clone()], &[], 10).unwrap();
        assert!(secure_fs.is_within(&root2.join("file.txt")));
    }
}
