use super::*;
use std::path::PathBuf;

fn fixture_adapter_path(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("adapters")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

#[test]
fn unknown_team_adapter_path_fails_terminally() {
    // Invariant: `ForgeConfig::from_file` validates every team's adapter is
    // loadable at config-load time, so a load failure at dispatch time means
    // the filesystem changed underneath a validated config — unrecoverable,
    // so recovery must be Terminal rather than Retry/Split/ElevateModel.
    let provider = ScriptedProvider::from_strs(&[]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let mut request = work_request("do something");
    request.adapter = "/nonexistent/team-adapter.yaml".to_string();

    let result = runner.run_node(request, &NoopTelemetry);

    let NodeRunResult::Failed(failure) = result else {
        panic!("expected Failed, got a successful result");
    };
    assert!(
        matches!(failure.recovery, RecoveryAction::Terminal { .. }),
        "expected Terminal recovery, got {:?}",
        failure.recovery
    );
    assert!(
        failure.message.contains("/nonexistent/team-adapter.yaml"),
        "failure message must name the adapter path; got: {}",
        failure.message
    );
}

#[test]
fn unknown_team_northstar_path_fails_terminally() {
    let provider = ScriptedProvider::from_strs(&[]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let mut request = work_request("do something");
    request.adapter = fixture_adapter_path("coding.yaml");
    request.northstar = "/nonexistent/team-northstar.txt".to_string();

    let result = runner.run_node(request, &NoopTelemetry);

    let NodeRunResult::Failed(failure) = result else {
        panic!("expected Failed, got a successful result");
    };
    assert!(
        matches!(failure.recovery, RecoveryAction::Terminal { .. }),
        "expected Terminal recovery, got {:?}",
        failure.recovery
    );
    assert!(
        failure.message.contains("/nonexistent/team-northstar.txt"),
        "failure message must name the northstar path; got: {}",
        failure.message
    );
}

#[test]
fn team_adapter_and_northstar_override_the_top_level_ones() {
    // Invariant: a request naming a team's own adapter/northstar must run
    // under that team's wiring, not the run's top-level wiring — proven here
    // by the team's northstar text (and not the top-level one) appearing in
    // the rendered Plan prompt.
    let temp = TempDir::new("team-northstar-override");
    let northstar_path = temp.join("northstar.txt");
    std::fs::write(&northstar_path, "Team: ship the widget CLI.").unwrap();

    let plan = r#"{"kind":"plan","tasks":[{"id":"t1","objective":"decompose the widget work","name":"widget_work","depends_on":[]}]}"#;
    let provider = RecordingProvider::from_strs(&[
        plan,
        r#"{"status":"accepted","content":"plan looks good"}"#,
        r#"{"status":"accepted","content":"plan approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider)
        .with_northstar(Some("Top-level: ship the default CLI.".to_string()));

    let mut request = plan_request("Ship the widget CLI.");
    request.adapter = fixture_adapter_path("coding.yaml");
    request.northstar = northstar_path.to_string_lossy().into_owned();

    let result = runner.run_node(request, &NoopTelemetry);
    assert!(
        matches!(result, NodeRunResult::PlanAccepted(_)),
        "expected the plan to be accepted"
    );

    let prompts = provider.recorded_prompts();
    assert!(!prompts.is_empty(), "provider must have received prompts");
    let first = &prompts[0];
    assert!(
        first.contains("Northstar:\nTeam: ship the widget CLI."),
        "plan prompt must include the team's northstar; got:\n{first}"
    );
    assert!(
        !first.contains("Top-level: ship the default CLI."),
        "plan prompt must not include the run's top-level northstar; got:\n{first}"
    );
}
