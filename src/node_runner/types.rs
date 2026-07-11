//! Request and result types for the NodeRunner boundary.

use std::cell::RefCell;
use std::rc::Rc;

use crate::artifacts::{ArtifactView, Workspace};
use crate::machines::scheduler::{
    ModelTier, NodeFailure, NodeId, NodeKind, PlanOutput, TestPlanContext, WorkOutput,
};

/// A request to run a single scheduler node.
///
/// Carries exactly the fields the scheduler emits in a `RunNode` effect, so
/// the handler layer can forward them verbatim without re-reading the graph.
pub struct NodeRunRequest {
    /// Whether the node should plan or execute.
    pub kind: NodeKind,
    /// Identifier of the scheduler node this request was dispatched for.
    ///
    /// Carried for telemetry enrichment; runners must not use this for
    /// control flow.
    pub node_id: NodeId,
    /// Natural-language description of what the node should accomplish.
    pub objective: String,
    /// The worker role assigned to this Work node, if any.
    pub worker_role: Option<String>,
    /// Structured target files this node is expected and allowed to touch.
    pub target_files: Vec<String>,
    /// Structured test-target context computed from graph metadata.
    pub test_plan_context: TestPlanContext,
    /// The model capability level the runner should use.
    pub model_tier: ModelTier,
    /// Zero-based retry count; 0 on the first attempt.
    pub attempt: u32,
    /// Optional read-only view of the artifact at the time this node was dispatched.
    ///
    /// When present, runners may use file listings and content as context for
    /// deliberation. Absence means no artifact is associated with this node.
    pub artifact_view: Option<ArtifactView>,
    /// Attempt-local artifact workspace for live artifact-producing Work.
    ///
    /// Producer tools mutate this workspace directly. Reviewer tools read the
    /// same workspace. Integration is responsible for validating and
    /// publishing it.
    pub work_attempt: Option<WorkAttempt>,
    /// Copied verbatim from `Node::adapter`. Empty for the single-team path.
    /// Runners do not yet consume this field.
    pub adapter: String,
    /// Copied verbatim from `Node::northstar`. Empty for the single-team
    /// path. Runners do not yet consume this field.
    pub northstar: String,
}

/// Candidate artifact state for one Work node attempt.
#[derive(Clone)]
pub struct WorkAttempt {
    /// Zero-based scheduler attempt number this workspace belongs to.
    pub attempt: u32,
    /// Mutable checkout owned by the attempt.
    pub workspace: Rc<RefCell<Workspace>>,
}

/// The output of a completed work node.
pub struct NodeRunWorkResult {
    /// The work summary returned by the node.
    pub work: WorkOutput,
}

/// The outcome of running a single scheduler node.
///
/// Dispatch maps this directly onto the corresponding scheduler event.
pub enum NodeRunResult {
    /// A plan node completed and produced child nodes to insert.
    PlanAccepted(PlanOutput),
    /// A work node completed and produced a summary.
    ///
    /// Artifact-backed Work publishes only the state in its WorkAttempt
    /// workspace.
    WorkAccepted(NodeRunWorkResult),
    /// The node could not complete. The embedded failure says how to recover.
    Failed(NodeFailure),
}
