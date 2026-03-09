use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::types::{BranchId, NodeId, NodeKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventMetadata {
    pub id: Uuid,
    pub occurred_at: DateTime<Utc>,
    pub actor: Option<String>,
}

impl EventMetadata {
    pub fn new(actor: Option<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            occurred_at: Utc::now(),
            actor,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphEvent {
    pub metadata: EventMetadata,
    pub body: GraphEventBody,
}

impl GraphEvent {
    pub fn new(body: GraphEventBody) -> Self {
        Self {
            metadata: EventMetadata::new(None),
            body,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GraphEventBody {
    NodeAppended {
        node_id: NodeId,
        branch_id: BranchId,
        parent_id: Option<NodeId>,
        kind: NodeKind,
        tags: Vec<String>,
        payload: serde_json::Value,
    },
    BranchForked {
        branch_id: BranchId,
        name: String,
        forked_from: Option<NodeId>,
    },
    CheckpointCreated {
        checkpoint_id: NodeId,
        branch_id: BranchId,
        label: String,
        note: Option<String>,
        tags: Vec<String>,
    },
}
