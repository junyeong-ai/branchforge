#![allow(missing_docs)]

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::GraphError;
use super::event::{EventMetadata, GraphEvent, GraphEventBody};
use super::types::{
    Bookmark, Branch, BranchId, Checkpoint, GraphNode, NodeId, NodeKind, NodeProvenance,
    SessionGraphId,
};
use crate::events::EventBus;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionGraph {
    pub(crate) id: SessionGraphId,
    pub(crate) created_at: chrono::DateTime<Utc>,
    pub(crate) events: Vec<GraphEvent>,
    pub(crate) branches: HashMap<BranchId, Branch>,
    pub(crate) nodes: HashMap<NodeId, GraphNode>,
    pub(crate) checkpoints: HashMap<NodeId, Checkpoint>,
    pub(crate) bookmarks: HashMap<super::BookmarkId, Bookmark>,
    pub(crate) primary_branch: BranchId,
    /// Nodes on the primary branch before this watermark are "archived" --
    /// skipped in message projection but preserved for referential integrity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) archived_watermark: Option<NodeId>,
    #[serde(skip)]
    pub(crate) event_bus: Option<Arc<EventBus>>,
}

impl SessionGraph {
    // ── Read-only accessors ──────────────────────────────────────────

    pub fn id(&self) -> SessionGraphId {
        self.id
    }

    pub fn created_at(&self) -> chrono::DateTime<Utc> {
        self.created_at
    }

    pub fn events(&self) -> &[GraphEvent] {
        &self.events
    }

    pub fn branches(&self) -> &HashMap<BranchId, Branch> {
        &self.branches
    }

    pub fn nodes(&self) -> &HashMap<NodeId, GraphNode> {
        &self.nodes
    }

    pub fn checkpoints(&self) -> &HashMap<NodeId, Checkpoint> {
        &self.checkpoints
    }

    pub fn bookmarks(&self) -> &HashMap<super::BookmarkId, Bookmark> {
        &self.bookmarks
    }

    pub fn primary_branch(&self) -> BranchId {
        self.primary_branch
    }

    /// Returns the current archived watermark, if set.
    ///
    /// Nodes on the primary branch whose `created_at` is strictly before
    /// the watermark node's `created_at` are considered archived and should
    /// be skipped in message projection.
    pub fn archived_watermark(&self) -> Option<NodeId> {
        self.archived_watermark
    }

    // ── Archival ─────────────────────────────────────────────────────

    /// Maximum number of walk-back iterations when adjusting the
    /// archive watermark to preserve tool-pair integrity.
    ///
    /// The algorithm converges in at most one step per distinct
    /// `tool_call_id` in the graph, so this bound only trips on
    /// pathological inputs. It exists so the loop cannot deadlock.
    pub const MAX_WATERMARK_WALKBACK: usize = 256;

    /// Mark all primary-branch nodes before `watermark` as archived.
    ///
    /// Archived nodes remain in the graph (preserving `parent_id` chains
    /// and checkpoint/bookmark references) but are skipped by the session
    /// layer's message projection (`Session::current_branch_messages()`).
    ///
    /// # Tool-pair integrity
    ///
    /// If the requested watermark would fall *between* a `ToolCall`
    /// content part and its matching `ToolResult`, the watermark is
    /// **walked back** until every visible `ToolResult` on the primary
    /// branch has its matching `ToolCall` visible as well. This protects
    /// downstream providers (OpenAI Chat Completions, Gemini
    /// `generateContent`) from receiving an orphaned `tool` role message
    /// and returning a `400 invalid_request` error — the production bug
    /// that `claw-code` documented as
    /// [compact.rs:121-159](https://github.com/ultraworkers/claw-code).
    ///
    /// The walk-back is bounded by [`SessionGraph::MAX_WATERMARK_WALKBACK`] iterations.
    /// If the bound is exceeded (pathological graph with thousands of
    /// interleaved tool pairs), the method returns
    /// [`GraphError::WatermarkUnresolvable`] and leaves the graph's
    /// existing watermark untouched — archival failure is always safer
    /// than corrupting projection.
    ///
    /// Returns the number of primary-branch nodes that precede the
    /// **effective** watermark (after walk-back adjustment), i.e. the
    /// archived count.
    pub fn archive_before(&mut self, watermark: NodeId) -> Result<usize, GraphError> {
        if !self.nodes.contains_key(&watermark) {
            return Err(GraphError::MissingNode { node_id: watermark });
        }

        let effective = self.tool_pair_adjusted_watermark(watermark)?;
        let primary = self.primary_branch;
        let watermark_created_at = self.nodes[&effective].created_at;
        let count = self
            .nodes
            .values()
            .filter(|n| n.branch_id == primary && n.created_at < watermark_created_at)
            .count();

        self.archived_watermark = Some(effective);
        self.events.push(GraphEvent::with_metadata(
            EventMetadata::new(None),
            GraphEventBody::EventsArchived {
                watermark_node_id: effective,
                archived_count: count,
            },
        ));
        Ok(count)
    }

