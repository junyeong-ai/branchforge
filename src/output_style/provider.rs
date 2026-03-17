//! Output style provider implementations using common infrastructure.

use super::OutputStyle;
use crate::common::InMemoryProvider as GenericInMemoryProvider;

pub type InMemoryOutputStyleProvider = GenericInMemoryProvider<OutputStyle>;

#[cfg(feature = "file-resources")]
pub type FileOutputStyleProvider = crate::common::FileProvider<
    OutputStyle,
    super::OutputStyleLoader,
    crate::common::OutputStyleLookupStrategy,
>;

#[cfg(feature = "file-resources")]
pub type ChainOutputStyleProvider = crate::common::ChainProvider<OutputStyle>;

#[cfg(feature = "file-resources")]
pub fn file_output_style_provider() -> FileOutputStyleProvider {
    FileOutputStyleProvider::new(
        super::OutputStyleLoader::new(),
        crate::common::OutputStyleLookupStrategy,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::Provider;

    #[tokio::test]
    async fn test_in_memory_provider() {
        let style = OutputStyle::new("test", "Test style", "Do something");
        let provider = InMemoryOutputStyleProvider::new().item(style);

        let names = provider.list().await.unwrap();
        assert_eq!(names, vec!["test"]);

        let loaded = provider.get("test").await.unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().name, "test");
    }

    #[cfg(feature = "file-resources")]
    #[tokio::test]
    async fn test_chain_provider() {
        let low = InMemoryOutputStyleProvider::new()
            .item(OutputStyle::new("shared", "Low", "Low content"))
            .priority(0);

        let high = InMemoryOutputStyleProvider::new()
            .item(OutputStyle::new("shared", "High", "High content"))
            .priority(10);

        let chain = ChainOutputStyleProvider::new().provider(low).provider(high);

        let style = chain.get("shared").await.unwrap().unwrap();
        assert_eq!(style.description, "High");
    }

    #[cfg(feature = "file-resources")]
    #[tokio::test]
    async fn test_file_provider() {
        use crate::common::{Provider, SourceType};

        let temp = tempfile::tempdir().unwrap();
        let provider = file_output_style_provider()
            .path(temp.path())
            .priority(5)
            .source_type(SourceType::Project);

        assert_eq!(Provider::priority(&provider), 5);
        assert_eq!(provider.paths().len(), 1);
    }

    #[cfg(feature = "file-resources")]
    #[tokio::test]
    async fn test_file_provider_load_style() {
        let temp = tempfile::tempdir().unwrap();
        let style_file = temp.path().join("custom.md");
        tokio::fs::write(
            &style_file,
            r#"---
name: custom-style
description: A custom style
---

Custom content here.
"#,
        )
        .await
        .unwrap();

        let provider = file_output_style_provider().path(temp.path());

        let style = provider.get("custom").await.unwrap();
        assert!(style.is_some());
        assert_eq!(style.unwrap().name, "custom-style");
    }
}
