//! Execution modes for controlling tool execution behavior.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// Plan mode tools — only these are allowed when in Plan mode.
const PLAN_TOOLS: &[&str] = &["Read", "Glob", "Grep", "Plan", "TodoWrite", "GraphHistory"];

/// Controls how tools are executed — automatic, supervised, or exploration-only.
///
/// # Modes
///
/// - **Auto**: Tools execute automatically when policy allows (default).
/// - **Plan**: Exploration only — only read/navigation tools execute. Write/execute tools are blocked.
/// - **Supervised**: All tools require user review before execution.
/// - **SupervisedFor(set)**: Only the specified tools require user review; others execute automatically.
///
/// # Example
///
/// ```rust
/// use branchforge::authorization::ExecutionMode;
///
/// let mode = ExecutionMode::Auto;
/// assert!(!mode.requires_review("Read"));
/// assert!(mode.allows_tool("Read"));
///
/// let mode = ExecutionMode::Plan;
/// assert!(mode.is_plan());
/// assert!(mode.allows_tool("Read"));
/// assert!(!mode.allows_tool("Write"));
/// ```
#[derive(Clone, Debug, Default)]
pub enum ExecutionMode {
    /// Tools execute automatically when policy allows (default).
    #[default]
    Auto,
    /// Exploration only — only read/navigation tools execute. Write/execute tools are blocked.
    Plan,
    /// All tools require user review before execution.
    Supervised,
    /// Only the specified tools require user review; others execute automatically.
    SupervisedFor(HashSet<String>),
}

impl ExecutionMode {
    /// Returns true if the given tool requires user review before execution.
    pub fn requires_review(&self, tool_name: &str) -> bool {
        match self {
            ExecutionMode::Auto => false,
            ExecutionMode::Plan => false,
            ExecutionMode::Supervised => true,
            ExecutionMode::SupervisedFor(set) => set.contains(tool_name),
        }
    }

    /// Returns true if this mode is `Plan`.
    pub fn is_plan(&self) -> bool {
        matches!(self, ExecutionMode::Plan)
    }

    /// Returns true if the given tool is allowed under this execution mode.
    ///
    /// In `Plan` mode, only tools in the `PLAN_TOOLS` list are allowed.
    /// All other modes delegate to policy.
    pub fn allows_tool(&self, tool_name: &str) -> bool {
        match self {
            ExecutionMode::Plan => PLAN_TOOLS.contains(&tool_name),
            _ => true,
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            ExecutionMode::Auto => "Automatic tool execution",
            ExecutionMode::Plan => "Exploration only (read/navigation tools)",
            ExecutionMode::Supervised => "All tools require review",
            ExecutionMode::SupervisedFor(_) => "Specified tools require review",
        }
    }
}

impl std::fmt::Display for ExecutionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecutionMode::Auto => write!(f, "auto"),
            ExecutionMode::Plan => write!(f, "plan"),
            ExecutionMode::Supervised => write!(f, "supervised"),
            ExecutionMode::SupervisedFor(set) => {
                write!(
                    f,
                    "supervisedFor({})",
                    set.iter().cloned().collect::<Vec<_>>().join(",")
                )
            }
        }
    }
}

impl std::str::FromStr for ExecutionMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "auto" => Ok(ExecutionMode::Auto),
            "plan" => Ok(ExecutionMode::Plan),
            "supervised" => Ok(ExecutionMode::Supervised),
            _ => Err(format!("Unknown execution mode: {}", s)),
        }
    }
}

