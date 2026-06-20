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

use super::state::{NodeId, NodeKind};

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
#[derive(Clone, Debug, PartialEq)]
pub struct NodeRequest {
    /// Whether the new node should plan or execute.
    pub kind: NodeKind,
    /// Natural-language description of what the new node should accomplish.
    pub objective: String,
    /// Nodes that must complete before this node is eligible to run.
    pub dependencies: Vec<NodeId>,
}

/// The failure report returned when a node cannot complete successfully.
///
/// A `NodeFailure` always carries a human-readable `reason` (for logging and
/// audit) and a `recovery` that tells the scheduler exactly what to do next.
/// The scheduler does not interpret `reason`; it acts solely on `recovery`.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeFailure {
    /// Why the node failed. Preserved in logs; not parsed by the scheduler.
    pub reason: String,
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
/// The scheduler pattern-matches on `NodeOutcome` inside the `Waiting` →
/// `SelectingReady` transition to decide how to update the graph.
#[derive(Clone, Debug, PartialEq)]
pub enum NodeOutcome {
    /// A plan node completed successfully and wants new nodes inserted.
    PlanAccepted(PlanOutput),
    /// A work node completed successfully and produced a summary.
    WorkAccepted(WorkOutput),
    /// The node could not complete. The embedded `NodeFailure` tells the
    /// scheduler which recovery path to take.
    Failed(NodeFailure),
}

/// Events that the scheduler machine can receive.
///
/// `Start` is a synthetic tick injected by the runner when no effect is
/// pending. It drives the machine through pure bookkeeping steps
/// (`NotStarted` → `SelectingReady` → `Dispatching`) without blocking on an
/// external result.
///
/// `NodeReturned` carries the real external result: the outcome of a node
/// that was previously dispatched via a `RunNode` effect.
#[derive(Clone, Debug, PartialEq)]
pub enum SchedulerEvent {
    /// A synthetic tick. Used to advance the machine through states that
    /// require no external input.
    Start,
    /// A previously-dispatched node has finished and is reporting its outcome.
    NodeReturned {
        /// The ID of the node that finished, used to verify it matches `running`.
        node_id: NodeId,
        /// What the node produced and how the scheduler should react.
        outcome: NodeOutcome,
    },
}
