//! Routing Strategy for Skill Discovery
//!
//! Determines how skills are discovered and activated based on user input.

use serde::{Deserialize, Serialize};

use crate::common::IndexRegistry;
use crate::skills::{
    SkillIndex, find_explicit_command, find_trigger_matches, list_model_invocable_skills,
};

/// Strategy for routing user input to skills
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RoutingStrategy {
    /// Explicit skill invocation via slash command (/skill-name)
    Explicit {
        /// The matched skill name
        skill_name: String,
    },

    /// Keyword-based matching using trigger words
    KeywordMatch {
        /// Matched trigger keywords
        matched_triggers: Vec<String>,
        /// Candidate skill names
        skill_names: Vec<String>,
    },

    /// Semantic matching (delegated to Claude)
    ///
    /// When no explicit or keyword match is found, Claude uses
    /// the skill index summaries to decide.
    Semantic {
        /// Confidence score (0.0 to 1.0)
        confidence: f32,
    },

    /// No skill routing needed
    NoSkill,
}

impl RoutingStrategy {
    /// Check if this is an explicit invocation
    pub fn is_explicit(&self) -> bool {
        matches!(self, Self::Explicit { .. })
    }

    /// Check if any skill was matched
    pub fn has_skill(&self) -> bool {
        !matches!(self, Self::NoSkill)
    }

    /// Get the primary skill name if available
    pub fn primary_skill(&self) -> Option<&str> {
        match self {
            Self::Explicit { skill_name } => Some(skill_name),
            Self::KeywordMatch { skill_names, .. } => skill_names.first().map(|s| s.as_str()),
            Self::Semantic { .. } => None,
            Self::NoSkill => None,
        }
    }
}

/// Route user input to determine skill discovery strategy.
pub fn route(input: &str, skill_indices: &[SkillIndex]) -> RoutingStrategy {
    // Explicit slash commands are a user choice and stay routable even when the
    // skill is hidden from model-driven discovery.
    let mut registry = IndexRegistry::new();
    registry.register_all(skill_indices.iter().cloned());

    if let Some(skill) = find_explicit_command(&registry, input) {
        return RoutingStrategy::Explicit {
            skill_name: skill.name.clone(),
        };
    }

    let model_visible_skills = list_model_invocable_skills(&registry);

    // 2. Check for trigger keyword matches
    let matches = find_trigger_matches(&registry, input);

    if !matches.is_empty() {
        let matched_triggers: Vec<String> = matches
            .iter()
            .flat_map(|s| {
                s.triggers
                    .iter()
                    .filter(|t| input.to_lowercase().contains(&t.to_lowercase()))
                    .cloned()
            })
            .collect();

        let skill_names: Vec<String> = matches.iter().map(|s| s.name.clone()).collect();

        return RoutingStrategy::KeywordMatch {
            matched_triggers,
            skill_names,
        };
    }

    // 3. Fall back to semantic matching (Claude will decide)
    if !model_visible_skills.is_empty() {
        RoutingStrategy::Semantic { confidence: 0.0 }
    } else {
        RoutingStrategy::NoSkill
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::SourceType;

    fn test_skills() -> Vec<SkillIndex> {
        vec![
            SkillIndex::new("commit", "Create a git commit")
                .triggers(["git commit", "commit changes"])
                .source_type(SourceType::User),
            SkillIndex::new("review", "Review code changes")
                .triggers(["code review", "review pr"])
                .source_type(SourceType::Project),
        ]
    }

    #[test]
    fn test_explicit_routing() {
        let skills = test_skills();

        let result = route("/commit", &skills);
        assert!(
            matches!(result, RoutingStrategy::Explicit { skill_name } if skill_name == "commit")
        );
    }

    #[test]
    fn test_keyword_routing() {
        let skills = test_skills();

        let result = route("I want to git commit these changes", &skills);
        assert!(matches!(result, RoutingStrategy::KeywordMatch { .. }));

        if let RoutingStrategy::KeywordMatch { skill_names, .. } = result {
            assert!(skill_names.contains(&"commit".to_string()));
        }
    }

    #[test]
    fn test_no_match_semantic() {
        let skills = test_skills();

        let result = route("help me with this bug", &skills);
        assert!(matches!(result, RoutingStrategy::Semantic { .. }));
    }

    #[test]
    fn test_no_skill_empty_index() {
        let skills: Vec<SkillIndex> = vec![];

        let result = route("anything", &skills);
        assert!(matches!(result, RoutingStrategy::NoSkill));
    }

    #[test]
    fn test_explicit_takes_precedence() {
        let skills = test_skills();

        // Explicit command works even if keywords also match
        let result = route("/commit", &skills);
        assert!(matches!(result, RoutingStrategy::Explicit { .. }));

        // Keywords match when no explicit command
        let result = route("git commit these changes", &skills);
        assert!(matches!(result, RoutingStrategy::KeywordMatch { .. }));
    }

    #[test]
    fn test_manual_only_skill_is_still_explicitly_routable() {
        let mut skills = test_skills();
        let mut internal = SkillIndex::new("internal", "Manual-only internal workflow");
        internal.disable_model_invocation = true;
        skills.push(internal);

        let result = route("/internal", &skills);
        assert!(
            matches!(result, RoutingStrategy::Explicit { skill_name } if skill_name == "internal")
        );

        let result = route("internal workflow", &skills);
        assert!(!matches!(result, RoutingStrategy::KeywordMatch { .. }));
    }

    #[test]
    fn test_routing_strategy_methods() {
        let explicit = RoutingStrategy::Explicit {
            skill_name: "test".to_string(),
        };
        assert!(explicit.is_explicit());
        assert!(explicit.has_skill());
        assert_eq!(explicit.primary_skill(), Some("test"));

        let no_skill = RoutingStrategy::NoSkill;
        assert!(!no_skill.is_explicit());
        assert!(!no_skill.has_skill());
        assert_eq!(no_skill.primary_skill(), None);
    }
}
