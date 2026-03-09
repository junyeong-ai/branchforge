use serde::{Deserialize, Serialize};

use crate::graph::{BranchId, NodeId, NodeKind, SessionGraph};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunDescriptor {
    pub model: String,
    pub provider: String,
    pub prompt_hash: Option<String>,
    pub context_hash: Option<String>,
}

impl RunDescriptor {
    pub fn new(model: impl Into<String>, provider: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            provider: provider.into(),
            prompt_hash: None,
            context_hash: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeEventRecorder {
    pub run: RunDescriptor,
    pub branch_id: BranchId,
}

impl RuntimeEventRecorder {
    pub fn new(run: RunDescriptor, branch_id: BranchId) -> Self {
        Self { run, branch_id }
    }

    pub fn record_message(
        &self,
        graph: &mut SessionGraph,
        kind: NodeKind,
        payload: serde_json::Value,
    ) -> NodeId {
        graph.append_node(self.branch_id, kind, payload)
    }
}
