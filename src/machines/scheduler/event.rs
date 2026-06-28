//! Events and node-outcome types for the scheduler machine.
//!
//! Events are facts delivered *to* the scheduler from the outside world.
//! They describe things that have already happened: a node finished, or the
//! runner is ready to start.
//!
//! The outcome types in this module (`NodeOutcome`, `NodeFailure`,
//! `RecoveryAction`, `PlanOutput`, `WorkOutput`) are part of the event payload
//! carried inside `NodeReturned`. They describe what a node produced and how
//! the scheduler should respond to it.
//!
//! This module does **not** own scheduler state shapes or emitted commands;
//! those live in `state.rs` and `effect.rs`.

use crate::validation::ValidationPlan;

use super::state::{NodeId, NodeKind};

/// Machine-readable cause of a node or integration failure.
///
/// `message` fields on failure payloads remain human-readable diagnostics only;
/// recovery policy must switch on this kind instead of parsing message text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// Provider transport, timeout, rate-limit, or other retryable provider failure.
    ProviderFailure,
    /// Provider failure known not to benefit from retrying, such as bad auth/config.
    ProviderTerminalFailure,
    /// Role or planner response violated the expected JSON/protocol contract.
    ProtocolFailure,
    /// File/tool loop failure.
    ToolFailure,
    /// Project validation command failed.
    ValidationFailure,
    /// Planner output violated structured planner validation.
    PlannerValidationFailure,
    /// Work producer accepted a result with no artifact file changes.
    WorkSemanticValidationFailure,
    /// Deliberation reached a semantic quality limit, such as exhausted revisions.
    DeliberationFailure,
    /// Artifact integration failed.
    IntegrationFailure,
    /// The user task was semantically rejected by the producing role.
    UserTaskRejection,
}

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
/// `NodeId`; actual graph IDs are generated from the `RunGraph::next_id` counter
/// at insertion time.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeRequest {
    /// Planner-assigned local name for this request.
    ///
    /// Used by `validate_plan_dependencies` to identify same-batch sibling
    /// references. Not used as or mapped to the resulting graph `NodeId`.
    pub id: NodeId,
    /// Whether the new node should plan or execute.
    pub kind: NodeKind,
    /// Natural-language description of what the new node should accomplish.
    pub objective: String,
    /// Structured target files this node is expected and allowed to touch.
    ///
    /// This is planner metadata, not natural-language prompt text. An empty
    /// list means no target constraint is known.
    pub target_files: Vec<String>,
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
/// continue the work — except `Terminal`, which halts the run immediately.
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
    /// Create a new `Plan` node (at `ModelTier::Strong`) whose objective is
    /// `message`. The planner will decompose the failed objective into
    /// sub-tasks. Use when the task proved too large or ambiguous to execute
    /// directly.
    Split {
        /// The objective for the new plan node that will decompose the work.
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

/// The three possible outcomes when a node finishes.
///
/// The scheduler pattern-matches on `NodeOutcome` inside the `Waiting +
/// NodeReturned` transition to decide how to update the graph.
#[derive(Clone, Debug, PartialEq)]
pub enum NodeOutcome {
    /// A plan node completed successfully and wants new nodes inserted.
    PlanAccepted(PlanOutput),
    /// A work node produced work that must still pass integration before the
    /// node is marked `Completed`.
    WorkAccepted(WorkOutput),
    /// The node could not complete. The embedded `NodeFailure` tells the
    /// scheduler which recovery path to take.
    Failed(NodeFailure),
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

/// The two possible outcomes when integration finishes.
#[derive(Clone, Debug, PartialEq)]
pub enum IntegrationOutcome {
    /// Integration completed successfully.
    Succeeded(IntegrationOutput),
    /// Integration could not complete. The embedded `IntegrationFailure` tells
    /// the scheduler which recovery path to take.
    Failed(IntegrationFailure),
}

/// Events that the scheduler machine can receive.
///
/// `Start` is a synthetic tick injected by the runner when the scheduler is
/// in `Running` state and no external result is pending. It drives the
/// machine from `Running` to `Waiting` (by dispatching a ready node), to
/// `Complete`, or to `Failed` — all without blocking on an external result.
///
/// `NodeReturned` and `IntegrationReturned` carry real external results that
/// drive the `Waiting` state forward.
#[derive(Clone, Debug, PartialEq)]
pub enum SchedulerEvent {
    /// A synthetic tick that drives the `Running` state forward. The
    /// scheduler scans the graph, selects a ready node to dispatch, and
    /// moves to `Waiting`. If no node is ready the run fails; if all nodes
    /// are terminal the run completes.
    Start,
    /// A previously-dispatched node has finished and is reporting its outcome.
    NodeReturned {
        /// The ID of the node that finished, used to verify it matches `running`.
        node_id: NodeId,
        /// What the node produced and how the scheduler should react.
        outcome: NodeOutcome,
    },
    /// A previously-dispatched integration has finished and is reporting its outcome.
    IntegrationReturned {
        /// The ID of the node whose work was being integrated.
        node_id: NodeId,
        /// Whether integration succeeded or failed, and how to proceed.
        outcome: IntegrationOutcome,
    },
}
