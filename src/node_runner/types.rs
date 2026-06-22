//! Request and result types for the NodeRunner boundary.

use crate::artifacts::{ArtifactUpdate, ArtifactView};
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
    /// Optional read-only view of the artifact at the time this node was dispatched.
    ///
    /// When present, runners may use file listings and content as context for
    /// deliberation. Absence means no artifact is associated with this node.
    pub artifact_view: Option<ArtifactView>,
}

/// The output of a completed work node, paired with any artifact changes it produced.
pub struct NodeRunWorkResult {
    /// The work summary returned by the node.
    pub work: WorkOutput,
    /// File changes the node produced, if any.
    ///
    /// `None` means the node produced no artifact changes.
    ///
    /// The scheduler does not understand artifacts yet. Callers converting to
    /// [`NodeOutcome`](crate::machines::scheduler::NodeOutcome) must discard this field.
    pub artifact_update: Option<ArtifactUpdate>,
}

/// The outcome of running a single scheduler node.
///
/// Maps directly onto [`NodeOutcome`](crate::machines::scheduler::NodeOutcome):
/// use `From<NodeRunResult> for NodeOutcome` to convert.
pub enum NodeRunResult {
    /// A plan node completed and produced child nodes to insert.
    PlanAccepted(PlanOutput),
    /// A work node completed and produced a summary with optional artifact changes.
    ///
    /// The scheduler does not understand artifact changes yet; when converting to
    /// `NodeOutcome` the `artifact_update` inside [`NodeRunWorkResult`] is discarded.
    WorkAccepted(NodeRunWorkResult),
    /// The node could not complete. The embedded failure says how to recover.
    Failed(NodeFailure),
}
