//! Team trigger evaluation and node spawning.
//!
//! Bridges the pure `evaluate_trigger` service function (which knows nothing
//! about the run graph) with `RunGraph` mutation. Called from `machine.rs`
//! after every `IntegrationSucceeded`/`PlannerTasksIntegrated` transition:
//! every configured team's trigger is evaluated against the current
//! manifest, and each team that should act gets a new Pending node inserted.

use crate::artifacts::TaskRecord;
use crate::config::TeamConfig;
use crate::services::team_trigger::{TaskCompletion, TriggerDecision, evaluate_trigger};

use super::config::RunConfig;
use super::graph::{NodeId, NodeKind, NodeOrigin, RunGraph, new_node_id};
use super::types::NodeRequest;

/// Evaluates every configured team's trigger against `manifest_tasks` and
/// inserts a Pending node for each team that should act.
///
/// Each spawned node is inserted via `RunGraph::insert_children`, anchored at
/// `node_id` (the node whose completion triggered this re-evaluation) purely
/// for `plan_depth` computation — spawned nodes are not children of that node
/// in any other sense.
///
/// Dedup is graph-checked, not manifest-checked: a (team, task) pair already
/// represented by a non-terminal-failed node is skipped even when the
/// manifest has no row for it yet. This matters because triggers are
/// re-evaluated on every task completion, not once per run — without the
/// graph check, a still-in-flight node (no manifest row yet) would be
/// re-spawned on every unrelated completion.
pub(super) fn apply_team_triggers(
    mut graph: RunGraph,
    node_id: &NodeId,
    run_config: &RunConfig,
    manifest_tasks: &[TaskRecord],
) -> RunGraph {
    let completions: Vec<TaskCompletion> = manifest_tasks
        .iter()
        .filter_map(|record| {
            record.team.clone().map(|team| TaskCompletion {
                task_id: record.id.clone(),
                team,
            })
        })
        .collect();

    for team in &run_config.teams {
        graph = match evaluate_trigger(&team.trigger, &team.name, &completions) {
            TriggerDecision::RunOnce { should_run } => {
                spawn_run_once(graph, node_id, team, should_run)
            }
            TriggerDecision::ForTasks(ids) => {
                spawn_for_tasks(graph, node_id, team, ids, manifest_tasks)
            }
        };
    }
    graph
}

/// Spawns a team's Start-triggered initial `Plan` node, unless it should not
/// run yet or a non-terminal-failed node for this team already exists.
fn spawn_run_once(
    graph: RunGraph,
    node_id: &NodeId,
    team: &TeamConfig,
    should_run: bool,
) -> RunGraph {
    if !should_run || graph.has_active_team_node(&team.name, None) {
        return graph;
    }
    let request = NodeRequest {
        id: new_node_id(),
        kind: NodeKind::Plan,
        team: team.name.clone(),
        task_id: None,
        adapter: team.adapter.clone(),
        northstar: team.northstar.clone(),
        worker_role: None,
        objective: root_objective(&graph),
        target_files: vec![],
        required_validation_targets: vec![],
        dependencies: vec![],
        validation_plan: None,
    };
    graph.insert_children(node_id, vec![request])
}

/// Spawns a `Work` node per qualifying task id, skipping ids that already
/// have a non-terminal-failed node for this team.
fn spawn_for_tasks(
    mut graph: RunGraph,
    node_id: &NodeId,
    team: &TeamConfig,
    ids: Vec<String>,
    manifest_tasks: &[TaskRecord],
) -> RunGraph {
    let requests: Vec<NodeRequest> = ids
        .into_iter()
        .filter(|id| !graph.has_active_team_node(&team.name, Some(id.as_str())))
        .map(|id| {
            let objective = manifest_tasks
                .iter()
                .find(|record| record.id == id)
                .map(|record| record.objective.clone())
                .unwrap_or_default();
            NodeRequest {
                id: new_node_id(),
                kind: NodeKind::Work,
                team: team.name.clone(),
                task_id: Some(id),
                adapter: team.adapter.clone(),
                northstar: team.northstar.clone(),
                worker_role: None,
                objective,
                target_files: vec![],
                required_validation_targets: vec![],
                dependencies: vec![],
                validation_plan: None,
            }
        })
        .collect();
    if !requests.is_empty() {
        graph = graph.insert_children(node_id, requests);
    }
    graph
}

/// The objective the run was started with, per the graph's `Root` node.
///
/// Reused as the objective for a `RunOnce`-spawned node: per the multi-team
/// design, a Start-triggered team is the decomposer/planner role, so it
/// plans from the same top-level objective the single-team root node does.
fn root_objective(graph: &RunGraph) -> String {
    graph
        .nodes
        .iter()
        .find(|n| n.origin == NodeOrigin::Root)
        .map(|n| n.objective.clone())
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "triggers_tests.rs"]
mod tests;
