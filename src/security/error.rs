//! Security error types.

use std::path::PathBuf;
use thiserror::Error;

#[non_exhaustive]
#[derive(Debug, Error)]
pub enum SecurityError {
    #[error("path escapes sandbox: {0}")]
    PathEscape(PathBuf),

    #[error("absolute symlink outside sandbox: {0}")]
    AbsoluteSymlink(PathBuf),

    #[error("symlink depth exceeded (max {max}): {path}")]
    SymlinkDepthExceeded { path: PathBuf, max: u8 },

    #[error("invalid path: {0}")]
    InvalidPath(String),

    #[error("denied path: {0}")]
    DeniedPath(PathBuf),

    #[error("bash command blocked: {0}")]
    BashBlocked(String),

    #[error("path not within sandbox: {0}")]
    NotWithinSandbox(PathBuf),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("resource limit error: {0}")]
    ResourceLimit(String),
}
