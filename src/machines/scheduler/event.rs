//! Events received by the scheduler machine.
//!
//! Events are facts delivered *to* the scheduler from the outside world.
//! They describe things that have already happened: a node finished, or the
//! runner is ready to start.
//!
//! This module owns only `SchedulerEvent`. Event payload types live in
//! `types.rs`; state and effect shapes live in `state.rs` and `effect.rs`.

use super::graph::NodeId;
use super::types::{IntegrationFailure, IntegrationOutput, NodeFailure, PlanOutput, WorkOutput};

/// Events that the scheduler machine can receive.
///
/// `Start` is a synthetic tick injected by the runner when the scheduler is
/// in `Active` state and no external result is pending. It drives the
/// machine from `Active` to `Waiting` (by dispatching a ready node), to
/// `Complete`, or to `Failed` — all without blocking on an external result.
///
/// Node and integration completion events carry real external results that
/// drive the `Waiting` state forward.
#[derive(Clone, Debug, PartialEq)]
pub enum SchedulerEvent {
    /// A synthetic tick that drives the `Active` state forward. The
    /// scheduler scans the graph, selects a ready node to dispatch, and
    /// moves to `Waiting`. If no node is ready the run fails; if all nodes
    /// are terminal the run completes.
    Start,
    /// A previously-dispatched plan node completed and wants new nodes inserted.
    PlanAccepted {
        /// The ID of the node that finished, used to verify it matches the
        /// graph's single active node.
        node_id: NodeId,
        /// The plan output to insert into the graph.
        plan: PlanOutput,
    },
    /// A previously-dispatched work node produced work that must be integrated.
    WorkAccepted {
        /// The ID of the node whose work was being integrated.
        node_id: NodeId,
        /// The work output to integrate before the node can complete.
        work: WorkOutput,
    },
    /// A previously-dispatched node could not complete.
    NodeFailed {
        /// The ID of the node that failed.
        node_id: NodeId,
        /// The failure and recovery direction.
        failure: NodeFailure,
    },
    /// A previously-dispatched integration completed successfully.
    IntegrationSucceeded {
        /// The ID of the node whose work was integrated.
        node_id: NodeId,
        /// The integration output to store on the node.
        output: IntegrationOutput,
    },
    /// A previously-dispatched integration could not complete.
    IntegrationFailed {
        /// The ID of the node whose work was being integrated.
        node_id: NodeId,
        /// The failure and recovery direction.
        failure: IntegrationFailure,
    },
}
