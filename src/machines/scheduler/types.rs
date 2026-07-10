//! Scheduler support payload and domain vocabulary.

use crate::validation::ValidationPlan;

use super::failure::FailureKind;
use super::graph::{NodeId, NodeKind};

/// The structured output of a plan node that succeeded.
///
/// A `PlanOutput` tells the scheduler which new nodes to add to the graph.
/// Each entry in `children` becomes a real `Node` with a fresh `NodeId`.
/// The planner is responsible for specifying correct dependency relationships
/// so that children run in the right order.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanOutput {
    /// The set of nodes the planner wants the scheduler to insert.
    pub children: Vec<NodeRequest>,
    /// Planner-produced task records with no corresponding scheduler node.
    ///
    /// Populated only for `PlannerOutputKind::Task` output, in which case
    /// `children` is always empty. Recorded into the task manifest via
    /// `SchedulerEffect::IntegratePlannerTasks` instead of becoming graph
    /// nodes.
    pub tasks: Vec<PlannerTaskOutput>,
}

/// A single planner-produced task record with no corresponding scheduler
/// node, carried from `PlannerOutputKind::Task` output through to
/// `IntegrationService::integrate_plan_tasks`.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannerTaskOutput {
    /// Planner-assigned identifier for this task.
    pub id: String,
    /// Natural-language description of the planner's intent.
    pub objective: String,
}

/// The structured output of a work node that succeeded.
///
/// `WorkOutput` is minimal: a work node's only obligation is to report
/// what it did. The summary is stored on the node and is available in the
/// final `RunGraph` for audit and downstream context.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkOutput {
    /// A brief, human-readable description of what was accomplished.
    pub summary: String,
}

/// A description of a node that the scheduler should create and add to the graph.
///
/// `NodeRequest` is the currency planners use to expand the graph. The
/// scheduler assigns a fresh `NodeId` and wraps this into a real `Node`.
/// Initial `attempt` and `model_tier` are always reset to defaults on creation.
///
/// The `id` field is a planner-supplied local name used solely for same-batch
/// dependency detection during validation. It does not become the node's graph
/// `NodeId`; actual graph IDs are freshly generated at insertion time.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeRequest {
    /// Planner-assigned local name for this request.
    ///
    /// Used by `validate_plan_dependencies` to identify same-batch sibling
    /// references. Not used as or mapped to the resulting graph `NodeId`.
    pub id: NodeId,
    /// Whether the new node should plan or execute.
    pub kind: NodeKind,
    /// The adapter-assigned worker role for a `Work` node (e.g. `"tester"`).
    ///
    /// `None` for `Plan` nodes and for `Work` nodes with no distinct role.
    /// Copied verbatim onto the resulting `Node.worker_role`.
    pub worker_role: Option<String>,
    /// Natural-language description of what the new node should accomplish.
    pub objective: String,
    /// Structured target files this node is expected and allowed to touch.
    ///
    /// This is planner metadata, not natural-language prompt text. An empty
    /// list means no target constraint is known.
    pub target_files: Vec<String>,
    /// Adapter-derived test targets required for this node's target files.
    ///
    /// Planners normally leave this empty; runner-side plan stamping fills it
    /// from project adapter metadata before the scheduler inserts the node.
    pub required_validation_targets: Vec<String>,
    /// Nodes that must complete before this node is eligible to run.
    pub dependencies: Vec<NodeId>,
    /// The validation contract to attach to the new node.
    ///
    /// The scheduler copies this into the resulting `Node.validation_plan`.
    /// `None` means no plan; integration will fall back to the handler-level
    /// validator.
    pub validation_plan: Option<ValidationPlan>,
}

/// The failure report returned when a node cannot complete successfully.
///
/// A `NodeFailure` always carries a typed `kind`, a human-readable `message`
/// (for logging and audit), and a `recovery` that tells the scheduler exactly
/// what to do next. The scheduler does not interpret `message`; it acts solely
/// on `recovery`.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeFailure {
    /// Machine-readable failure cause.
    pub kind: FailureKind,
    /// Why the node failed. Preserved in logs; not parsed by the scheduler.
    pub message: String,
    /// The scheduler's next action. Determines how the graph evolves after
    /// this failure.
    pub recovery: RecoveryAction,
}

/// The scheduler's course of action after a node reports failure.
///
/// Every `RecoveryAction` results in the original node being marked `Failed`
/// (never mutated or removed) and a new graph element being created to
/// continue the work, except `Terminal`, which halts the run immediately.
///
/// # Invariant: failed nodes are permanent records
///
/// Recovery always creates a *replacement* node. The failed node stays in the
/// graph so the full attempt history is available for inspection.
#[derive(Clone, Debug, PartialEq)]
pub enum RecoveryAction {
    /// Create a replacement node with the same objective and model tier,
    /// incrementing `attempt`. Use when the failure is transient and the same
    /// approach is likely to succeed on a second try.
    Retry {
        /// A human-readable note about why the retry was requested.
        message: String,
    },
    /// Create a new `Plan` node (at `ModelTier::Strong`) that re-plans the
    /// failed node's original objective, with `message` carried alongside it
    /// as diagnostic context. The planner will decompose the objective into
    /// sub-tasks. Use when the task proved too large or ambiguous to execute
    /// directly.
    Split {
        /// Diagnostic context describing why the task needs decomposition.
        /// Not the new plan node's objective.
        message: String,
    },
    /// Create a replacement node with `ModelTier::Strong`, incrementing
    /// `attempt`. Use when the failure was caused by the model lacking
    /// sufficient capability, not by a transient error.
    ElevateModel {
        /// A human-readable note about why model escalation was requested.
        message: String,
    },
    /// Halt the entire run immediately. No replacement is created.
    /// Use when the failure is unrecoverable and continuing would be
    /// meaningless or harmful.
    Terminal {
        /// The reason the run was halted; preserved in `SchedulerState::Failed`.
        message: String,
    },
}

/// The structured output of a successful integration.
#[derive(Clone, Debug, PartialEq)]
pub struct IntegrationOutput {
    /// A brief human-readable description of what the integration produced.
    pub summary: String,
}

/// The failure report returned when integration cannot complete.
#[derive(Clone, Debug, PartialEq)]
pub struct IntegrationFailure {
    /// Machine-readable failure cause.
    pub kind: FailureKind,
    /// Why integration failed.
    pub message: String,
    /// The scheduler's next action after integration failure.
    pub recovery: RecoveryAction,
}
