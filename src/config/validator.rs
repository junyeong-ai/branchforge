//! Configuration Validation Layer
//!
//! Validates configuration values before use.

#![allow(missing_docs)]

use std::collections::HashMap;
use std::ops::RangeInclusive;

use serde_json::Value;

use super::{ConfigError, ConfigResult, ValidationErrors};

pub type ValidationFn = Box<dyn Fn(&Value) -> Result<(), String> + Send + Sync>;

pub struct ConfigValidator {
    required_keys: Vec<String>,
    type_rules: HashMap<String, ValueType>,
    range_rules: HashMap<String, RangeInclusive<i64>>,
    pattern_rules: HashMap<String, regex::Regex>,
    custom_rules: HashMap<String, ValidationFn>,
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    String,
    Number,
    Boolean,
    Array,
    Object,
}

impl ValueType {
    fn matches(&self, value: &Value) -> bool {
        match self {
            ValueType::String => value.is_string(),
            ValueType::Number => value.is_number(),
            ValueType::Boolean => value.is_boolean(),
            ValueType::Array => value.is_array(),
            ValueType::Object => value.is_object(),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            ValueType::String => "string",
            ValueType::Number => "number",
            ValueType::Boolean => "boolean",
            ValueType::Array => "array",
            ValueType::Object => "object",
        }
    }
}

impl ConfigValidator {
    pub fn new() -> Self {
        Self {
            required_keys: Vec::new(),
            type_rules: HashMap::new(),
            range_rules: HashMap::new(),
            pattern_rules: HashMap::new(),
            custom_rules: HashMap::new(),
        }
    }

    pub fn require(mut self, key: impl Into<String>) -> Self {
        self.required_keys.push(key.into());
        self
    }

    pub fn require_many(mut self, keys: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.required_keys.extend(keys.into_iter().map(Into::into));
        self
    }

    pub fn expect_type(mut self, key: impl Into<String>, value_type: ValueType) -> Self {
        self.type_rules.insert(key.into(), value_type);
        self
    }

    pub fn expect_range(mut self, key: impl Into<String>, range: RangeInclusive<i64>) -> Self {
        self.range_rules.insert(key.into(), range);
        self
    }

    pub fn expect_pattern(mut self, key: impl Into<String>, pattern: &str) -> ConfigResult<Self> {
        let key = key.into();
        let regex = regex::Regex::new(pattern).map_err(|e| ConfigError::InvalidValue {
            key: key.clone(),
            message: format!("Invalid regex pattern: {}", e),
        })?;
        self.pattern_rules.insert(key, regex);
        Ok(self)
    }

    pub fn custom<F>(mut self, key: impl Into<String>, validator: F) -> Self
    where
        F: Fn(&Value) -> Result<(), String> + Send + Sync + 'static,
    {
        self.custom_rules.insert(key.into(), Box::new(validator));
        self
    }

    pub fn validate(&self, config: &Value) -> ConfigResult<()> {
        let errors = self.collect_errors(config);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::ValidationErrors(ValidationErrors(errors)))
        }
    }

    pub fn validate_partial(&self, config: &Value) -> Vec<ConfigError> {
        self.collect_errors(config)
    }

    fn collect_errors(&self, config: &Value) -> Vec<ConfigError> {
        let mut errors = Vec::new();

        for key in &self.required_keys {
            if get_nested(config, key).is_none() {
                errors.push(ConfigError::NotFound { key: key.clone() });
            }
        }

        for (key, expected_type) in &self.type_rules {
            if let Some(value) = get_nested(config, key)
                && !expected_type.matches(value)
            {
                errors.push(ConfigError::InvalidValue {
                    key: key.clone(),
                    message: format!(
                        "expected {}, got {}",
                        expected_type.name(),
                        value_type_name(value)
                    ),
                });
            }
        }

        for (key, range) in &self.range_rules {
            if let Some(value) = get_nested(config, key)
                && let Some(num) = value.as_i64()
                && !range.contains(&num)
            {
                errors.push(ConfigError::InvalidValue {
                    key: key.clone(),
                    message: format!(
                        "value {} not in range {}..={}",
                        num,
                        range.start(),
                        range.end()
                    ),
                });
            }
        }

        for (key, pattern) in &self.pattern_rules {
            if let Some(value) = get_nested(config, key)
                && let Some(s) = value.as_str()
                && !pattern.is_match(s)
            {
                errors.push(ConfigError::InvalidValue {
                    key: key.clone(),
                    message: format!("Value '{}' does not match pattern", s),
                });
            }
        }

        for (key, validator) in &self.custom_rules {
            if let Some(value) = get_nested(config, key)
                && let Err(msg) = validator(value)
            {
                errors.push(ConfigError::InvalidValue {
                    key: key.clone(),
                    message: msg,
                });
            }
        }

        errors
    }
}

