//! Sandbox error types.

#![allow(missing_docs)]

use std::io;
use std::path::PathBuf;

use thiserror::Error;

#[non_exhaustive]
#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("sandbox not supported on this platform")]
    NotSupported,

    #[error("sandbox not available: {0}")]
    NotAvailable(String),

    #[error("failed to create sandbox: {0}")]
    Creation(String),

    #[error("failed to apply sandbox rules: {0}")]
    RuleApplication(String),

    #[error("path not accessible: {}", .0.display())]
    PathNotAccessible(PathBuf),

    #[error("invalid sandbox configuration: {0}")]
    InvalidConfig(String),

    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

pub type SandboxResult<T> = Result<T, SandboxError>;
