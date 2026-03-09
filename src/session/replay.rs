use crate::graph::{NodeId, ReplayInput, SessionGraph};

pub struct ReplayService;

impl ReplayService {
    pub fn replay_input(graph: &SessionGraph, from_node: Option<NodeId>) -> ReplayInput {
        graph.replay_input(graph.primary_branch, from_node)
    }
}