    /// Adjust `desired` backward so no visible `ToolResult` ends up
    /// orphaned from its matching `ToolCall`.
    ///
    /// Algorithm:
    ///
    /// 1. Build a `tool_call_id → (call_node, result_node)` index across
    ///    all primary-branch nodes. (Independent of the current watermark.)
    /// 2. Starting from `desired`, loop: find every pair where
    ///    `call.created_at < current_wm AND result.created_at >= current_wm`
    ///    — these are the crossing pairs that would orphan a `ToolResult`.
    ///    If there are none, `current_wm` is the answer. Otherwise, move
    ///    the watermark backward to the earliest `call.created_at` across
    ///    all crossing pairs. That folds every currently-crossing pair
    ///    back into the visible set; widening the visible set may expose
    ///    new `ToolResult`s whose matching `ToolCall`s sit even earlier,
    ///    so re-check.
    /// 3. Bounded by [`MAX_WATERMARK_WALKBACK`] iterations.
    ///
    /// This helper is the only place tool-pair integrity is enforced;
    /// the `GraphValidator::validate` invariant `archived_watermark_*`
    /// confirms the outcome after the fact but does not repair it.
    fn tool_pair_adjusted_watermark(&self, desired: NodeId) -> Result<NodeId, GraphError> {
        use crate::ir::ContentPart;
        use std::collections::HashMap;

        let primary = self.primary_branch;

        // Walk all primary-branch nodes in chronological order and
        // extract every ToolCall / ToolResult by `tool_call_id`.
        struct PairEndpoint {
            node_id: NodeId,
            created_at: chrono::DateTime<chrono::Utc>,
        }
        let mut calls: HashMap<String, PairEndpoint> = HashMap::new();
        let mut results: HashMap<String, PairEndpoint> = HashMap::new();

        for node in self.nodes.values().filter(|n| n.branch_id == primary) {
            let Some(content_value) = node.payload.get("content") else {
                continue;
            };
            let Ok(parts) = serde_json::from_value::<Vec<ContentPart>>(content_value.clone())
            else {
                continue;
            };
            for part in parts {
                match part {
                    ContentPart::ToolCall { id, .. } => {
                        calls.insert(
                            id,
                            PairEndpoint {
                                node_id: node.id,
                                created_at: node.created_at,
                            },
                        );
                    }
                    ContentPart::ToolResult { tool_call_id, .. } => {
                        results.insert(
                            tool_call_id,
                            PairEndpoint {
                                node_id: node.id,
                                created_at: node.created_at,
                            },
                        );
                    }
                    _ => {}
                }
            }
        }

        // Pair table: (call_time, result_time, call_node_id) for every
        // tool call that has a matching result on the same branch.
        // Orphaned results without a matching call are ignored — the
        // graph is already broken in a different way and the walker
        // cannot fix it.
        let pairs: Vec<(
            chrono::DateTime<chrono::Utc>,
            chrono::DateTime<chrono::Utc>,
            NodeId,
        )> = calls
            .into_iter()
            .filter_map(|(id, call)| {
                results
                    .remove(&id)
                    .map(|result| (call.created_at, result.created_at, call.node_id))
            })
            .collect();

        // Iterative walk-back.
        let mut current = desired;
        for _ in 0..Self::MAX_WATERMARK_WALKBACK {
            let current_ts = self.nodes[&current].created_at;

            // Find the earliest call_time among crossing pairs.
            let earliest_crossing_call = pairs
                .iter()
                .filter(|(call_ts, result_ts, _)| *call_ts < current_ts && *result_ts >= current_ts)
                .map(|(_, _, call_node)| *call_node)
                .min_by_key(|node| self.nodes[node].created_at);

            match earliest_crossing_call {
                None => return Ok(current), // fixed point reached
                Some(call_node) => current = call_node,
            }
        }

        Err(GraphError::WatermarkUnresolvable {
            desired_watermark: desired,
            walkback_limit: Self::MAX_WATERMARK_WALKBACK,
        })
    }

    // ── Incremental event application ─────────────────────────────────

    /// Apply a single [`GraphEvent`] in-place without rebuilding the whole graph.
    ///
    /// This is O(1) per event (hash-map inserts/lookups) vs the O(n)
    /// full-rebuild path through `GraphMaterializer::from_events`.
    pub fn apply_event(&mut self, event: &GraphEvent) {
        match &event.body {
            GraphEventBody::NodeAppended {
                node_id,
                branch_id,
                parent_id,
                kind,
                tags,
                payload,
                provenance,
            } => {
                self.nodes.insert(
                    *node_id,
                    GraphNode {
                        id: *node_id,
                        branch_id: *branch_id,
                        kind: *kind,
                        parent_id: *parent_id,
                        created_by_principal_id: event.metadata.actor.clone(),
                        provenance: provenance.clone(),
                        created_at: event.metadata.occurred_at,
                        tags: tags.clone(),
                        payload: payload.clone(),
                    },
                );
                if let Some(branch) = self.branches.get_mut(branch_id) {
                    branch.head = Some(*node_id);
                }
            }
            GraphEventBody::BranchForked {
                branch_id,
                name,
                forked_from,
            } => {
                self.branches.insert(
                    *branch_id,
                    Branch {
                        id: *branch_id,
                        name: name.clone(),
                        forked_from: *forked_from,
                        created_at: event.metadata.occurred_at,
                        head: *forked_from,
                    },
                );
            }
            GraphEventBody::CheckpointCreated {
                checkpoint_id,
                branch_id,
                label,
                note,
                tags,
                provenance,
            } => {
                let parent_id = self.branches.get(branch_id).and_then(|b| b.head);
                self.checkpoints.insert(
                    *checkpoint_id,
                    Checkpoint {
                        id: *checkpoint_id,
                        branch_id: *branch_id,
                        label: label.clone(),
                        note: note.clone(),
                        tags: tags.clone(),
                        created_by_principal_id: event.metadata.actor.clone(),
                        provenance: provenance.clone(),
                        created_at: event.metadata.occurred_at,
                    },
                );
                self.nodes.insert(
                    *checkpoint_id,
                    GraphNode {
                        id: *checkpoint_id,
                        branch_id: *branch_id,
                        kind: NodeKind::Checkpoint,
                        parent_id,
                        created_by_principal_id: event.metadata.actor.clone(),
                        provenance: provenance.clone(),
                        created_at: event.metadata.occurred_at,
                        tags: tags.clone(),
                        payload: serde_json::json!({
                            "label": label,
                            "note": note,
                        }),
                    },
                );
                if let Some(branch) = self.branches.get_mut(branch_id) {
                    branch.head = Some(*checkpoint_id);
                }
            }
            GraphEventBody::BookmarkCreated {
                bookmark_id,
                node_id,
                branch_id,
                label,
                note,
                provenance,
            } => {
                self.bookmarks.insert(
                    *bookmark_id,
                    Bookmark {
                        id: *bookmark_id,
                        node_id: *node_id,
                        branch_id: *branch_id,
                        label: label.clone(),
                        note: note.clone(),
                        created_by_principal_id: event.metadata.actor.clone(),
                        provenance: provenance.clone(),
                        created_at: event.metadata.occurred_at,
                    },
                );
            }
            GraphEventBody::NodeMetadataPatched { node_id, metadata } => {
                if let Some(node) = self.nodes.get_mut(node_id) {
                    if let Some(payload) = node.payload.as_object_mut() {
                        payload.insert("metadata".to_string(), metadata.clone());
                    } else {
                        node.payload = serde_json::json!({ "metadata": metadata });
                    }
                }
            }
            GraphEventBody::EventsArchived {
                watermark_node_id, ..
            } => {
                self.archived_watermark = Some(*watermark_node_id);
            }
        }
    }

