//! Scheduler state types.
//!
//! This module owns the durable data shapes the scheduler carries between
//! transitions: the work graph and all node descriptors.
//!
//! It does **not** own events (what the scheduler receives) or effects (what it
//! commands). Those live in `event.rs` and `effect.rs` respectively.
//!
//! # Key invariants
//!
//! - `NodeId` values are unique within a `RunGraph` and never reused.
//! - Nodes are never removed from the graph; status fields move forward only.
//! - `RunGraph::next_id` is an internal generator cursor used when the
//!   scheduler mints new identifiers.

use serde::{Deserialize, Serialize};

use crate::validation::ValidationPlan;

/// An opaque, stable identifier for a node in the run graph.
///
/// IDs are unique within a run. The string form is human-readable but must not
/// be parsed; its internal structure is an implementation detail.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub String);

/// Whether a node performs planning or execution.
///
/// The distinction determines what output the scheduler expects back and how it
/// reacts to that output:
///
/// - `Plan` nodes are expected to decompose work and return child
///   [`NodeRequest`](super::event::NodeRequest)s. When accepted, the scheduler
///   inserts the requested children and continues graph traversal.
/// - `Work` nodes are expected to perform a concrete task and return a summary
///   string. When the runner reports `WorkAccepted`, the node moves to
///   `Integrating` and an `IntegrateWork` effect is emitted. The node reaches
///   `Completed` only after `IntegrationReturned(Succeeded)` arrives.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum NodeKind {
    /// A planning node. Decomposes an objective into child nodes.
    Plan,
    /// An execution node. Carries out a concrete, bounded task.
    Work,
}

/// Structured test-target context for a work node.
///
/// `required_test_targets` is the adapter-derived contract attached to source
/// nodes. `planned_test_targets` is computed from graph dependency metadata at
/// dispatch time and tells reviewers whether tests are scheduled separately.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestPlanContext {
    /// Test targets required for the node's own structured target files.
    pub required_test_targets: Vec<String>,
    /// Targets scheduled in nodes that depend on this node, directly or transitively.
    pub planned_test_targets: Vec<String>,
}

/// The model capability level to use when running a node.
///
/// `Cheap` is used for most work because cost compounds quickly across many
/// nodes. `Strong` is reserved for cases where the task has already proven too
/// difficult for the cheaper tier, or where plan quality directly determines
/// downstream work.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum ModelTier {
    /// The default, cost-efficient tier. Used for initial attempts.
    Cheap,
    /// The high-capability tier. Used for model-escalation retries and split
    /// recovery planning nodes.
    Strong,
}

/// How a node was introduced into the run graph.
///
/// Stored on every node so the scheduler can derive a typed `RecoverySummary`
/// from the final graph without inspecting IDs or objective strings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum NodeOrigin {
    /// The root plan node created directly from a `RunRequest`.
    Root,
    /// A node inserted by a plan node's `PlanOutput` during graph expansion.
    PlanExpansion,
    /// A replacement node created by `Retry` recovery.
    Retry {
        /// The node that failed and triggered this replacement.
        source: NodeId,
    },
    /// A replacement node created by `ElevateModel` recovery.
    ElevateModel {
        /// The node that failed and triggered this replacement.
        source: NodeId,
    },
    /// A replacement `Plan` node created by `Split` recovery.
    Split {
        /// The node that failed and triggered this plan node.
        source: NodeId,
    },
}

/// The lifecycle position of a node within the run graph.
///
/// Status only moves forward; no transition goes backward. Terminals
/// (`Completed`, `Failed`, `Cancelled`) are permanent.
///
/// # Invariant: failed nodes are historical records
///
/// A `Failed` node is never resurrected. Recovery always creates a *new*
/// replacement node, so the original failure is preserved for inspection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum NodeStatus {
    /// Not yet eligible to run; waiting for dependencies to complete.
    Pending,
    /// Dispatched to a runner; awaiting a `NodeReturned` event.
    Running,
    /// Work has been produced by the runner but is not dependency-satisfying
    /// until integration succeeds.
    Integrating,
    /// Finished successfully. Dependencies on this node are now satisfiable.
    Completed,
    /// Finished unsuccessfully. The node is a permanent historical record.
    /// Recovery creates a replacement node rather than mutating this one.
    Failed,
    /// Skipped due to an upstream terminal failure. Set by the scheduler on
    /// every `Pending` node that depends (directly or transitively) on a node
    /// that received a `Terminal` recovery action.
    Cancelled,
}

