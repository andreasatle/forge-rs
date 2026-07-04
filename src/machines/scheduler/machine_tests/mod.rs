use super::*;
use crate::machines::scheduler::graph::{Node, RunGraph};
use crate::machines::scheduler::handler::SchedulerHandler;
use crate::machines::scheduler::request::RunRequest;
use crate::machines::scheduler::{
    ExhaustedAction, FailureKind, FailureReason, IntegrationFailure, IntegrationOutput,
    NodeFailure, NodeRequest, PlanOutput, RecoveryAction, RunConfig, WorkOutput, run_scheduler,
};
use crate::node_runner::StaticNodeRunner;

fn scheduler_handler() -> SchedulerHandler<StaticNodeRunner> {
    SchedulerHandler::new(StaticNodeRunner)
}

fn work_node(id: &str, objective: &str, deps: &[&str]) -> Node {
    Node {
        id: NodeId(id.to_string()),
        kind: NodeKind::Work,
        worker_role: None,
        objective: objective.to_string(),
        target_files: vec![],
        required_validation_targets: vec![],
        dependencies: deps.iter().map(|d| NodeId(d.to_string())).collect(),
        status: NodeStatus::Pending,
        attempt: 0,
        plan_depth: 0,
        model_tier: ModelTier::Cheap,
        summary: None,
        origin: NodeOrigin::Root,
        validation_plan: None,
        retry_feedback: None,
    }
}

fn plan_node(id: &str, objective: &str, deps: &[&str]) -> Node {
    Node {
        id: NodeId(id.to_string()),
        kind: NodeKind::Plan,
        worker_role: None,
        objective: objective.to_string(),
        target_files: vec![],
        required_validation_targets: vec![],
        dependencies: deps.iter().map(|d| NodeId(d.to_string())).collect(),
        status: NodeStatus::Pending,
        attempt: 0,
        plan_depth: 0,
        model_tier: ModelTier::Cheap,
        summary: None,
        origin: NodeOrigin::Root,
        validation_plan: None,
        retry_feedback: None,
    }
}

fn single_work_graph() -> RunGraph {
    RunGraph {
        nodes: vec![work_node("A", "do a thing", &[])],
        next_id: 0,
    }
}

fn chain_graph() -> RunGraph {
    RunGraph {
        nodes: vec![
            work_node("A", "step one", &[]),
            work_node("B", "step two", &["A"]),
            work_node("C", "step three", &["B"]),
        ],
        next_id: 0,
    }
}

fn running(mut graph: RunGraph, id: &str) -> RunGraph {
    for n in &mut graph.nodes {
        if n.id.0 == id {
            n.status = NodeStatus::Running;
        }
    }
    graph
}

fn active_node_id(graph: &RunGraph) -> Option<NodeId> {
    graph
        .nodes
        .iter()
        .find(|n| matches!(n.status, NodeStatus::Running | NodeStatus::Integrating))
        .map(|n| n.id.clone())
}

fn graph_with_filler_nodes(first: Node, total_nodes: usize) -> RunGraph {
    let mut nodes = vec![first];
    for i in 1..total_nodes {
        let mut node = work_node(&format!("filler-{i}"), "already done", &[]);
        node.status = NodeStatus::Completed;
        nodes.push(node);
    }
    RunGraph { nodes, next_id: 0 }
}

fn do_transition(
    state: SchedulerState,
    event: SchedulerEvent,
) -> Transition<SchedulerState, SchedulerEffect> {
    SchedulerMachine.transition(state, event)
}

mod checkpoint;
mod completion;
mod graph_limits;
mod integration;
mod planning;
mod recovery;
mod validation_plan;