    // ── Mutators ─────────────────────────────────────────────────────

    /// Attach an [`EventBus`] for non-blocking observability events.
    pub fn with_event_bus(&mut self, bus: Arc<EventBus>) {
        self.event_bus = Some(bus);
    }

    /// Walk up the parent chain from a node to compute its depth in the tree.
    ///
    /// If a cycle is detected in the parent chain, traversal stops early to
    /// avoid infinite loops.
    pub fn node_depth(&self, node_id: NodeId) -> usize {
        let mut depth = 0;
        let mut visited = std::collections::HashSet::new();
        visited.insert(node_id);
        let mut current = self.nodes.get(&node_id).and_then(|n| n.parent_id);
        while let Some(id) = current {
            if !visited.insert(id) {
                break; // cycle detected
            }
            depth += 1;
            current = self.nodes.get(&id).and_then(|n| n.parent_id);
        }
        depth
    }
}

impl Default for SessionGraph {
    fn default() -> Self {
        Self::new("main")
    }
}

impl SessionGraph {
    pub(crate) fn branch_lineage_node_ids(
        &self,
        branch_id: BranchId,
    ) -> Result<Vec<NodeId>, GraphError> {
        let branch = self
            .branches
            .get(&branch_id)
            .ok_or(GraphError::MissingBranch { branch_id })?;
        let Some(head) = branch.head else {
            return Ok(Vec::new());
        };

        let mut nodes = Vec::new();
        let mut current = Some(head);
        while let Some(node_id) = current {
            let node = self
                .nodes
                .get(&node_id)
                .ok_or(GraphError::MissingNode { node_id })?;
            nodes.push(node_id);
            current = node.parent_id;
        }
        nodes.reverse();
        Ok(nodes)
    }

    fn validated_branch_head(
        &self,
        branch_id: BranchId,
        node_id: NodeId,
    ) -> Result<Option<NodeId>, GraphError> {
        let branch = self
            .branches
            .get(&branch_id)
            .ok_or(GraphError::MissingBranch { branch_id })?;
        let Some(parent_id) = branch.head else {
            return Ok(None);
        };
        let parent = self
            .nodes
            .get(&parent_id)
            .ok_or(GraphError::MissingParent { node_id, parent_id })?;
        if parent.branch_id != branch_id && branch.forked_from != Some(parent_id) {
            return Err(GraphError::ParentBranchMismatch {
                node_id,
                branch_id,
                parent_id,
                parent_branch_id: parent.branch_id,
            });
        }
        Ok(Some(parent_id))
    }

    pub fn new(primary_branch_name: impl Into<String>) -> Self {
        let branch_id = BranchId::new();
        let now = Utc::now();
        let branch = Branch {
            id: branch_id,
            name: primary_branch_name.into(),
            forked_from: None,
            created_at: now,
            head: None,
        };

        Self {
            id: SessionGraphId::new(),
            created_at: now,
            events: Vec::new(),
            branches: [(branch_id, branch)].into_iter().collect(),
            nodes: HashMap::new(),
            checkpoints: HashMap::new(),
            bookmarks: HashMap::new(),
            primary_branch: branch_id,
            archived_watermark: None,
            event_bus: None,
        }
    }

    pub fn append_node(
        &mut self,
        branch_id: BranchId,
        kind: NodeKind,
        payload: serde_json::Value,
    ) -> Result<NodeId, GraphError> {
        self.append_node_with_actor(branch_id, kind, payload, None, None)
    }

