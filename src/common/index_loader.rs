//! Shared directory scanning utilities for index loaders.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

#[non_exhaustive]
pub enum DirAction {
    Recurse,
    LoadFile(std::path::PathBuf),
}

/// Read a file and parse it via the provided closure.
pub async fn load_file<T, F>(path: &Path, parse: F, type_name: &str) -> crate::Result<T>
where
    F: FnOnce(&str, &Path) -> crate::Result<T>,
{
    let content = tokio::fs::read_to_string(path).await.map_err(|e| {
        crate::Error::Config(format!(
            "Failed to read {} file {:?}: {}",
            type_name, path, e
        ))
    })?;
    parse(&content, path)
}

/// Scan a directory for index entries, returning empty vec if directory doesn't exist.
pub async fn scan_directory<T, F, Filter, DirHandler>(
    dir: &Path,
    load_fn: F,
    file_filter: Filter,
    dir_handler: DirHandler,
) -> crate::Result<Vec<T>>
where
    T: Send,
    F: for<'a> Fn(&'a Path) -> Pin<Box<dyn Future<Output = crate::Result<T>> + Send + 'a>>
        + Send
        + Sync,
    Filter: Fn(&Path) -> bool + Send + Sync,
    DirHandler: Fn(&Path) -> DirAction + Send + Sync,
{
    let mut indices = Vec::new();
    if !dir.exists() {
        return Ok(indices);
    }
    scan_recursive(dir, &mut indices, &load_fn, &file_filter, &dir_handler).await?;
    Ok(indices)
}

fn scan_recursive<'a, T, F, Filter, DirHandler>(
    dir: &'a Path,
    indices: &'a mut Vec<T>,
    load_fn: &'a F,
    file_filter: &'a Filter,
    dir_handler: &'a DirHandler,
) -> Pin<Box<dyn Future<Output = crate::Result<()>> + Send + 'a>>
where
    T: Send,
    F: Fn(&Path) -> Pin<Box<dyn Future<Output = crate::Result<T>> + Send + '_>> + Send + Sync,
    Filter: Fn(&Path) -> bool + Send + Sync,
    DirHandler: Fn(&Path) -> DirAction + Send + Sync,
{
    Box::pin(async move {
        let mut entries = tokio::fs::read_dir(dir).await.map_err(|e| {
            crate::Error::Config(format!("Failed to read directory {:?}: {}", dir, e))
        })?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| crate::Error::Config(format!("Failed to read directory entry: {}", e)))?
        {
            let path = entry.path();

            if path.is_dir() {
                match dir_handler(&path) {
                    DirAction::Recurse => {
                        scan_recursive(&path, indices, load_fn, file_filter, dir_handler).await?;
                    }
                    DirAction::LoadFile(file_path) => {
                        if let Ok(index) = load_fn(&file_path).await {
                            indices.push(index);
                        }
                    }
                }
            } else if file_filter(&path) {
                match load_fn(&path).await {
                    Ok(index) => indices.push(index),
                    Err(e) => {
                        tracing::warn!("Failed to load index from {:?}: {}", path, e);
                    }
                }
            }
        }

        Ok(())
    })
}
