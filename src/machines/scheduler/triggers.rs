//! Team trigger evaluation and node spawning.
//!
//! Bridges the pure `evaluate_trigger` service function (which knows nothing
//! about the run graph) with `RunGraph` mutation. Called from `machine.rs`
//! after every `IntegrationSucceeded`/`PlannerTasksIntegrated` transition:
//! every configured team's trigger is evaluated against the current
//! manifest, and each team that should act gets a new Pending node inserted.

use crate::artifacts::TaskRecord;
use crate::config::TeamConfig;
use crate::language::derive_target_from_name;
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
///
/// A `ForTasks` candidate id is further gated on its `depends_on`: it is not
/// eligible to spawn until every id it depends on has a completion row from
/// every team in `run_config.terminal_teams` (see
/// [`retain_ids_with_satisfied_dependencies`]). `RunOnce` has no task or
/// `depends_on` to gate on, so this only applies to `ForTasks`.
///
/// Returns `Err` with a diagnostic detail if a `ForTasks` spawn's target
/// files could not be derived (see [`task_target_files`]); the caller is
/// responsible for routing that into `FailureReason::TargetDerivationFailed`.
pub(super) fn apply_team_triggers(
    mut graph: RunGraph,
    node_id: &NodeId,
    run_config: &RunConfig,
    manifest_tasks: &[TaskRecord],
) -> Result<RunGraph, String> {
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
                let ids = retain_ids_with_satisfied_dependencies(
                    ids,
                    manifest_tasks,
                    &run_config.terminal_teams,
                    &completions,
                );
                spawn_for_tasks(graph, node_id, team, ids, manifest_tasks)?
            }
        };
    }
    Ok(graph)
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

/// Filters `ids` (a `ForTasks` decision's candidate ids) down to those whose
/// `depends_on` tasks have all completed, i.e. every id in `depends_on` has a
/// completion row from every team in `terminal_teams`.
///
/// A candidate id always has a matching row in `manifest_tasks` (see
/// [`spawn_for_tasks`]'s panic message for why), so a missing `depends_on` id
/// entry in `manifest_tasks` (should never happen) is treated the same as
/// "not yet completed" rather than panicking here.
fn retain_ids_with_satisfied_dependencies(
    ids: Vec<String>,
    manifest_tasks: &[TaskRecord],
    terminal_teams: &[String],
    completions: &[TaskCompletion],
) -> Vec<String> {
    ids.into_iter()
        .filter(|id| {
            let record = manifest_tasks
                .iter()
                .find(|record| record.id == *id)
                .expect(
                    "a ForTasks id is only ever drawn from `completions`, which is built from \
                     `manifest_tasks` itself, so a matching row must already exist",
                );
            record.depends_on.iter().all(|dep_id| {
                terminal_teams.iter().all(|team| {
                    completions
                        .iter()
                        .any(|c| c.task_id == *dep_id && &c.team == team)
                })
            })
        })
        .collect()
}

/// Spawns a `Work` node per qualifying task id, skipping ids that already
/// have a non-terminal-failed node for this team.
///
/// Returns `Err` as soon as any qualifying id's target files cannot be
/// derived (see [`task_target_files`]), without inserting any of the
/// requests built so far — a node that can touch no file must never reach
/// the graph.
fn spawn_for_tasks(
    mut graph: RunGraph,
    node_id: &NodeId,
    team: &TeamConfig,
    ids: Vec<String>,
    manifest_tasks: &[TaskRecord],
) -> Result<RunGraph, String> {
    let requests: Vec<NodeRequest> = ids
        .into_iter()
        .filter(|id| !graph.has_active_team_node(&team.name, Some(id.as_str())))
        .map(|id| {
            let record = manifest_tasks.iter().find(|record| record.id == id).expect(
                "a ForTasks id is only ever drawn from `completions`, which is built from \
                     `manifest_tasks` itself, so a matching row must already exist",
            );
            let target_files = task_target_files(team, record)?;
            Ok(NodeRequest {
                id: new_node_id(),
                kind: NodeKind::Work,
                team: team.name.clone(),
                task_id: Some(id),
                adapter: team.adapter.clone(),
                northstar: team.northstar.clone(),
                worker_role: None,
                objective: record.objective.clone(),
                target_files,
                required_validation_targets: vec![],
                dependencies: vec![],
                validation_plan: None,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    if !requests.is_empty() {
        graph = graph.insert_children(node_id, requests);
    }
    Ok(graph)
}

/// Derives a `ForTasks`-spawned node's target files from its matched
/// manifest task's `name`, using `team`'s `name_target_rules` (merged from
/// its adapter's language plugins at config-load time — see
/// [`TeamConfig::name_target_rules`]).
///
/// `record` is always the planner `Task` row this id was decomposed from: a
/// `ForTasks` id only ever exists because a manifest row with that id and a
/// team already does, and the first such row for any id is always a planner
/// row (a `Work`-node completion for the id can only be recorded *after* the
/// planner row that gave rise to it). `EmptyName` validation guarantees a
/// planner `Task` row's `name` is never empty. Failing to match it against
/// any configured rule — including when `team` has no language plugins at
/// all, i.e. `name_target_rules` is empty — is `Err`, never a guessed
/// fallback: the caller must fail the run rather than spawn a node that can
/// touch no file.
fn task_target_files(team: &TeamConfig, record: &TaskRecord) -> Result<Vec<String>, String> {
    let name = record
        .name
        .as_deref()
        .expect("a ForTasks-matched record is always a planner Task row, which EmptyName validation guarantees carries a name");
    derive_target_from_name(&team.name_target_rules, name)
        .map(|target| vec![target])
        .ok_or_else(|| {
            format!(
                "team '{}': no name_target_rule matched task name '{name}' (id {})",
                team.name, record.id
            )
        })
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
