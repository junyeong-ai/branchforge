#![allow(missing_docs)]

use crate::graph::{NodeId, ReplayInput, SessionGraph};
use crate::session::{SessionError, SessionResult};

pub struct Replayer;

impl Replayer {
    pub fn replay_input(
        graph: &SessionGraph,
        from_node: Option<NodeId>,
    ) -> SessionResult<ReplayInput> {
        graph
            .replay_input(graph.primary_branch, from_node)
            .map_err(|error| SessionError::Storage {
                message: error.to_string(),
            })
    }
}
