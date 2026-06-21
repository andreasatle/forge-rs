//! Request and result types for the NodeRunner boundary.

use crate::machines::scheduler::{ModelTier, NodeFailure, NodeKind, PlanOutput, WorkOutput};

/// A request to run a single scheduler node.
///
/// Carries exactly the fields the scheduler emits in a `RunNode` effect, so
/// the handler layer can forward them verbatim without re-reading the graph.
pub struct NodeRunRequest {
    /// Whether the node should plan or execute.
    pub kind: NodeKind,
    /// Natural-language description of what the node should accomplish.
    pub objective: String,
    /// The model capability level the runner should use.
    pub model_tier: ModelTier,
    /// Zero-based retry count; 0 on the first attempt.
    pub attempt: u32,
}

/// The outcome of running a single scheduler node.
///
/// Maps directly onto [`NodeOutcome`](crate::machines::scheduler::NodeOutcome):
/// use `From<NodeRunResult> for NodeOutcome` to convert.
pub enum NodeRunResult {
    /// A plan node completed and produced child nodes to insert.
    PlanAccepted(PlanOutput),
    /// A work node completed and produced a summary.
    WorkAccepted(WorkOutput),
    /// The node could not complete. The embedded failure says how to recover.
    Failed(NodeFailure),
}