    pub fn append_node_with_actor(
        &mut self,
        branch_id: BranchId,
        kind: NodeKind,
        payload: serde_json::Value,
        created_by_principal_id: Option<String>,
        provenance: Option<NodeProvenance>,
    ) -> Result<NodeId, GraphError> {
        let node_id = NodeId::new();
        let parent_id = self.validated_branch_head(branch_id, node_id)?;
        self.append_existing_node(
            branch_id,
            node_id,
            parent_id,
            kind,
            Vec::new(),
            payload,
            Utc::now(),
            created_by_principal_id,
            provenance,
        )?;
        Ok(node_id)
    }

    pub fn append_existing_node(
        &mut self,
        branch_id: BranchId,
        node_id: NodeId,
        parent_id: Option<NodeId>,
        kind: NodeKind,
        tags: Vec<String>,
        payload: serde_json::Value,
        created_at: chrono::DateTime<Utc>,
        created_by_principal_id: Option<String>,
        provenance: Option<NodeProvenance>,
    ) -> Result<(), GraphError> {
        let branch = self
            .branches
            .get(&branch_id)
            .ok_or(GraphError::MissingBranch { branch_id })?;
        if self.nodes.contains_key(&node_id) {
            return Err(GraphError::DuplicateNode { node_id });
        }
        if let Some(parent_id) = parent_id {
            let parent = self
                .nodes
                .get(&parent_id)
                .ok_or(GraphError::MissingParent { node_id, parent_id })?;
            if parent.branch_id != branch_id && branch.forked_from != Some(parent_id) {
                return Err(GraphError::ParentBranchMismatch {
                    node_id,
                    branch_id,
                    parent_id,
                    parent_branch_id: parent.branch_id,
                });
            }
        }
        let node = GraphNode {
            id: node_id,
            branch_id,
            kind,
            parent_id,
            created_by_principal_id: created_by_principal_id.clone(),
            provenance,
            created_at,
            tags: tags.clone(),
            payload: payload.clone(),
        };
        self.nodes.insert(node_id, node);
        if let Some(branch) = self.branches.get_mut(&branch_id) {
            branch.head = Some(node_id);
        }
        self.events.push(GraphEvent {
            metadata: super::EventMetadata {
                id: Uuid::new_v4(),
                occurred_at: created_at,
                actor: created_by_principal_id,
            },
            body: GraphEventBody::NodeAppended {
                node_id,
                branch_id,
                parent_id,
                kind,
                tags,
                payload,
                provenance: self
                    .nodes
                    .get(&node_id)
                    .and_then(|node| node.provenance.clone()),
            },
        });
        Ok(())
    }

    pub fn patch_node_metadata(
        &mut self,
        node_id: NodeId,
        metadata: serde_json::Value,
        actor: Option<String>,
    ) -> bool {
        let Some(node) = self.nodes.get_mut(&node_id) else {
            return false;
        };

        if let Some(payload) = node.payload.as_object_mut() {
            payload.insert("metadata".to_string(), metadata.clone());
        } else {
            node.payload = serde_json::json!({ "metadata": metadata.clone() });
        }

        self.events.push(GraphEvent {
            metadata: super::EventMetadata::new(actor),
            body: GraphEventBody::NodeMetadataPatched { node_id, metadata },
        });

        true
    }

    pub fn fork_branch(
        &mut self,
        from_node: Option<NodeId>,
        name: impl Into<String>,
    ) -> Result<BranchId, GraphError> {
        if let Some(node_id) = from_node
            && !self.nodes.contains_key(&node_id)
        {
            return Err(GraphError::MissingForkSource { node_id });
        }
        let branch_id = BranchId::new();
        let branch = Branch {
            id: branch_id,
            name: name.into(),
            forked_from: from_node,
            created_at: Utc::now(),
            head: from_node,
        };
        self.branches.insert(branch_id, branch.clone());
        self.events
            .push(GraphEvent::new(GraphEventBody::BranchForked {
                branch_id,
                name: branch.name.clone(),
                forked_from: branch.forked_from,
            }));

        if let Some(ref bus) = self.event_bus {
            bus.emit_typed(crate::events::BranchForkedPayload {
                branch_id: branch_id.to_string(),
                name: branch.name.clone(),
                forked_from: branch.forked_from.map(|id| id.to_string()),
            });
        }

        Ok(branch_id)
    }

