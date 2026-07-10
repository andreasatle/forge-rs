use super::*;

fn completion(task_id: &str, team: &str) -> TaskCompletion {
    TaskCompletion {
        task_id: task_id.to_string(),
        team: team.to_string(),
    }
}

fn after_each(teams: &[&str]) -> Trigger {
    Trigger::AfterEach(teams.iter().map(|t| t.to_string()).collect())
}

/// `start` fires on an empty manifest: the team has no rows yet, so it has
/// not run this run.
#[test]
fn start_should_run_on_empty_manifest() {
    let decision = evaluate_trigger(&Trigger::Start, "planner", &[]);
    assert_eq!(decision, TriggerDecision::RunOnce { should_run: true });
}

/// Once `planner` has recorded any row, `start` must not fire again — this
/// is what makes re-evaluating against a growing manifest safe.
#[test]
fn start_does_not_run_again_once_team_has_a_row() {
    let completions = vec![completion("t1", "planner")];
    let decision = evaluate_trigger(&Trigger::Start, "planner", &completions);
    assert_eq!(decision, TriggerDecision::RunOnce { should_run: false });
}

/// `start` is scoped per team: another team's rows don't count as this
/// team having already run.
#[test]
fn start_ignores_other_teams_rows() {
    let completions = vec![completion("t1", "implement")];
    let decision = evaluate_trigger(&Trigger::Start, "planner", &completions);
    assert_eq!(decision, TriggerDecision::RunOnce { should_run: true });
}

/// `after_each(team)` fires for every task id that has a row from `team`.
#[test]
fn after_each_single_team_fires_for_each_of_its_task_ids() {
    let trigger = after_each(&["planner"]);
    let completions = vec![completion("t1", "planner"), completion("t2", "planner")];
    let decision = evaluate_trigger(&trigger, "implement", &completions);
    assert_eq!(
        decision,
        TriggerDecision::ForTasks(vec!["t1".to_string(), "t2".to_string()])
    );
}

/// A task id is excluded once the calling team already has its own row for
/// it, so re-running against a growing manifest never double-fires.
#[test]
fn after_each_excludes_ids_the_calling_team_already_acted_on() {
    let trigger = after_each(&["planner"]);
    let completions = vec![
        completion("t1", "planner"),
        completion("t2", "planner"),
        completion("t1", "implement"),
    ];
    let decision = evaluate_trigger(&trigger, "implement", &completions);
    assert_eq!(decision, TriggerDecision::ForTasks(vec!["t2".to_string()]));
}

/// `after_each(a, b)` only fires for a task id once every named team has a
/// row for it — a row from just one of them is not enough.
#[test]
fn after_each_multi_team_requires_every_named_team() {
    let trigger = after_each(&["implement", "create_test"]);
    let completions = vec![
        completion("t1", "implement"),
        completion("t1", "create_test"),
        completion("t2", "implement"),
    ];
    let decision = evaluate_trigger(&trigger, "pass_tests", &completions);
    assert_eq!(decision, TriggerDecision::ForTasks(vec!["t1".to_string()]));
}

/// Multiple rows from the same required team for the same task id (e.g. a
/// retry) must not produce duplicate entries in the result.
#[test]
fn after_each_dedups_repeated_rows_for_the_same_task_id() {
    let trigger = after_each(&["planner"]);
    let completions = vec![
        completion("t1", "planner"),
        completion("t1", "planner"),
        completion("t2", "planner"),
    ];
    let decision = evaluate_trigger(&trigger, "implement", &completions);
    assert_eq!(
        decision,
        TriggerDecision::ForTasks(vec!["t1".to_string(), "t2".to_string()])
    );
}

/// An empty manifest satisfies no `after_each` trigger.
#[test]
fn after_each_returns_empty_for_empty_manifest() {
    let trigger = after_each(&["planner"]);
    let decision = evaluate_trigger(&trigger, "implement", &[]);
    assert_eq!(decision, TriggerDecision::ForTasks(Vec::new()));
}

/// Rows from unrelated teams don't satisfy or interfere with the trigger.
#[test]
fn after_each_ignores_rows_from_teams_outside_the_trigger() {
    let trigger = after_each(&["planner"]);
    let completions = vec![
        completion("t1", "planner"),
        completion("t1", "some_other_team"),
    ];
    let decision = evaluate_trigger(&trigger, "implement", &completions);
    assert_eq!(decision, TriggerDecision::ForTasks(vec!["t1".to_string()]));
}