impl Serialize for ExecutionMode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            ExecutionMode::Auto => serializer.serialize_str("auto"),
            ExecutionMode::Plan => serializer.serialize_str("plan"),
            ExecutionMode::Supervised => serializer.serialize_str("supervised"),
            ExecutionMode::SupervisedFor(set) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("supervisedFor", set)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ExecutionMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;

        struct ExecutionModeVisitor;

        impl<'de> de::Visitor<'de> for ExecutionModeVisitor {
            type Value = ExecutionMode;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str(
                    "a string (\"auto\", \"plan\", \"supervised\") or a map with \"supervisedFor\"",
                )
            }

            fn visit_str<E>(self, value: &str) -> Result<ExecutionMode, E>
            where
                E: de::Error,
            {
                match value.to_lowercase().as_str() {
                    "auto" => Ok(ExecutionMode::Auto),
                    "plan" => Ok(ExecutionMode::Plan),
                    "supervised" => Ok(ExecutionMode::Supervised),
                    _ => Err(de::Error::unknown_variant(
                        value,
                        &["auto", "plan", "supervised"],
                    )),
                }
            }

            fn visit_map<M>(self, mut map: M) -> Result<ExecutionMode, M::Error>
            where
                M: de::MapAccess<'de>,
            {
                let key: String = map
                    .next_key()?
                    .ok_or_else(|| de::Error::missing_field("supervisedFor"))?;
                if key == "supervisedFor" {
                    let tools: HashSet<String> = map.next_value()?;
                    Ok(ExecutionMode::SupervisedFor(tools))
                } else {
                    Err(de::Error::unknown_field(&key, &["supervisedFor"]))
                }
            }
        }

        deserializer.deserialize_any(ExecutionModeVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_mode() {
        let mode = ExecutionMode::default();
        assert!(!mode.requires_review("Read"));
        assert!(!mode.is_plan());
        assert!(mode.allows_tool("Read"));
        assert!(mode.allows_tool("Bash"));
    }

    #[test]
    fn test_plan_mode() {
        let mode = ExecutionMode::Plan;
        assert!(mode.is_plan());
        assert!(!mode.requires_review("Read"));
        assert!(mode.allows_tool("Read"));
        assert!(mode.allows_tool("Glob"));
        assert!(mode.allows_tool("Grep"));
        assert!(!mode.allows_tool("Write"));
        assert!(!mode.allows_tool("Bash"));
    }

    #[test]
    fn test_supervised_mode() {
        let mode = ExecutionMode::Supervised;
        assert!(mode.requires_review("Read"));
        assert!(mode.requires_review("Bash"));
        assert!(mode.allows_tool("Read"));
        assert!(mode.allows_tool("Bash"));
    }

    #[test]
    fn test_supervised_for_mode() {
        let set = HashSet::from(["Bash".to_string(), "Write".to_string()]);
        let mode = ExecutionMode::SupervisedFor(set);
        assert!(mode.requires_review("Bash"));
        assert!(mode.requires_review("Write"));
        assert!(!mode.requires_review("Read"));
        assert!(mode.allows_tool("Read"));
        assert!(mode.allows_tool("Bash"));
    }

    #[test]
    fn test_display() {
        assert_eq!(ExecutionMode::Auto.to_string(), "auto");
        assert_eq!(ExecutionMode::Plan.to_string(), "plan");
        assert_eq!(ExecutionMode::Supervised.to_string(), "supervised");
    }

    #[test]
    fn test_from_str() {
        assert!(matches!(
            "auto".parse::<ExecutionMode>().unwrap(),
            ExecutionMode::Auto
        ));
        assert!(matches!(
            "plan".parse::<ExecutionMode>().unwrap(),
            ExecutionMode::Plan
        ));
        assert!(matches!(
            "supervised".parse::<ExecutionMode>().unwrap(),
            ExecutionMode::Supervised
        ));
    }

    #[test]
    fn test_serde_auto() {
        let mode = ExecutionMode::Auto;
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(json, "\"auto\"");
        let parsed: ExecutionMode = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, ExecutionMode::Auto));
    }

    #[test]
    fn test_serde_supervised_for() {
        let set = HashSet::from(["Bash".to_string()]);
        let mode = ExecutionMode::SupervisedFor(set);
        let json = serde_json::to_string(&mode).unwrap();
        assert!(json.contains("supervisedFor"));
        let parsed: ExecutionMode = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, ExecutionMode::SupervisedFor(_)));
    }
}