    pub fn create_checkpoint(
        &mut self,
        branch_id: BranchId,
        label: impl Into<String>,
        note: Option<String>,
        tags: Vec<String>,
        created_by_principal_id: Option<String>,
        provenance: Option<NodeProvenance>,
    ) -> Result<NodeId, GraphError> {
        if !self.branches.contains_key(&branch_id) {
            return Err(GraphError::MissingBranch { branch_id });
        }
        let checkpoint_id = NodeId::new();
        let parent_id = self.validated_branch_head(branch_id, checkpoint_id)?;
        let checkpoint = Checkpoint {
            id: checkpoint_id,
            branch_id,
            label: label.into(),
            note: note.clone(),
            tags: tags.clone(),
            created_by_principal_id: created_by_principal_id.clone(),
            provenance: provenance.clone(),
            created_at: Utc::now(),
        };
        self.checkpoints.insert(checkpoint_id, checkpoint.clone());
        self.nodes.insert(
            checkpoint_id,
            GraphNode {
                id: checkpoint_id,
                branch_id,
                kind: NodeKind::Checkpoint,
                parent_id,
                created_by_principal_id,
                provenance,
                created_at: checkpoint.created_at,
                tags: checkpoint.tags.clone(),
                payload: serde_json::json!({
                    "label": checkpoint.label,
                    "note": checkpoint.note,
                }),
            },
        );
        if let Some(branch) = self.branches.get_mut(&branch_id) {
            branch.head = Some(checkpoint_id);
        }
        let checkpoint_label = self
            .checkpoints
            .get(&checkpoint_id)
            .map(|c| c.label.clone())
            .unwrap_or_default();

        self.events.push(GraphEvent::with_metadata(
            EventMetadata {
                id: Uuid::new_v4(),
                occurred_at: checkpoint.created_at,
                actor: self
                    .checkpoints
                    .get(&checkpoint_id)
                    .and_then(|checkpoint| checkpoint.created_by_principal_id.clone()),
            },
            GraphEventBody::CheckpointCreated {
                checkpoint_id,
                branch_id,
                label: checkpoint_label.clone(),
                note,
                tags,
                provenance: self
                    .checkpoints
                    .get(&checkpoint_id)
                    .and_then(|checkpoint| checkpoint.provenance.clone()),
            },
        ));

        if let Some(ref bus) = self.event_bus {
            bus.emit_typed(crate::events::CheckpointCreatedPayload {
                checkpoint_id: checkpoint_id.to_string(),
                branch_id: branch_id.to_string(),
                label: checkpoint_label.clone(),
            });
        }

        Ok(checkpoint_id)
    }

    pub fn branch_head(&self, branch_id: BranchId) -> Option<NodeId> {
        self.branches.get(&branch_id).and_then(|branch| branch.head)
    }

    pub fn branch_ids(&self) -> Vec<BranchId> {
        let mut branch_ids: Vec<_> = self.branches.keys().copied().collect();
        branch_ids.sort_by_key(|branch_id| self.branches.get(branch_id).map(|b| b.created_at));
        branch_ids
    }

    pub fn children_of(&self, node_id: NodeId) -> Vec<&GraphNode> {
        let mut children: Vec<_> = self
            .nodes
            .values()
            .filter(|node| node.parent_id == Some(node_id))
            .collect();
        children.sort_by_key(|node| node.created_at);
        children
    }

    pub fn branch_at(&self, node_id: NodeId) -> Option<BranchId> {
        self.nodes.get(&node_id).map(|node| node.branch_id)
    }

    pub fn checkpoints_for_branch(&self, branch_id: BranchId) -> Vec<&Checkpoint> {
        let mut checkpoints: Vec<_> = self
            .checkpoints
            .values()
            .filter(|checkpoint| checkpoint.branch_id == branch_id)
            .collect();
        checkpoints.sort_by_key(|checkpoint| checkpoint.created_at);
        checkpoints
    }

    pub fn create_bookmark(
        &mut self,
        node_id: NodeId,
        label: impl Into<String>,
        note: Option<String>,
        created_by_principal_id: Option<String>,
        provenance: Option<NodeProvenance>,
    ) -> Result<super::BookmarkId, GraphError> {
        let node = self
            .nodes
            .get(&node_id)
            .ok_or(GraphError::MissingBookmarkTarget { node_id })?;
        if !self.branches.contains_key(&node.branch_id) {
            return Err(GraphError::MissingBranch {
                branch_id: node.branch_id,
            });
        }
        let bookmark_id = super::BookmarkId::new();
        self.bookmarks.insert(
            bookmark_id,
            Bookmark {
                id: bookmark_id,
                node_id,
                branch_id: node.branch_id,
                label: label.into(),
                note,
                created_by_principal_id,
                provenance,
                created_at: Utc::now(),
            },
        );
        if let Some(bookmark) = self.bookmarks.get(&bookmark_id) {
            self.events.push(GraphEvent::with_metadata(
                EventMetadata {
                    id: Uuid::new_v4(),
                    occurred_at: bookmark.created_at,
                    actor: bookmark.created_by_principal_id.clone(),
                },
                GraphEventBody::BookmarkCreated {
                    bookmark_id,
                    node_id,
                    branch_id: bookmark.branch_id,
                    label: bookmark.label.clone(),
                    note: bookmark.note.clone(),
                    provenance: bookmark.provenance.clone(),
                },
            ));
        }
        Ok(bookmark_id)
    }

    pub fn bookmarks_for_branch(&self, branch_id: BranchId) -> Vec<&Bookmark> {
        let mut bookmarks: Vec<_> = self
            .bookmarks
            .values()
            .filter(|bookmark| bookmark.branch_id == branch_id)
            .collect();
        bookmarks.sort_by_key(|bookmark| bookmark.created_at);
        bookmarks
    }

    pub fn replay_slice(&self, from: Option<NodeId>, branch_id: BranchId) -> Vec<&GraphNode> {
        let Ok(branch_ids) = self.branch_lineage_node_ids(branch_id) else {
            return Vec::new();
        };
        let start_index = match from {
            Some(from_id) => branch_ids
                .iter()
                .position(|node_id| *node_id == from_id)
                .unwrap_or(branch_ids.len()),
            None => 0,
        };

        branch_ids
            .into_iter()
            .skip(start_index)
            .filter_map(|node_id| self.nodes.get(&node_id))
            .collect()
    }

    pub fn branch_nodes(&self, branch_id: BranchId) -> Vec<&GraphNode> {
        let mut nodes: Vec<&GraphNode> = self
            .nodes
            .values()
            .filter(|node| node.branch_id == branch_id)
            .collect();
        nodes.sort_by_key(|node| node.created_at);
        nodes
    }

