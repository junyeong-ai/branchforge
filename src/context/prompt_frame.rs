use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptFrame {
    pub base: String,
    pub context_sections: Vec<String>,
    pub output_contract: Option<String>,
}

impl PromptFrame {
    pub fn render(&self) -> String {
        let mut parts = Vec::new();
        if !self.base.is_empty() {
            parts.push(self.base.clone());
        }
        parts.extend(self.context_sections.iter().cloned());
        if let Some(ref contract) = self.output_contract {
            parts.push(contract.clone());
        }
        parts.join("\n\n")
    }
}
