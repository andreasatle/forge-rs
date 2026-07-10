//! Effects emitted by the scheduler machine.
//!
//! Effects are commands — things the scheduler *wants* to happen but cannot do
//! itself because they require I/O. The generic runner loop dispatches each
//! effect to the effect handler, which performs the work and converts the result
//! back into an event.
//!
//! This module does **not** own scheduler state or events; those live in
//! `state.rs` and `event.rs`.

use crate::validation::ValidationPlan;

use super::graph::{ModelTier, NodeId, NodeKind, RetryFeedback, TestPlanContext};
use super::types::{PlannerTaskOutput, WorkOutput};

/// Commands that the scheduler emits to the outside world.
///
/// Transition functions are pure, so they cannot run nodes or integrate work
/// directly. Instead they emit `SchedulerEffect` values that the handler layer
/// executes on their behalf.
#[derive(Clone, Debug, PartialEq)]
pub enum SchedulerEffect {
    /// Dispatch a single node to a runner for execution.
    ///
    /// The handler is responsible for running the node with the given
    /// parameters and returning a node result event when it finishes.
    /// Carrying `kind`, `objective`, `model_tier`, and `attempt` here means
    /// the handler does not need to re-read the graph.
    RunNode {
        /// The ID of the node to run, used to match the returned event.
        node_id: NodeId,
        /// The adapter-assigned worker role for a `Work` node (e.g.
        /// `"tester"`), or `None` for `Plan` nodes and default-role `Work`
        /// nodes. Carried here so the dispatch layer can render it into
        /// progress labels without re-reading the graph.
        worker_role: Option<String>,
        /// Whether the node should plan or execute.
        kind: NodeKind,
        /// Natural-language description of what the node should accomplish.
        objective: String,
        /// Structured target files this node is expected and allowed to touch.
        target_files: Vec<String>,
        /// Structured test-target planning context for this node.
        test_plan_context: TestPlanContext,
        /// The model capability level the runner should use.
        model_tier: ModelTier,
        /// Zero-based retry count; 0 on the first attempt.
        attempt: u32,
        /// Structured validation feedback to render into the prompt.
        ///
        /// `Some` only when the node is a retry triggered by a validation
        /// failure. The dispatch layer appends this to the objective text;
        /// the machine keeps the objective field itself immutable.
        retry_feedback: Option<RetryFeedback>,
    },

    /// Dispatch the work produced by a node to the integration handler.
    ///
    /// The handler integrates the work and returns an integration result event.
    /// The node remains `Integrating` until that event arrives.
    IntegrateWork {
        /// The ID of the node whose work is being integrated.
        node_id: NodeId,
        /// Natural-language description of what the node accomplished.
        ///
        /// Carried here (rather than re-read from the graph) so the task
        /// manifest record can be built without the integration layer
        /// depending on graph internals.
        objective: String,
        /// The work output to integrate.
        work: WorkOutput,
        /// Zero-based attempt number whose worktree should be integrated.
        attempt: u32,
        /// Structured target files declared for the node.
        target_files: Vec<String>,
        /// The node's declared validation contract.
        ///
        /// When present, integration executes this plan instead of the global
        /// handler-level validator.  `None` falls back to the global validator.
        validation_plan: Option<ValidationPlan>,
        /// The completing node's team, recorded into the manifest's
        /// `TaskRecord.team` so team trigger evaluation can see it.
        team: String,
    },

    /// Record a completed `Plan` node's `Task`-kind output into the task
    /// manifest.
    ///
    /// Parallel to `IntegrateWork`: the handler records the tasks and
    /// returns an integration result event. The node remains `Integrating`
    /// until that event arrives.
    IntegratePlannerTasks {
        /// The ID of the plan node whose task records are being integrated.
        node_id: NodeId,
        /// The planner-produced task records to record into the manifest.
        tasks: Vec<PlannerTaskOutput>,
        /// The completing node's team, recorded into each resulting
        /// `TaskRecord.team` so team trigger evaluation can see it.
        team: String,
    },
}