    pub fn current_branch_nodes(&self, branch_id: BranchId) -> Vec<&GraphNode> {
        self.branch_lineage_node_ids(branch_id)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|node_id| self.nodes.get(&node_id))
            .collect()
    }

    pub fn latest_summary(&self) -> Option<String> {
        self.current_branch_nodes(self.primary_branch)
            .into_iter()
            .rev()
            .find(|node| node.kind == NodeKind::Summary)
            .and_then(|node| {
                node.payload
                    .get("summary")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_nodes_to_branch_head() {
        let mut graph = SessionGraph::new("main");
        let branch = graph.primary_branch;
        let first = graph
            .append_node(branch, NodeKind::User, serde_json::json!({ "text": "hi" }))
            .unwrap();
        let second = graph
            .append_node(
                branch,
                NodeKind::Assistant,
                serde_json::json!({ "text": "hello" }),
            )
            .unwrap();

        assert_eq!(graph.branch_head(branch), Some(second));
        assert_eq!(
            graph.nodes.get(&second).and_then(|node| node.parent_id),
            Some(first)
        );
    }

    #[test]
    fn forks_new_branch_from_existing_node() {
        let mut graph = SessionGraph::default();
        let root = graph
            .append_node(
                graph.primary_branch,
                NodeKind::User,
                serde_json::json!({ "text": "root" }),
            )
            .unwrap();

        let branch = graph.fork_branch(Some(root), "experiment").unwrap();

        assert_eq!(graph.branch_head(branch), Some(root));
    }

    #[test]
    fn lists_children_and_checkpoints() {
        let mut graph = SessionGraph::default();
        let root = graph
            .append_node(graph.primary_branch, NodeKind::User, serde_json::json!({}))
            .unwrap();
        let branch = graph.fork_branch(Some(root), "alt").unwrap();
        let child = graph
            .append_node(branch, NodeKind::Assistant, serde_json::json!({}))
            .unwrap();
        graph
            .create_checkpoint(
                branch,
                "milestone",
                None,
                vec!["tag".to_string()],
                None,
                None,
            )
            .unwrap();

        assert_eq!(graph.children_of(root).len(), 1);
        assert_eq!(graph.branch_at(child), Some(branch));
        assert_eq!(graph.checkpoints_for_branch(branch).len(), 1);
    }

    #[test]
    fn creates_branch_bookmark() {
        let mut graph = SessionGraph::default();
        let node = graph
            .append_node(graph.primary_branch, NodeKind::User, serde_json::json!({}))
            .unwrap();
        let bookmark = graph
            .create_bookmark(node, "start", Some("entry".to_string()), None, None)
            .unwrap();

        assert_ne!(bookmark, crate::graph::BookmarkId::nil());
        assert_eq!(graph.bookmarks_for_branch(graph.primary_branch).len(), 1);
    }

    #[test]
    fn extracts_latest_summary_from_primary_branch() {
        let mut graph = SessionGraph::default();
        let branch = graph.primary_branch;
        graph
            .append_node(branch, NodeKind::User, serde_json::json!({}))
            .unwrap();
        graph
            .append_node(
                branch,
                NodeKind::Summary,
                serde_json::json!({
                    "summary": "old",
                    "content": [],
                }),
            )
            .unwrap();
        graph
            .append_node(
                branch,
                NodeKind::Summary,
                serde_json::json!({
                    "summary": "new",
                    "content": [],
                }),
            )
            .unwrap();

        assert_eq!(graph.latest_summary().as_deref(), Some("new"));
    }

    #[test]
    fn checkpoint_and_bookmark_events_preserve_actor_metadata() {
        let mut graph = SessionGraph::default();
        let node = graph
            .append_node(graph.primary_branch, NodeKind::User, serde_json::json!({}))
            .unwrap();
        let provenance = NodeProvenance {
            source_session_id: Uuid::new_v4().to_string(),
            session_type: "main".to_string(),
            task_id: None,
            subagent_session_id: None,
            subagent_type: None,
            subagent_description: None,
        };

        let checkpoint_id = graph
            .create_checkpoint(
                graph.primary_branch,
                "milestone",
                None,
                vec![],
                Some("user-1".to_string()),
                Some(provenance.clone()),
            )
            .unwrap();
        let bookmark_id = graph
            .create_bookmark(
                node,
                "start",
                None,
                Some("user-1".to_string()),
                Some(provenance),
            )
            .expect("bookmark should exist");

        let restored = crate::graph::GraphMaterializer::from_events(&graph.events);
        assert_eq!(
            restored
                .checkpoints
                .get(&checkpoint_id)
                .and_then(|checkpoint| checkpoint.created_by_principal_id.as_deref()),
            Some("user-1")
        );
        assert_eq!(
            restored
                .bookmarks
                .get(&bookmark_id)
                .and_then(|bookmark| bookmark.created_by_principal_id.as_deref()),
            Some("user-1")
        );
    }

    #[test]
    fn rejects_missing_branch_and_parent_mutations() {
        let mut graph = SessionGraph::default();
        let missing_branch = BranchId::new();
        let missing_parent = NodeId::new();

        assert!(matches!(
            graph.append_node(missing_branch, NodeKind::User, serde_json::json!({})),
            Err(GraphError::MissingBranch { .. })
        ));
        assert!(matches!(
            graph.append_existing_node(
                graph.primary_branch,
                NodeId::new(),
                Some(missing_parent),
                NodeKind::User,
                Vec::new(),
                serde_json::json!({}),
                Utc::now(),
                None,
                None,
            ),
            Err(GraphError::MissingParent { .. })
        ));
    }

    #[test]
    fn rejects_duplicate_nodes_and_cross_branch_parents() {
        let mut graph = SessionGraph::default();
        let root = graph
            .append_node(graph.primary_branch, NodeKind::User, serde_json::json!({}))
            .unwrap();
        let main_follow_up = graph
            .append_node(
                graph.primary_branch,
                NodeKind::Assistant,
                serde_json::json!({}),
            )
            .unwrap();
        let side = graph.fork_branch(Some(root), "side").unwrap();

        assert!(matches!(
            graph.append_existing_node(
                graph.primary_branch,
                root,
                Some(root),
                NodeKind::Assistant,
                Vec::new(),
                serde_json::json!({}),
                Utc::now(),
                None,
                None,
            ),
            Err(GraphError::DuplicateNode { node_id }) if node_id == root
        ));

        assert!(matches!(
            graph.append_existing_node(
                side,
                NodeId::new(),
                Some(main_follow_up),
                NodeKind::Assistant,
                Vec::new(),
                serde_json::json!({}),
                Utc::now(),
                None,
                None,
            ),
            Err(GraphError::ParentBranchMismatch { branch_id, parent_id, .. })
                if branch_id == side && parent_id == main_follow_up
        ));
    }

    #[test]
    fn apply_event_matches_full_materializer() {
        // Build a graph using the normal API (which pushes events internally).
        let mut original = SessionGraph::default();
        let primary = original.primary_branch;

        let n1 = original
            .append_node(primary, NodeKind::User, serde_json::json!({"q": 1}))
            .unwrap();
        let _n2 = original
            .append_node(primary, NodeKind::Assistant, serde_json::json!({"a": 1}))
            .unwrap();

        let side = original.fork_branch(Some(n1), "side").unwrap();
        original
            .append_node(side, NodeKind::User, serde_json::json!({"alt": true}))
            .unwrap();

        original
            .create_checkpoint(
                primary,
                "cp",
                Some("note".into()),
                vec!["v1".into()],
                None,
                None,
            )
            .unwrap();
        original
            .create_bookmark(n1, "mark", None, None, None)
            .unwrap();
        original.patch_node_metadata(n1, serde_json::json!({"tokens": 42}), None);

        // Replay all events through apply_event into a fresh graph that has the
        // same primary branch.
        let mut incremental = SessionGraph::new("main");
        // Replace the default primary branch with the original's primary branch
        // so the IDs line up.
        incremental.branches.clear();
        incremental.primary_branch = primary;
        incremental.branches.insert(
            primary,
            Branch {
                id: primary,
                name: "main".to_string(),
                forked_from: None,
                created_at: original.created_at,
                head: None,
            },
        );
        for event in &original.events {
            incremental.apply_event(event);
        }

        // Structural equality checks.
        assert_eq!(original.nodes.len(), incremental.nodes.len());
        assert_eq!(original.branches.len(), incremental.branches.len());
        assert_eq!(original.checkpoints.len(), incremental.checkpoints.len());
        assert_eq!(original.bookmarks.len(), incremental.bookmarks.len());
        assert_eq!(original.archived_watermark, incremental.archived_watermark);

        for (id, node) in &original.nodes {
            let inc_node = incremental.nodes.get(id).expect("node missing");
            assert_eq!(node.kind, inc_node.kind);
            assert_eq!(node.parent_id, inc_node.parent_id);
            assert_eq!(node.payload, inc_node.payload);
            assert_eq!(node.branch_id, inc_node.branch_id);
        }

        for (id, branch) in &original.branches {
            let inc_branch = incremental.branches.get(id).expect("branch missing");
            assert_eq!(branch.head, inc_branch.head);
        }
    }

    #[test]
    fn apply_event_handles_events_archived() {
        let mut graph = SessionGraph::default();
        let primary = graph.primary_branch;
        let n1 = graph
            .append_node(primary, NodeKind::User, serde_json::json!({}))
            .unwrap();

        assert!(graph.archived_watermark.is_none());

        let archive_event = GraphEvent::new(GraphEventBody::EventsArchived {
            watermark_node_id: n1,
            archived_count: 1,
        });
        graph.apply_event(&archive_event);

        assert_eq!(graph.archived_watermark, Some(n1));
    }

    // ---------------------------------------------------------------
    // archive_before — tool-pair integrity walk-back (T1-3)
    // ---------------------------------------------------------------

    /// Build a graph with a tool call / result pair so archival tests
    /// can reason about pair integrity. Layout on the primary branch:
    ///
    /// ```text
    ///   n0 (User "query")
    ///   n1 (Assistant with ToolCall id="t1")
    ///   n2 (User with ToolResult tool_call_id="t1")
    ///   n3 (Assistant "final answer")
    /// ```
    fn tool_pair_graph() -> (SessionGraph, [NodeId; 4]) {
        use crate::ir::ContentPart;
        let mut graph = SessionGraph::default();
        let primary = graph.primary_branch;

        let n0 = graph
            .append_node(
                primary,
                NodeKind::User,
                serde_json::json!({
                    "content": [ContentPart::text("query")],
                }),
            )
            .unwrap();
        let n1 = graph
            .append_node(
                primary,
                NodeKind::Assistant,
                serde_json::json!({
                    "content": [ContentPart::ToolCall {
                        id: "t1".to_string(),
                        name: "echo".to_string(),
                        arguments: serde_json::json!({}),
                        origin: crate::ir::ToolOrigin::Local,
                    }],
                }),
            )
            .unwrap();
        let n2 = graph
            .append_node(
                primary,
                NodeKind::User,
                serde_json::json!({
                    "content": [ContentPart::ToolResult {
                        tool_call_id: "t1".to_string(),
                        tool_name: Some("echo".to_string()),
                        content: crate::ir::ToolResultContent::Text("ok".to_string()),
                        is_error: false,
                    }],
                }),
            )
            .unwrap();
        let n3 = graph
            .append_node(
                primary,
                NodeKind::Assistant,
                serde_json::json!({
                    "content": [ContentPart::text("final answer")],
                }),
            )
            .unwrap();

        (graph, [n0, n1, n2, n3])
    }

    #[test]
    fn archive_before_leaves_watermark_alone_when_no_pair_crosses() {
        let (mut graph, nodes) = tool_pair_graph();
        // Archive everything up to n3 — this archives n0, n1, n2 and
        // leaves only n3 visible. No tool pair is visible (because n2
        // becomes archived too), so walk-back is not needed.
        let count = graph.archive_before(nodes[3]).unwrap();
        assert_eq!(count, 3);
        assert_eq!(graph.archived_watermark, Some(nodes[3]));
    }

    #[test]
    fn archive_before_walks_back_to_include_matching_tool_call() {
        let (mut graph, nodes) = tool_pair_graph();
        // Request watermark at n2 (the ToolResult). This would archive
        // n0, n1 and leave n2, n3 visible — orphaning n2's ToolResult
        // from its matching ToolCall at n1. The walk-back must shift
        // the watermark back to n1 so both sides of the pair stay
        // visible together.
        let count = graph.archive_before(nodes[2]).unwrap();
        assert_eq!(
            graph.archived_watermark,
            Some(nodes[1]),
            "watermark must walk back to the ToolCall node"
        );
        // After walk-back, only n0 is archived (the User query before
        // the ToolCall).
        assert_eq!(count, 1);
    }

    #[test]
    fn archive_before_with_no_crossing_is_identity() {
        let (mut graph, nodes) = tool_pair_graph();
        // Archive before n1 — this archives only n0 and leaves the
        // whole pair plus the final answer visible. No crossing, no
        // walk-back needed.
        let count = graph.archive_before(nodes[1]).unwrap();
        assert_eq!(count, 1);
        assert_eq!(graph.archived_watermark, Some(nodes[1]));
    }

    #[test]
    fn archive_before_walks_back_past_multiple_pairs() {
        // Layout:
        //   n0 User, n1 Asst ToolCall t1, n2 User ToolResult t1,
        //   n3 Asst ToolCall t2, n4 User ToolResult t2, n5 Asst "done".
        // Requesting watermark at n4 would visible-set = {n4, n5} and
        // archive {n0..n3}. But n4's ToolResult t2 has its matching
        // ToolCall at n3 (archived). Walk-back goes to n3; now visible
        // set = {n3, n4, n5} and {n0, n1, n2} archived. n3 has no
        // dangling ToolResult. Stop. Final watermark = n3.
        use crate::ir::ContentPart;
        let mut graph = SessionGraph::default();
        let primary = graph.primary_branch;
        let _n0 = graph
            .append_node(
                primary,
                NodeKind::User,
                serde_json::json!({ "content": [ContentPart::text("q")] }),
            )
            .unwrap();
        let _n1 = graph
            .append_node(
                primary,
                NodeKind::Assistant,
                serde_json::json!({
                    "content": [ContentPart::ToolCall {
                        id: "t1".into(),
                        name: "e".into(),
                        arguments: serde_json::json!({}),
                        origin: crate::ir::ToolOrigin::Local,
                    }],
                }),
            )
            .unwrap();
        let _n2 = graph
            .append_node(
                primary,
                NodeKind::User,
                serde_json::json!({
                    "content": [ContentPart::ToolResult {
                        tool_call_id: "t1".into(),
                        tool_name: Some("e".into()),
                        content: crate::ir::ToolResultContent::Text("r1".into()),
                        is_error: false,
                    }],
                }),
            )
            .unwrap();
        let n3 = graph
            .append_node(
                primary,
                NodeKind::Assistant,
                serde_json::json!({
                    "content": [ContentPart::ToolCall {
                        id: "t2".into(),
                        name: "e".into(),
                        arguments: serde_json::json!({}),
                        origin: crate::ir::ToolOrigin::Local,
                    }],
                }),
            )
            .unwrap();
        let n4 = graph
            .append_node(
                primary,
                NodeKind::User,
                serde_json::json!({
                    "content": [ContentPart::ToolResult {
                        tool_call_id: "t2".into(),
                        tool_name: Some("e".into()),
                        content: crate::ir::ToolResultContent::Text("r2".into()),
                        is_error: false,
                    }],
                }),
            )
            .unwrap();

        let _count = graph.archive_before(n4).unwrap();
        assert_eq!(
            graph.archived_watermark,
            Some(n3),
            "walk-back must stop at the second pair's ToolCall"
        );
    }

    #[test]
    fn archive_before_missing_node_returns_error() {
        let mut graph = SessionGraph::default();
        let fake = NodeId::from_uuid(uuid::Uuid::new_v4());
        let err = graph.archive_before(fake).unwrap_err();
        assert!(matches!(err, GraphError::MissingNode { .. }));
    }
}
