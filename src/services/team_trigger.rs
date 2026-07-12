//! Evaluates a team's [`Trigger`] against a snapshot of the `.forge/tasks.json`
//! manifest.
//!
//! A manifest row is both a team's declaration and its completion record
//! for a task id. `after_teams(team)` is satisfied for a task id once that
//! team has recorded a row for it; `after_teams(a, b)` is satisfied once
//! every named team has.

use std::collections::HashSet;

use crate::config::Trigger;

/// One manifest row, reduced to the fields trigger evaluation needs: which
/// team recorded a completion, for which task id.
///
/// Deliberately narrower than `artifacts::TaskRecord` (the full manifest
/// row persisted to disk) — trigger evaluation doesn't need the objective,
/// commit, or timestamp, and rows with no team yet (pre-multi-team-scheduler
/// records) carry no trigger meaning, so this type has no place for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCompletion {
    /// The task id this row is for.
    pub task_id: String,
    /// The team that recorded this row.
    pub team: String,
}

/// What a team should do next, per its trigger and the current manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerDecision {
    /// `Trigger::Start` is not task-scoped: it fires once per run, before
    /// any task exists for it to reference. There is no manifest row to key
    /// a "task id to act on" off of, so instead of forcing this into the
    /// `ForTasks` shape, this variant reports whether the team's one-time
    /// run has already happened. A team has "already run" once it has
    /// recorded any row of its own — the same "exclude ids I've already
    /// acted on" idea `ForTasks` uses, generalized to a team with no task
    /// ids at all yet.
    RunOnce {
        /// `true` if the team has not yet recorded any manifest row and
        /// should run; `false` if it already has.
        should_run: bool,
    },
    /// `Trigger::AfterTeams` fires once per task id that satisfies the
    /// trigger, excluding ids the calling team already has a row for.
    ForTasks(Vec<String>),
}

/// Evaluates `trigger` for `team` against `completions`, the current
/// `.forge/tasks.json` rows reduced to task id + team.
///
/// Safe to call repeatedly against a growing manifest: task ids `team`
/// already has a row for are excluded from `ForTasks`, and `RunOnce` reports
/// `should_run: false` once `team` has any row, so re-running never
/// double-fires.
pub fn evaluate_trigger(
    trigger: &Trigger,
    team: &str,
    completions: &[TaskCompletion],
) -> TriggerDecision {
    match trigger {
        Trigger::Start => {
            let already_ran = completions.iter().any(|c| c.team == team);
            TriggerDecision::RunOnce {
                should_run: !already_ran,
            }
        }
        Trigger::AfterTeams(required_teams) => {
            TriggerDecision::ForTasks(ready_task_ids(required_teams, team, completions))
        }
    }
}

fn ready_task_ids(
    required_teams: &[String],
    team: &str,
    completions: &[TaskCompletion],
) -> Vec<String> {
    let own_ids: HashSet<&str> = completions
        .iter()
        .filter(|c| c.team == team)
        .map(|c| c.task_id.as_str())
        .collect();

    let mut seen = HashSet::new();
    let mut ready = Vec::new();
    for completion in completions {
        if own_ids.contains(completion.task_id.as_str())
            || !seen.insert(completion.task_id.as_str())
        {
            continue;
        }
        let satisfied = required_teams.iter().all(|required| {
            completions
                .iter()
                .any(|c| c.task_id == completion.task_id && &c.team == required)
        });
        if satisfied {
            ready.push(completion.task_id.clone());
        }
    }
    ready
}

#[cfg(test)]
#[path = "team_trigger_tests.rs"]
mod tests;
