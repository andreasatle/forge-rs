use super::state::{NodeId, RunGraph};

#[derive(Clone, Debug, PartialEq)]
pub enum SchedulerEffect {
    RunNode {
        node_id: NodeId,
    },
    ReturnComplete {
        graph: RunGraph,
    },
    ReturnFailed {
        graph: RunGraph,
        reason: String,
    },
}
