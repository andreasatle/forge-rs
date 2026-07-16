use super::*;

fn team(name: &str, trigger: Trigger) -> TeamConfig {
    let kind = match trigger {
        Trigger::Start => NodeKind::Plan,
        Trigger::AfterTeams(_) => NodeKind::Work,
    };
    TeamConfig {
        name: name.to_string(),
        northstar: String::new(),
        adapter: String::new(),
        kind,
        trigger,
        language_plugins: std::collections::BTreeMap::new(),
        language: String::new(),
        derives_target: false,
    }
}

fn after_teams(names: &[&str]) -> Trigger {
    Trigger::AfterTeams(names.iter().map(|s| s.to_string()).collect())
}

#[test]
fn chain_topology_has_a_single_terminal_team_at_the_end() {
    // Invariant: in a straight-line chain, only the team nothing else names
    // in its after_teams is terminal — every upstream team is referenced by
    // something downstream.
    let teams = vec![
        team("planner", Trigger::Start),
        team("implement", after_teams(&["planner"])),
        team("create_test", after_teams(&["planner"])),
        team("pass_tests", after_teams(&["implement", "create_test"])),
    ];
    let terminal = compute_terminal_teams(&teams).unwrap();
    assert_eq!(terminal, vec!["pass_tests".to_string()]);
}

#[test]
fn branching_topology_can_have_multiple_terminal_teams() {
    // Invariant: terminal-ness is "nothing downstream references me", not
    // "there is exactly one end of the graph" — independent branches off a
    // shared root are each terminal.
    let teams = vec![
        team("planner", Trigger::Start),
        team("branch_a", after_teams(&["planner"])),
        team("branch_b", after_teams(&["planner"])),
    ];
    let mut terminal = compute_terminal_teams(&teams).unwrap();
    terminal.sort();
    assert_eq!(
        terminal,
        vec!["branch_a".to_string(), "branch_b".to_string()]
    );
}

#[test]
fn direct_cycle_fails_to_compute() {
    // Invariant: a team-trigger cycle (a's after_teams chain transitively
    // refers back to a) must fail loudly rather than silently produce a team
    // that could never be scheduled.
    let teams = vec![
        team("a", after_teams(&["b"])),
        team("b", after_teams(&["a"])),
    ];
    let err = compute_terminal_teams(&teams).unwrap_err();
    assert!(
        err.to_string().contains("cycle"),
        "error must explain that a team trigger cycle was found; got: {err}"
    );
}

#[test]
fn self_referential_cycle_fails_to_compute() {
    // Invariant: a team naming itself in its own after_teams is a cycle of
    // length one, not a degenerate no-op.
    let teams = vec![team("a", after_teams(&["a"]))];
    let err = compute_terminal_teams(&teams).unwrap_err();
    assert!(
        err.to_string().contains("cycle"),
        "error must explain that a team trigger cycle was found; got: {err}"
    );
}

#[test]
fn no_teams_yields_no_terminal_teams() {
    let terminal = compute_terminal_teams(&[]).unwrap();
    assert!(terminal.is_empty());
}
