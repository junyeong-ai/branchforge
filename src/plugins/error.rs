use std::path::PathBuf;

#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("Plugin manifest not found: {path}")]
    ManifestNotFound { path: PathBuf },

    #[error("Invalid plugin manifest at {path}: {reason}")]
    InvalidManifest { path: PathBuf, reason: String },

    #[error("Duplicate plugin name '{name}': first at {first}, second at {second}")]
    DuplicateName {
        name: String,
        first: PathBuf,
        second: PathBuf,
    },

    #[error("Invalid plugin name '{name}': {reason}")]
    InvalidName { name: String, reason: String },

    #[error("Failed to load resources for plugin '{plugin}': {message}")]
    ResourceLoad { plugin: String, message: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = PluginError::ManifestNotFound {
            path: PathBuf::from("/plugins/test"),
        };
        assert!(err.to_string().contains("/plugins/test"));

        let err = PluginError::InvalidManifest {
            path: PathBuf::from("/plugins/bad"),
            reason: "missing name".into(),
        };
        assert!(err.to_string().contains("missing name"));

        let err = PluginError::DuplicateName {
            name: "my-plugin".into(),
            first: PathBuf::from("/plugins/first"),
            second: PathBuf::from("/plugins/second"),
        };
        let msg = err.to_string();
        assert!(msg.contains("my-plugin"));
        assert!(msg.contains("first"));
        assert!(msg.contains("second"));

        let err = PluginError::InvalidName {
            name: "bad:name".into(),
            reason: "contains namespace separator".into(),
        };
        assert!(err.to_string().contains("bad:name"));
    }

    #[test]
    fn test_io_error_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let plugin_err: PluginError = io_err.into();
        assert!(matches!(plugin_err, PluginError::Io(_)));
    }

    #[test]
    fn test_json_error_conversion() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid").unwrap_err();
        let plugin_err: PluginError = json_err.into();
        assert!(matches!(plugin_err, PluginError::Json(_)));
    }
}