/// Structured diagnostic feedback attached to a node that failed validation
/// and will be retried.
///
/// The machine stores this on the retry node instead of appending it to the
/// objective string. The dispatch layer renders it into the prompt at
/// dispatch time so the objective remains the original task description.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RetryFeedback {
    /// Concise diagnostic output from the failed validation attempt.
    ///
    /// Truncated by the machine to a reasonable prompt length. The format
    /// (command, exit code, diagnostic lines, etc.) is determined by the
    /// integration layer and not parsed here.
    pub diagnostics: String,
}

/// A single unit of work in the run graph.
///
/// Each node carries everything the scheduler and runner need to dispatch,
/// track, and audit it. Fields are set at creation and updated only through
/// the explicit graph-mutation helpers on `SchedulerMachine`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// Stable identifier, unique within the run graph.
    pub id: NodeId,
    /// Whether this node plans or executes.
    pub kind: NodeKind,
    /// A natural-language description of what this node should accomplish.
    /// Passed verbatim to the runner; preserved across retries and escalations.
    pub objective: String,
    /// Structured target files this node is expected and allowed to touch.
    ///
    /// Prompt text may render these for the model, but tooling must use this
    /// metadata instead of parsing the objective.
    pub target_files: Vec<String>,
    /// Adapter-derived test targets required for this node's target files.
    ///
    /// This is structured planner/adapter metadata. It is not inferred from
    /// objective text and is preserved across retries and model escalation.
    #[serde(default)]
    pub required_test_targets: Vec<String>,
    /// Nodes that must be `Completed` before this node is eligible to run.
    /// The scheduler will not dispatch a node until all listed dependencies are
    /// in the `Completed` state.
    pub dependencies: Vec<NodeId>,
    /// Current lifecycle position in the graph.
    pub status: NodeStatus,
    /// Zero-based retry count. Incremented each time a replacement node is
    /// created for this objective, giving the runner observability into how
    /// many previous attempts have been made.
    pub attempt: u32,
    /// Scheduler circuit breaker metadata for recursive planning.
    ///
    /// This is not a business rule. It records ancestry through `Plan` nodes
    /// so the scheduler can bound plan chains without traversing the graph.
    /// Work nodes inherit their parent's depth without increasing it.
    pub plan_depth: usize,
    /// The model capability level to use when running this node.
    pub model_tier: ModelTier,
    /// A brief human-readable description of the outcome, set when the node
    /// reaches `Completed`. `None` until then.
    pub summary: Option<String>,
    /// How this node was introduced into the graph.
    ///
    /// Used by `RecoverySummary::from_graph` to classify a completed run
    /// without inspecting IDs or objective strings.
    pub origin: NodeOrigin,
    /// The validation contract for this node.
    ///
    /// Present only on `Work` nodes.  Set at plan-expansion time and preserved
    /// unchanged through retries, model escalations, and checkpoint/resume.
    /// When `None`, integration falls back to the handler-level validator.
    #[serde(default)]
    pub validation_plan: Option<ValidationPlan>,
    /// Structured diagnostic feedback from a failed validation attempt.
    ///
    /// Set by `apply_retry` when the failure kind is `ValidationFailure` or
    /// `WorkSemanticValidationFailure`. The objective string is left unchanged;
    /// the dispatch layer renders this into the prompt at dispatch time.
    /// `None` for the first attempt and for non-validation retries.
    #[serde(default)]
    pub retry_feedback: Option<RetryFeedback>,
}

