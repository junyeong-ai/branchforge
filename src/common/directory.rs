use std::path::Path;
#[cfg(feature = "file-resources")]
use std::path::PathBuf;

#[cfg(feature = "file-resources")]
pub async fn load_files<T, F, Fut>(
    dir: &Path,
    filter: impl Fn(&Path) -> bool,
    loader: F,
) -> crate::Result<Vec<T>>
where
    F: Fn(PathBuf) -> Fut,
    Fut: std::future::Future<Output = crate::Result<T>>,
{
    let mut items = Vec::new();

    if !dir.exists() {
        return Ok(items);
    }

    let mut entries = tokio::fs::read_dir(dir).await.map_err(|e| {
        crate::Error::Config(format!("Failed to read directory {}: {}", dir.display(), e))
    })?;

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| crate::Error::Config(format!("Failed to read directory entry: {}", e)))?
    {
        let path = entry.path();
        if filter(&path) {
            match loader(path.clone()).await {
                Ok(item) => items.push(item),
                Err(e) => tracing::warn!("Failed to load {}: {}", path.display(), e),
            }
        }
    }

    Ok(items)
}

#[cfg(feature = "file-resources")]
pub fn is_markdown(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "md")
}

pub fn is_skill_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("SKILL.md") || name.ends_with(".skill.md"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "file-resources")]
    fn test_is_markdown() {
        assert!(is_markdown(Path::new("file.md")));
        assert!(is_markdown(Path::new("/path/to/file.md")));
        assert!(!is_markdown(Path::new("file.txt")));
        assert!(!is_markdown(Path::new("file")));
    }

    #[test]
    fn test_is_skill_file() {
        assert!(is_skill_file(Path::new("SKILL.md")));
        assert!(is_skill_file(Path::new("skill.md"))); // case insensitive
        assert!(is_skill_file(Path::new("commit.skill.md")));
        assert!(!is_skill_file(Path::new("README.md")));
        assert!(!is_skill_file(Path::new("file.md")));
    }

    #[tokio::test]
    #[cfg(feature = "file-resources")]
    async fn test_load_files_empty_dir() {
        let result = load_files(
            Path::new("/nonexistent/path"),
            |_| true,
            |_| async { Ok::<_, crate::Error>(()) },
        )
        .await
        .unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    #[cfg(feature = "file-resources")]
    async fn test_load_files_from_temp() {
        let temp = tempfile::tempdir().unwrap();
        let file1 = temp.path().join("test.md");
        let file2 = temp.path().join("test.txt");

        tokio::fs::write(&file1, "content1").await.unwrap();
        tokio::fs::write(&file2, "content2").await.unwrap();

        let result: Vec<PathBuf> = load_files(temp.path(), is_markdown, |p| async move { Ok(p) })
            .await
            .unwrap();

        assert_eq!(result.len(), 1);
        assert!(result[0].ends_with("test.md"));
    }
}
