use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type SessionGraphId = Uuid;
pub type NodeId = Uuid;
pub type BranchId = Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeProvenance {
    pub source_session_id: String,
    pub session_type: String,
    pub task_id: Option<String>,
    pub subagent_session_id: Option<String>,
    pub subagent_type: Option<String>,
    pub subagent_description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    User,
    Assistant,
    ToolCall,
    ToolResult,
    Summary,
    Plan,
    Todo,
    Checkpoint,
    Branch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: NodeId,
    pub branch_id: BranchId,
    pub kind: NodeKind,
    pub parent_id: Option<NodeId>,
    pub created_by_principal_id: Option<String>,
    pub provenance: Option<NodeProvenance>,
    pub created_at: DateTime<Utc>,
    pub tags: Vec<String>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branch {
    pub id: BranchId,
    pub name: String,
    pub forked_from: Option<NodeId>,
    pub created_at: DateTime<Utc>,
    pub head: Option<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: NodeId,
    pub branch_id: BranchId,
    pub label: String,
    pub note: Option<String>,
    pub tags: Vec<String>,
    pub created_by_principal_id: Option<String>,
    pub provenance: Option<NodeProvenance>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: Uuid,
    pub node_id: NodeId,
    pub branch_id: BranchId,
    pub label: String,
    pub note: Option<String>,
    pub created_by_principal_id: Option<String>,
    pub provenance: Option<NodeProvenance>,
    pub created_at: DateTime<Utc>,
}