/// The complete set of nodes for one Forge run, plus the internal ID cursor.
///
/// The graph only grows: nodes are appended on plan expansion and recovery, but
/// never removed. This ensures the full execution history is always available
/// for debugging and audit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunGraph {
    /// All nodes, in insertion order. The ordering has no semantic meaning;
    /// the scheduler scans the vec when computing ready sets.
    pub nodes: Vec<Node>,
    /// Internal cursor used to mint fresh `NodeId`s without global state.
    /// Graph validation treats existing `NodeId` strings as opaque and does
    /// not parse them to verify this cursor.
    pub next_id: u32,
}

/// The external input to the scheduler.
///
/// Callers provide a `RunRequest` to start a new run instead of constructing a
/// `RunGraph` directly. `SchedulerMachine::initial_state` converts it into a
/// `SchedulerState::Active` containing a single root `Plan` node.
pub struct RunRequest {
    /// A natural-language description of what this run should accomplish.
    /// Becomes the objective of the root plan node.
    pub objective: String,
}

/// The durable checkpoints of the scheduler state machine.
///
/// Each variant carries exactly the data needed to resume from that point.
/// The scheduler advances through these states as it drives the run graph
/// toward completion.
///
/// # State flow
///
/// ```text
/// Active
///   │ Start
///   ├─ invalid graph ───────────────→ Failed
///   ├─ all nodes terminal ──────────→ Complete
///   ├─ no ready nodes (deadlock) ───→ Failed
///   └─ first ready node found
///        mark Running, emit RunNode
///              ↓
///           Waiting
///              │ NodeReturned
///              ├─ PlanAccepted ────────→ Active   (insert children)
///              ├─ WorkAccepted ────────→ Waiting  (mark Integrating, emit IntegrateWork)
///              │    │ IntegrationReturned
///              │    ├─ Succeeded ──────→ Active   (mark Completed)
///              │    └─ Failed ─────────→ Active | Failed  (recovery)
///              ├─ recoverable failure ─→ Active   (insert replacement)
///              └─ Terminal failure ────→ Failed   (cancel dependents)
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SchedulerState {
    /// The scheduler is ready to scan the graph and dispatch the next node.
    ///
    /// On a `Start` event the scheduler checks whether all nodes are terminal
    /// (→ `Complete`), whether the graph is deadlocked (→ `Failed`), or picks
    /// the first ready node to dispatch (→ `Waiting`).
    #[serde(rename = "Running")]
    Active {
        /// The run graph to scan and advance.
        graph: RunGraph,
    },
    /// One node in the graph has been dispatched and the scheduler is waiting
    /// for its result. No further dispatch happens until `NodeReturned` or
    /// `IntegrationReturned` arrives. The active node is derived from the
    /// single node whose status is `Running` or `Integrating`. If the node
    /// reported `WorkAccepted`, it will be in `Integrating` status and the
    /// scheduler awaits `IntegrationReturned`.
    Waiting {
        /// The run graph with the dispatched node marked `Running` or `Integrating`.
        graph: RunGraph,
    },
    /// All nodes have reached a terminal status (`Completed`, `Failed`, or
    /// `Cancelled`) with no failures that halted the run. The graph is the
    /// complete execution record.
    Complete {
        /// The final graph with every node in a terminal status.
        graph: RunGraph,
    },
    /// The run was halted and cannot continue. The graph is preserved for
    /// post-mortem inspection.
    ///
    /// Causes include:
    /// - A `Terminal` recovery action (node reported an unrecoverable failure).
    /// - Attempt exhaustion: `Retry`, `ElevateModel`, or `Split` on a node
    ///   already at `MAX_ATTEMPTS`.
    /// - An invalid graph supplied to `Active + Start` (duplicate IDs or
    ///   missing dependency references).
    /// - An invalid node outcome: mismatched kind/outcome (e.g. `WorkAccepted`
    ///   for a `Plan` node, or `PlanAccepted` for a `Work` node).
    /// - An invalid plan output: a child request references an unknown `NodeId`.
    /// - A deadlock: no node is ready but the graph is not yet complete
    ///   (blocked dependency chain or cycle).
    Failed {
        /// The graph at the point of failure.
        graph: RunGraph,
        /// A human-readable explanation of why the run was halted.
        reason: String,
    },
}
