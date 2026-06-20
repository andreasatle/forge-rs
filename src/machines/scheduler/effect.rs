use super::state::{ModelTier, NodeId, NodeKind, RunGraph};

#[derive(Clone, Debug, PartialEq)]
pub enum SchedulerEffect {
    RunNode {
        node_id: NodeId,
        kind: NodeKind,
        objective: String,
        model_tier: ModelTier,
        attempt: u32,
    },
    ReturnComplete {
        graph: RunGraph,
    },
    ReturnFailed {
        graph: RunGraph,
        reason: String,
    },
}
