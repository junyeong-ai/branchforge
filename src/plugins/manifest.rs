#![allow(missing_docs)]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::PluginError;

pub(super) const PLUGIN_CONFIG_DIR: &str = ".claude-plugin";
const PLUGIN_MANIFEST_FILE: &str = "plugin.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginAuthor {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub description: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<PluginAuthor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
}

impl PluginManifest {
    pub fn load(root_dir: &Path) -> Result<Self, PluginError> {
        let manifest_path = root_dir.join(PLUGIN_CONFIG_DIR).join(PLUGIN_MANIFEST_FILE);
        if !manifest_path.exists() {
            return Err(PluginError::ManifestNotFound {
                path: manifest_path,
            });
        }
        let content = std::fs::read_to_string(&manifest_path)?;
        serde_json::from_str(&content).map_err(|e| PluginError::InvalidManifest {
            path: manifest_path,
            reason: e.to_string(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct PluginDescriptor {
    pub(crate) manifest: PluginManifest,
    pub(crate) root_dir: PathBuf,
}

impl PluginDescriptor {
    pub(crate) fn new(manifest: PluginManifest, root_dir: PathBuf) -> Self {
        Self { manifest, root_dir }
    }

    pub fn name(&self) -> &str {
        &self.manifest.name
    }

    pub fn version(&self) -> &str {
        &self.manifest.version
    }

    pub fn description(&self) -> &str {
        &self.manifest.description
    }

    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.root_dir.join("skills")
    }

    pub fn commands_dir(&self) -> PathBuf {
        self.root_dir.join("commands")
    }

    pub fn agents_dir(&self) -> PathBuf {
        self.root_dir.join("agents")
    }

    pub fn hooks_config_path(&self) -> PathBuf {
        self.root_dir.join("hooks").join("hooks.json")
    }

    pub fn mcp_config_path(&self) -> PathBuf {
        self.root_dir.join(".mcp.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_manifest_load() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join(PLUGIN_CONFIG_DIR);
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join(PLUGIN_MANIFEST_FILE),
            r#"{"name":"test-plugin","description":"A test plugin","version":"1.0.0"}"#,
        )
        .unwrap();

        let manifest = PluginManifest::load(dir.path()).unwrap();
        assert_eq!(manifest.name, "test-plugin");
        assert_eq!(manifest.description, "A test plugin");
        assert_eq!(manifest.version, "1.0.0");
        assert!(manifest.author.is_none());
    }

    #[test]
    fn test_manifest_load_with_author() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join(PLUGIN_CONFIG_DIR);
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join(PLUGIN_MANIFEST_FILE),
            r#"{
                "name": "authored",
                "description": "Has author",
                "version": "0.1.0",
                "author": {"name": "Alice", "email": "alice@example.com"}
            }"#,
        )
        .unwrap();

        let manifest = PluginManifest::load(dir.path()).unwrap();
        let author = manifest.author.unwrap();
        assert_eq!(author.name, "Alice");
        assert_eq!(author.email.as_deref(), Some("alice@example.com"));
    }

    #[test]
    fn test_manifest_not_found() {
        let dir = tempdir().unwrap();
        let err = PluginManifest::load(dir.path()).unwrap_err();
        assert!(matches!(err, PluginError::ManifestNotFound { .. }));
    }

    #[test]
    fn test_manifest_invalid_json() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join(PLUGIN_CONFIG_DIR);
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join(PLUGIN_MANIFEST_FILE), "not json").unwrap();

        let err = PluginManifest::load(dir.path()).unwrap_err();
        assert!(matches!(err, PluginError::InvalidManifest { .. }));
    }

    #[test]
    fn test_manifest_missing_required_fields() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join(PLUGIN_CONFIG_DIR);
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join(PLUGIN_MANIFEST_FILE),
            r#"{"name":"incomplete"}"#,
        )
        .unwrap();

        let err = PluginManifest::load(dir.path()).unwrap_err();
        assert!(matches!(err, PluginError::InvalidManifest { .. }));
    }

    #[test]
    fn test_descriptor_paths() {
        let descriptor = PluginDescriptor::new(
            PluginManifest {
                name: "my-plugin".into(),
                description: "desc".into(),
                version: "1.0.0".into(),
                author: None,
                homepage: None,
                repository: None,
                license: None,
                keywords: Vec::new(),
            },
            PathBuf::from("/plugins/my-plugin"),
        );

        assert_eq!(descriptor.name(), "my-plugin");
        assert_eq!(
            descriptor.skills_dir(),
            PathBuf::from("/plugins/my-plugin/skills")
        );
        assert_eq!(
            descriptor.commands_dir(),
            PathBuf::from("/plugins/my-plugin/commands")
        );
        assert_eq!(
            descriptor.agents_dir(),
            PathBuf::from("/plugins/my-plugin/agents")
        );
        assert_eq!(
            descriptor.hooks_config_path(),
            PathBuf::from("/plugins/my-plugin/hooks/hooks.json")
        );
        assert_eq!(
            descriptor.mcp_config_path(),
            PathBuf::from("/plugins/my-plugin/.mcp.json")
        );
    }

    #[test]
    fn test_manifest_serde_roundtrip() {
        let manifest = PluginManifest {
            name: "roundtrip".into(),
            description: "Test roundtrip".into(),
            version: "2.0.0".into(),
            author: Some(PluginAuthor {
                name: "Bob".into(),
                email: None,
                url: Some("https://example.com".into()),
            }),
            homepage: None,
            repository: Some("https://github.com/bob/roundtrip".into()),
            license: Some("MIT".into()),
            keywords: vec!["test".into(), "roundtrip".into()],
        };

        let json = serde_json::to_string(&manifest).unwrap();
        let parsed: PluginManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "roundtrip");
        assert_eq!(parsed.version, "2.0.0");
        assert!(parsed.author.unwrap().url.is_some());
    }
}
