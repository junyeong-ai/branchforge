//! Tool input extraction for authorization pattern matching.

use serde_json::Value;

/// Extracts a matchable string from a tool's JSON input for authorization checks.
pub trait InputExtractor: Send + Sync {
    fn extract<'a>(&self, input: &'a Value) -> Option<&'a str>;
}

/// Extracts a string value from a specific JSON field.
pub struct FieldExtractor(pub &'static str);

impl InputExtractor for FieldExtractor {
    fn extract<'a>(&self, input: &'a Value) -> Option<&'a str> {
        input.get(self.0).and_then(|v| v.as_str())
    }
}

/// Returns the default extractors for built-in tools.
pub fn default_extractors() -> Vec<(&'static str, std::sync::Arc<dyn InputExtractor>)> {
    vec![
        ("Bash", std::sync::Arc::new(FieldExtractor("command"))),
        ("Skill", std::sync::Arc::new(FieldExtractor("skill"))),
        ("Read", std::sync::Arc::new(FieldExtractor("file_path"))),
        ("Write", std::sync::Arc::new(FieldExtractor("file_path"))),
        ("Edit", std::sync::Arc::new(FieldExtractor("file_path"))),
        ("Glob", std::sync::Arc::new(FieldExtractor("path"))),
        ("Grep", std::sync::Arc::new(FieldExtractor("path"))),
    ]
}
