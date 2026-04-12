#![allow(missing_docs)]

use std::path::{Path, PathBuf};

use super::PluginError;
use super::manifest::{PLUGIN_CONFIG_DIR, PluginDescriptor, PluginManifest};

pub struct PluginDiscovery;

impl PluginDiscovery {
    /// Returns the default plugins directory: `~/.claude/plugins/`.
    pub fn default_plugins_dir() -> Option<PathBuf> {
        crate::common::home_dir().map(|home| home.join(".claude").join("plugins"))
    }

    pub fn discover(dirs: &[PathBuf]) -> Result<Vec<PluginDescriptor>, PluginError> {
        let mut descriptors = Vec::new();

        for dir in dirs {
            if !dir.exists() {
                continue;
            }

            if Self::is_plugin_root(dir) {
                let manifest = PluginManifest::load(dir)?;
                descriptors.push(PluginDescriptor::new(manifest, dir.clone()));
            } else {
                Self::scan_children(dir, &mut descriptors)?;
            }
        }

        Ok(descriptors)
    }

    fn is_plugin_root(dir: &Path) -> bool {
        dir.join(PLUGIN_CONFIG_DIR).is_dir()
    }

    fn scan_children(
        parent: &Path,
        descriptors: &mut Vec<PluginDescriptor>,
    ) -> Result<(), PluginError> {
        let entries = std::fs::read_dir(parent)?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() && Self::is_plugin_root(&path) {
                let manifest = PluginManifest::load(&path)?;
                descriptors.push(PluginDescriptor::new(manifest, path));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn create_plugin(parent: &Path, name: &str) -> PathBuf {
        let plugin_dir = parent.join(name);
        let config_dir = plugin_dir.join(PLUGIN_CONFIG_DIR);
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("plugin.json"),
            format!(
                r#"{{"name":"{}","description":"Test","version":"1.0.0"}}"#,
                name
            ),
        )
        .unwrap();
        plugin_dir
    }

    #[test]
    fn test_discover_direct_plugin_root() {
        let dir = tempdir().unwrap();
        let plugin_dir = create_plugin(dir.path(), "my-plugin");

        let descriptors = PluginDiscovery::discover(&[plugin_dir]).unwrap();
        assert_eq!(descriptors.len(), 1);
        assert_eq!(descriptors[0].name(), "my-plugin");
    }

    #[test]
    fn test_discover_parent_directory() {
        let dir = tempdir().unwrap();
        create_plugin(dir.path(), "plugin-a");
        create_plugin(dir.path(), "plugin-b");

        let descriptors = PluginDiscovery::discover(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(descriptors.len(), 2);
        let names: Vec<&str> = descriptors.iter().map(|d| d.name()).collect();
        assert!(names.contains(&"plugin-a"));
        assert!(names.contains(&"plugin-b"));
    }

    #[test]
    fn test_discover_nonexistent_dir() {
        let descriptors = PluginDiscovery::discover(&[PathBuf::from("/nonexistent/path")]).unwrap();
        assert!(descriptors.is_empty());
    }

    #[test]
    fn test_discover_empty_dir() {
        let dir = tempdir().unwrap();
        let descriptors = PluginDiscovery::discover(&[dir.path().to_path_buf()]).unwrap();
        assert!(descriptors.is_empty());
    }

    #[test]
    fn test_discover_mixed_dirs() {
        let dir = tempdir().unwrap();
        create_plugin(dir.path(), "real-plugin");
        // non-plugin subdirectory
        std::fs::create_dir(dir.path().join("not-a-plugin")).unwrap();

        let descriptors = PluginDiscovery::discover(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(descriptors.len(), 1);
        assert_eq!(descriptors[0].name(), "real-plugin");
    }

    #[test]
    fn test_discover_multiple_dirs() {
        let dir1 = tempdir().unwrap();
        let dir2 = tempdir().unwrap();
        create_plugin(dir1.path(), "p1");
        create_plugin(dir2.path(), "p2");

        let descriptors =
            PluginDiscovery::discover(&[dir1.path().to_path_buf(), dir2.path().to_path_buf()])
                .unwrap();
        assert_eq!(descriptors.len(), 2);
    }

    #[test]
    fn test_default_plugins_dir() {
        let dir = PluginDiscovery::default_plugins_dir();
        assert!(dir.is_some());
        let path = dir.unwrap();
        assert!(path.ends_with(".claude/plugins"));
    }
}
