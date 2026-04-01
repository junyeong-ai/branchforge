//! File-based credential storage.

use std::path::PathBuf;

use directories::BaseDirs;

use super::CliCredentials;
use crate::Result;

const CLAUDE_DIR: &str = ".claude";
const CREDENTIALS_FILE: &str = ".credentials.json";

/// File system credential storage.
pub struct FileStorage;

impl FileStorage {
    fn credentials_path() -> Option<PathBuf> {
        BaseDirs::new().map(|dirs| dirs.home_dir().join(CLAUDE_DIR).join(CREDENTIALS_FILE))
    }

    /// Save credentials to file using atomic write (temp + rename).
    pub async fn save(credentials: &CliCredentials) -> Result<()> {
        let path = Self::credentials_path()
            .ok_or_else(|| crate::Error::auth("Cannot determine credentials path"))?;

        let content = serde_json::to_string_pretty(credentials)
            .map_err(|e| crate::Error::auth(format!("Failed to serialize credentials: {}", e)))?;

        // Atomic write: write to temp file, then rename
        let tmp = path.with_extension("tmp");
        tokio::fs::write(&tmp, &content)
            .await
            .map_err(|e| crate::Error::auth(format!("Failed to write credentials: {}", e)))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .await
                .map_err(|e| {
                    crate::Error::auth(format!("Failed to set credentials permissions: {}", e))
                })?;
        }

        tokio::fs::rename(&tmp, &path)
            .await
            .map_err(|e| crate::Error::auth(format!("Failed to finalize credentials: {}", e)))?;

        Ok(())
    }

    /// Load credentials from file.
    pub async fn load() -> Result<Option<CliCredentials>> {
        let Some(path) = Self::credentials_path() else {
            return Ok(None);
        };

        if !path.exists() {
            return Ok(None);
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = tokio::fs::metadata(&path).await {
                let mode = metadata.permissions().mode();
                if mode & 0o077 != 0 {
                    tracing::warn!(
                        "Credentials file {:?} has overly permissive permissions {:o}, expected 0600",
                        path,
                        mode & 0o777
                    );
                }
            }
        }

        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| crate::Error::auth(format!("Failed to read credentials file: {}", e)))?;

        let creds: CliCredentials = serde_json::from_str(&content)
            .map_err(|e| crate::Error::auth(format!("Failed to parse credentials: {}", e)))?;

        Ok(Some(creds))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_credentials_path() {
        let path = FileStorage::credentials_path();
        assert!(path.is_some());
        let path = path.unwrap();
        assert!(path.to_string_lossy().contains(".claude"));
    }
}