impl Default for ConfigValidator {
    fn default() -> Self {
        Self::new()
    }
}

fn get_nested<'a>(config: &'a Value, key: &str) -> Option<&'a Value> {
    let parts: Vec<&str> = key.split('.').collect();
    let mut current = config;

    for part in parts {
        current = current.get(part)?;
    }

    Some(current)
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_required_keys() {
        let validator = ConfigValidator::new().require("api_key").require("model");

        let config = json!({
            "api_key": "sk-test",
            "model": "claude-sonnet-4-5"
        });
        assert!(validator.validate(&config).is_ok());

        let missing = json!({
            "api_key": "sk-test"
        });
        assert!(validator.validate(&missing).is_err());
    }

    #[test]
    fn test_type_validation() {
        let validator = ConfigValidator::new()
            .expect_type("port", ValueType::Number)
            .expect_type("enabled", ValueType::Boolean);

        let valid = json!({
            "port": 8080,
            "enabled": true
        });
        assert!(validator.validate(&valid).is_ok());

        let invalid = json!({
            "port": "8080",
            "enabled": true
        });
        assert!(validator.validate(&invalid).is_err());
    }

    #[test]
    fn test_range_validation() {
        let validator = ConfigValidator::new()
            .expect_range("port", 1..=65535)
            .expect_range("timeout", 1..=300);

        let valid = json!({
            "port": 8080,
            "timeout": 30
        });
        assert!(validator.validate(&valid).is_ok());

        let invalid = json!({
            "port": 70000,
            "timeout": 30
        });
        assert!(validator.validate(&invalid).is_err());
    }

    #[test]
    fn test_pattern_validation() {
        let validator = ConfigValidator::new()
            .expect_pattern("api_key", r"^sk-[a-zA-Z0-9]+$")
            .unwrap();

        let valid = json!({
            "api_key": "sk-test123"
        });
        assert!(validator.validate(&valid).is_ok());

        let invalid = json!({
            "api_key": "invalid-key"
        });
        assert!(validator.validate(&invalid).is_err());
    }

    #[test]
    fn test_nested_keys() {
        let validator = ConfigValidator::new()
            .require("database.host")
            .expect_type("database.port", ValueType::Number);

        let config = json!({
            "database": {
                "host": "localhost",
                "port": 5432
            }
        });
        assert!(validator.validate(&config).is_ok());
    }

    #[test]
    fn test_custom_validator() {
        let validator = ConfigValidator::new().custom("urls", |v| {
            if let Some(arr) = v.as_array()
                && arr.is_empty()
            {
                return Err("urls cannot be empty".to_string());
            }
            Ok(())
        });

        let valid = json!({
            "urls": ["http://example.com"]
        });
        assert!(validator.validate(&valid).is_ok());

        let invalid = json!({
            "urls": []
        });
        assert!(validator.validate(&invalid).is_err());
    }

    #[test]
    fn test_require_many() {
        let validator = ConfigValidator::new().require_many(["host", "port", "database"]);

        let config = json!({
            "host": "localhost",
            "port": 5432,
            "database": "mydb"
        });
        assert!(validator.validate(&config).is_ok());
    }

    #[test]
    fn test_validate_partial() {
        let validator = ConfigValidator::new()
            .require("a")
            .require("b")
            .require("c");

        let config = json!({
            "a": 1
        });

        let errors = validator.validate_partial(&config);
        assert_eq!(errors.len(), 2);
    }
}
