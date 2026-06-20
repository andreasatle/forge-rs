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
//! - `RunGraph::next_id` is monotonically increasing and is the sole authority
//!   for minting new identifiers.

/// An opaque, stable identifier for a node in the run graph.
///
/// IDs are minted by incrementing `RunGraph::next_id` and are unique within a
/// run. The string form is human-readable but must not be parsed; its internal
/// structure is an implementation detail.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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
///   string. When accepted, the node is marked `Completed`.
#[derive(Clone, Debug, PartialEq)]
pub enum NodeKind {
    /// A planning node. Decomposes an objective into child nodes.
    Plan,
    /// An execution node. Carries out a concrete, bounded task.
    Work,
}

/// The model capability level to use when running a node.
///
/// `Cheap` is used for most work because cost compounds quickly across many
/// nodes. `Strong` is reserved for cases where the task has already proven too
/// difficult for the cheaper tier, or where plan quality directly determines
/// downstream work.
#[derive(Clone, Debug, PartialEq)]
pub enum ModelTier {
    /// The default, cost-efficient tier. Used for initial attempts.
    Cheap,
    /// The high-capability tier. Used for model-escalation retries and split
    /// recovery planning nodes.
    Strong,
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
#[derive(Clone, Debug, PartialEq)]
pub enum NodeStatus {
    /// Not yet eligible to run; waiting for dependencies to complete.
    Pending,
    /// Dispatched to a runner; awaiting a `NodeReturned` event.
    Running,
    /// Finished successfully. Dependencies on this node are now satisfiable.
    Completed,
    /// Finished unsuccessfully. The node is a permanent historical record.
    /// Recovery creates a replacement node rather than mutating this one.
    Failed,
    /// Skipped due to an upstream failure. Reserved for future cancellation
    /// propagation; not yet set by the scheduler.
    Cancelled,
}

/// A single unit of work in the run graph.
///
/// Each node carries everything the scheduler and runner need to dispatch,
/// track, and audit it. Fields are set at creation and updated only through
/// the explicit graph-mutation helpers on `SchedulerMachine`.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// Stable identifier, unique within the run graph.
    pub id: NodeId,
    /// Whether this node plans or executes.
    pub kind: NodeKind,
    /// A natural-language description of what this node should accomplish.
    /// Passed verbatim to the runner; preserved across retries and escalations.
    pub objective: String,
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
    /// The model capability level to use when running this node.
    pub model_tier: ModelTier,
    /// A brief human-readable description of the outcome, set when the node
    /// reaches `Completed`. `None` until then.
    pub summary: Option<String>,
}

/// The complete set of nodes for one Forge run, plus the ID counter.
///
/// The graph only grows: nodes are appended on plan expansion and recovery, but
/// never removed. This ensures the full execution history is always available
/// for debugging and audit.
#[derive(Clone, Debug, PartialEq)]
pub struct RunGraph {
    /// All nodes, in insertion order. The ordering has no semantic meaning;
    /// the scheduler scans the vec when computing ready sets.
    pub nodes: Vec<Node>,
    /// Monotonic counter used to mint fresh `NodeId`s without global state.
    /// Increment each time a node is inserted.
    pub next_id: u32,
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
/// NotStarted
///   └─ Start ──────────────────────→ SelectingReady
///                                          │
///                          ┌──────────────┤
///                          │              │ ready nodes found
///                          │              ↓
///                          │         Dispatching
///                          │              │ RunNode effect dispatched
///                          │              ↓
///                          │           Waiting
///                          │              │ NodeReturned event received
///                          │              │
///                          │  ┌───────────┤
///                          │  │ outcomes  │
///                          │  └───────────┘
///                          │    ↑ loops back to SelectingReady
///                          │
///                    ──────┴──────
///                   /             \
///              Complete          Failed
/// ```
#[derive(Clone, Debug, PartialEq)]
pub enum SchedulerState {
    /// The scheduler has been created but has not yet processed its first tick.
    NotStarted { graph: RunGraph },
    /// The scheduler is scanning the graph to find nodes whose dependencies are
    /// all `Completed` and that are themselves still `Pending`.
    SelectingReady { graph: RunGraph },
    /// At least one ready node has been identified. The scheduler will dispatch
    /// the first one on the next tick.
    ///
    /// Note: only one node is dispatched per tick even when multiple are ready.
    /// Parallel dispatch is a future concern.
    Dispatching { graph: RunGraph, ready: Vec<NodeId> },
    /// One node has been dispatched and the scheduler is waiting for its result.
    /// No further dispatch happens until `NodeReturned` arrives.
    Waiting { graph: RunGraph, running: NodeId },
    /// All nodes have reached a terminal status (`Completed`, `Failed`, or
    /// `Cancelled`) with no failures that halted the run. The graph is the
    /// complete execution record.
    Complete { graph: RunGraph },
    /// A `Terminal` failure was reported by a node. The run cannot continue.
    /// The graph is preserved for post-mortem inspection.
    Failed { graph: RunGraph, reason: String },
}
