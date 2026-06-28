use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::artifacts::{ArtifactView, FileChange};
use crate::machines::scheduler::NodeKind;
use crate::providers::types::{ProviderError, ProviderErrorKind, ProviderResponse};

fn parse_role_response(raw_response: &str) -> RoleResult {
    super::super::parser::try_parse_role_response(raw_response).unwrap_or_else(|reason| {
        RoleResult::Failed {
            kind: FailureKind::ProtocolFailure,
            reason,
        }
    })
}

fn role_json(status: &str, field: &str, value: &str) -> String {
    serde_json::json!({ "status": status, field: value }).to_string()
}

fn accepted_json(content: &str) -> String {
    role_json("accepted", "content", content)
}

fn rejected_json(reason: &str) -> String {
    role_json("rejected", "reason", reason)
}

fn assert_parse_failed(case: &str, input: &str) -> String {
    let result = parse_role_response(input);
    let RoleResult::Failed { reason, .. } = result else {
        panic!("[{case}] response must produce Failed, got {result:?}");
    };
    reason
}

fn assert_parse_failed_reason_contains(case: &str, input: &str, expected: &str) -> String {
    let reason = assert_parse_failed(case, input);
    assert!(
        reason.contains(expected),
        "[{case}] failure reason must mention '{expected}'; got: {reason}"
    );
    reason
}

fn assert_placeholder_rejected(case: &str, input: &str) -> String {
    assert_parse_failed_reason_contains(case, input, "placeholder")
}

fn assert_too_short(case: &str, input: &str) {
    assert_parse_failed_reason_contains(case, input, "too short");
}

// --- fake providers ---

struct FailingProvider {
    kind: ProviderErrorKind,
    message: String,
}

impl ProviderClient for FailingProvider {
    fn call(&self, _req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        Err(ProviderError {
            kind: self.kind.clone(),
            message: self.message.clone(),
        })
    }
}

struct ScriptedProvider {
    responses: RefCell<VecDeque<String>>,
    requests: RefCell<Vec<ProviderRequest>>,
}

impl ScriptedProvider {
    fn from_strs(responses: &[&str]) -> Self {
        Self {
            responses: RefCell::new(responses.iter().map(|s| s.to_string()).collect()),
            requests: RefCell::new(Vec::new()),
        }
    }
}

impl ProviderClient for ScriptedProvider {
    fn call(&self, req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        self.requests.borrow_mut().push(req);
        let content = self
            .responses
            .borrow_mut()
            .pop_front()
            .expect("ScriptedProvider: responses exhausted");
        Ok(ProviderResponse {
            content,
            finish_reason: None,
        })
    }
}

// --- parse_role_response unit tests ---

#[test]
fn json_accepted_response_maps_to_role_accepted() {
    let result = parse_role_response(r#"{"status":"accepted","content":"draft output"}"#);
    assert!(
        matches!(result, RoleResult::Accepted { ref content } if content == "draft output"),
        "expected Accepted {{ 'draft' }}, got {result:?}"
    );
}

#[test]
fn json_rejected_response_maps_to_role_rejected() {
    let result = parse_role_response(r#"{"status":"rejected","reason":"needs changes"}"#);
    assert!(
        matches!(result, RoleResult::Rejected { ref reason } if reason == "needs changes"),
        "expected Rejected {{ 'needs changes' }}, got {result:?}"
    );
}

#[test]
fn empty_role_response_field_fails() {
    for (case, input) in [
        ("accepted/content", accepted_json("")),
        ("rejected/reason", rejected_json("")),
    ] {
        assert_parse_failed(&case, &input);
    }
}

#[test]
fn dot_placeholder_fails_without_including_raw() {
    for (case, input) in [
        ("accepted/content", accepted_json("...")),
        ("rejected/reason", rejected_json("...")),
    ] {
        let reason = assert_placeholder_rejected(case, &input);
        assert!(
            !reason.contains("raw:"),
            "[{case}] failure reason must not include 'raw:'; got: {reason}"
        );
    }
}

#[test]
fn framework_placeholders_are_rejected() {
    for (case, input) in [
        (
            "accepted/response-summary",
            accepted_json("$RESPONSE_SUMMARY"),
        ),
        (
            "rejected/reason-for-rejection",
            rejected_json("$REASON_FOR_REJECTION"),
        ),
        (
            "rejected/min-length-dollar-reason",
            rejected_json("$REASON"),
        ),
    ] {
        assert_parse_failed_reason_contains(case, &input, "framework placeholder");
    }
}

// --- minimum-length guard tests ---

#[test]
fn too_short_role_response_field_fails() {
    for (case, input) in [
        ("accepted/1-char", accepted_json("{")),
        ("accepted/2-char", accepted_json("ok")),
        ("rejected/1-char", rejected_json("{")),
        ("rejected/2-char", rejected_json("ok")),
    ] {
        assert_too_short(case, &input);
    }
}

#[test]
fn meaningful_accepted_content_passes() {
    let result = parse_role_response(
        r#"{"status":"accepted","content":"Created src/main.rs with a Rust program that prints a haiku."}"#,
    );
    assert!(
        matches!(result, RoleResult::Accepted { .. }),
        "long meaningful content must be accepted, got {result:?}"
    );
}

#[test]
fn min_length_boundary_fields_pass() {
    // Exactly MIN_CONTENT_LENGTH characters must be accepted.
    let value = "a".repeat(super::MIN_CONTENT_LENGTH);
    for (case, input, expected_status) in [
        ("accepted/content", accepted_json(&value), "accepted"),
        ("rejected/reason", rejected_json(&value), "rejected"),
    ] {
        let result = parse_role_response(&input);
        let matches_expected = match expected_status {
            "accepted" => matches!(&result, RoleResult::Accepted { .. }),
            "rejected" => matches!(&result, RoleResult::Rejected { .. }),
            _ => unreachable!("unknown expected status"),
        };
        assert!(
            matches_expected,
            "[{case}] field at exactly MIN_CONTENT_LENGTH must be accepted, got {result:?}"
        );
    }
}

#[test]
fn arbitrary_angle_bracket_text_is_allowed() {
    let result = parse_role_response(r#"{"status":"accepted","content":"<p>hello world</p>"}"#);
    assert!(
        matches!(result, RoleResult::Accepted { .. }),
        "arbitrary angle-bracket content must be accepted, got {result:?}"
    );
}

#[test]
fn html_like_content_is_allowed() {
    let result =
        parse_role_response(r#"{"status":"accepted","content":"<html><body>ok</body></html>"}"#);
    assert!(
        matches!(result, RoleResult::Accepted { .. }),
        "HTML-like content must be accepted, got {result:?}"
    );
}

#[test]
fn xml_like_content_is_allowed() {
    let result =
        parse_role_response(r#"{"status":"accepted","content":"<root><item>data</item></root>"}"#);
    assert!(
        matches!(result, RoleResult::Accepted { .. }),
        "XML-like content must be accepted, got {result:?}"
    );
}

#[test]
fn normal_summary_is_allowed() {
    let result = parse_role_response(
        r#"{"status":"accepted","content":"Summary of changes made to the file."}"#,
    );
    assert!(
        matches!(result, RoleResult::Accepted { .. }),
        "normal summary content must be accepted, got {result:?}"
    );
}

#[test]
fn json_unknown_status_fails() {
    let result = parse_role_response(r#"{"status":"pending","content":"draft"}"#);
    assert!(
        matches!(result, RoleResult::Failed { .. }),
        "unknown status must produce Failed, got {result:?}"
    );
}

#[test]
fn malformed_json_fails() {
    let result = parse_role_response("not json at all");
    assert!(
        matches!(result, RoleResult::Failed { .. }),
        "malformed JSON must produce Failed, got {result:?}"
    );
}

#[test]
fn fenced_json_parses() {
    let input = "```json\n{\"status\":\"accepted\",\"content\":\"draft output\"}\n```";
    let result = parse_role_response(input);
    assert!(
        matches!(result, RoleResult::Accepted { ref content } if content == "draft output"),
        "fenced JSON must parse to Accepted {{ 'draft' }}, got {result:?}"
    );
}

#[test]
fn preamble_then_json_is_protocol_failure() {
    let input = "Here is the result:\n{\"status\":\"accepted\",\"content\":\"draft\"}";
    let result = parse_role_response(input);
    assert!(
        matches!(result, RoleResult::Failed { .. }),
        "preamble before JSON must be a protocol failure, got {result:?}"
    );
}

#[test]
fn json_with_trailing_text_parses_first_object() {
    let input = r#"{"status":"accepted","content":"draft output"}\nSome trailing explanation the model added."#;
    let result = parse_role_response(input);
    assert!(
        matches!(result, RoleResult::Accepted { ref content } if content == "draft output"),
        "trailing text after JSON must be ignored, got {result:?}"
    );
}

#[test]
fn json_with_braces_inside_string_parses() {
    let input = r#"{"status":"accepted","content":"use {} in templates"}"#;
    let result = parse_role_response(input);
    assert!(
        matches!(result, RoleResult::Accepted { ref content } if content == "use {} in templates"),
        "braces inside string must not affect depth count, got {result:?}"
    );
}

#[test]
fn unbalanced_json_object_fails() {
    let result = parse_role_response(r#"{"status":"accepted","content":"oops""#);
    assert!(
        matches!(result, RoleResult::Failed { .. }),
        "unbalanced JSON must produce Failed, got {result:?}"
    );
}

// --- trailing-whitespace robustness ---

#[test]
fn role_response_with_trailing_newline_parses() {
    let input = "{\"status\":\"accepted\",\"content\":\"The task was completed successfully.\"}\n";
    let result = parse_role_response(input);
    assert!(
        matches!(result, RoleResult::Accepted { .. }),
        "trailing newline must not cause role response parse failure, got {result:?}"
    );
}

#[test]
fn role_response_with_trailing_spaces_and_tabs_parses() {
    let input =
        "{\"status\":\"accepted\",\"content\":\"The task was completed successfully.\"}  \t  ";
    let result = parse_role_response(input);
    assert!(
        matches!(result, RoleResult::Accepted { .. }),
        "trailing spaces/tabs must not cause role response parse failure, got {result:?}"
    );
}

#[test]
fn role_response_rejected_with_trailing_whitespace_parses() {
    let input =
        "{\"status\":\"rejected\",\"reason\":\"The output does not meet requirements.\"}\n\n";
    let result = parse_role_response(input);
    assert!(
        matches!(result, RoleResult::Rejected { .. }),
        "rejected response with trailing whitespace must parse, got {result:?}"
    );
}

#[test]
fn role_response_with_leading_and_trailing_whitespace_parses() {
    let input =
        "\n  {\"status\":\"accepted\",\"content\":\"The task was completed successfully.\"}  \n";
    let result = parse_role_response(input);
    assert!(
        matches!(result, RoleResult::Accepted { .. }),
        "leading and trailing whitespace must not prevent parsing, got {result:?}"
    );
}

// --- ProviderRoleRunner tests ---

#[test]
fn provider_error_maps_to_failed() {
    for (case, kind, message) in [
        ("Retryable", ProviderErrorKind::Retryable, "rate limited"),
        ("Terminal", ProviderErrorKind::Terminal, "auth error"),
    ] {
        let runner = ProviderRoleRunner::new(FailingProvider {
            kind,
            message: message.to_string(),
        });
        let result = runner
            .run_role(
                make_role_request(DeliberationRole::Producer, "write a poem"),
                &crate::telemetry::NoopTelemetry,
            )
            .result;
        assert!(
            matches!(result, RoleResult::Failed { .. }),
            "[{case}] provider error must map to Failed, got {result:?}"
        );
    }
}

#[test]
fn role_prompt_includes_feedback() {
    let feedback = vec![RevisionFeedback {
        reason: "too vague".to_string(),
    }];
    let default = RolePolicy::default();
    let prompt = render_role_prompt(
        &default.worker_producer_system,
        &DeliberationRole::Producer,
        "write a poem",
        None,
        None,
        &feedback,
        &[],
    );
    assert!(
        prompt.contains("too vague"),
        "expected prompt to include feedback reason 'too vague', got: {prompt}"
    );
    assert!(
        prompt.contains("write a poem"),
        "expected prompt to include objective, got: {prompt}"
    );
    assert!(
        prompt.contains("\"status\""),
        "expected prompt to include JSON schema instructions, got: {prompt}"
    );
}

#[test]
fn provider_role_runner_retries_malformed_json() {
    let provider = ScriptedProvider::from_strs(&[
        "invalid text",
        r#"{"status":"accepted","content":"recovered"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let result = runner
        .run_role(
            producer_request("recover output"),
            &crate::telemetry::NoopTelemetry,
        )
        .result;

    assert!(matches!(result, RoleResult::Accepted { ref content } if content == "recovered"));
    assert_eq!(provider.requests.borrow().len(), 2);
}

#[test]
fn retry_prompt_contains_parse_error() {
    let provider = ScriptedProvider::from_strs(&[
        "invalid text",
        r#"{"status":"accepted","content":"recovered"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        producer_request("recover output"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let retry_prompt = &requests[1].prompt;
    // "invalid text" starts with 'i', not '{', so preamble check fires.
    assert!(retry_prompt.contains("preamble text is not permitted"));
    assert!(!retry_prompt.contains("invalid text"));
    assert!(retry_prompt.contains("Objective: recover output"));
    assert!(retry_prompt.contains("$RESPONSE_SUMMARY"));
    assert!(!retry_prompt.contains("\"...\""));
}

#[test]
fn retry_limit_returns_failure() {
    let provider = ScriptedProvider::from_strs(&["invalid one", "invalid two", "invalid three"]);
    let runner = ProviderRoleRunner::new(&provider);

    let result = runner
        .run_role(
            producer_request("never valid"),
            &crate::telemetry::NoopTelemetry,
        )
        .result;

    assert!(matches!(result, RoleResult::Failed { .. }));
    assert_eq!(provider.requests.borrow().len(), 3);
}

#[test]
fn provider_role_runner_returns_semantic_rejection_without_retry() {
    let provider =
        ScriptedProvider::from_strs(&[r#"{"status":"rejected","reason":"needs revision"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    let result = runner
        .run_role(
            referee_request("review output", "draft", "review"),
            &crate::telemetry::NoopTelemetry,
        )
        .result;

    assert!(
        matches!(result, RoleResult::Rejected { ref reason } if reason == "needs revision"),
        "semantic rejection must not retry, got {result:?}"
    );
    assert_eq!(provider.requests.borrow().len(), 1);
}

#[test]
fn protocol_retry_records_role_layer_telemetry() {
    use crate::telemetry::{TelemetryEvent, VecTelemetry};

    let provider = ScriptedProvider::from_strs(&[
        "invalid text",
        r#"{"status":"accepted","content":"recovered"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);
    let telemetry = VecTelemetry::new();

    runner.run_role(producer_request("recover output"), &telemetry);

    let records = telemetry.records();
    assert!(records.iter().all(|record| record.source == "RoleMachine"));
    // "invalid text" starts with 'i', not '{', so preamble check fires.
    assert!(records.iter().any(|record| matches!(
        &record.event,
        TelemetryEvent::RolePromptRendered {
            attempt_count: 2,
            prompt,
        } if prompt.contains("preamble text is not permitted")
    )));
    assert!(records.iter().any(|record| matches!(
        &record.event,
        TelemetryEvent::ProviderResponseReceived {
            attempt_count: 1,
            raw_response,
        } if raw_response == "invalid text"
    )));
    assert!(records.iter().any(|record| matches!(
        &record.event,
        TelemetryEvent::ParseFailed { parse_error, .. }
            if parse_error.contains("preamble text is not permitted")
    )));
    assert!(records.iter().any(|record| matches!(
        &record.event,
        TelemetryEvent::ProtocolRetry {
            attempt_count: 2,
            ..
        }
    )));
    assert!(records.iter().any(|record| matches!(
        record.event,
        TelemetryEvent::ParseSucceeded { attempt_count: 2 }
    )));
}

#[test]
fn role_events_use_role_machine_source() {
    use crate::telemetry::FileTelemetry;

    let dir = std::env::temp_dir().join("forge-role-machine-source-test");
    let _ = std::fs::remove_dir_all(&dir);
    let telemetry = FileTelemetry::new(dir.clone());
    let provider = ScriptedProvider::from_strs(&[
        "invalid text",
        r#"{"status":"accepted","content":"recovered"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(producer_request("recover output"), &telemetry);

    assert!(
        dir.join("000001--role-machine--producer--role-prompt-rendered.txt")
            .exists()
    );
    assert!(
        dir.join("000003--role-machine--producer--parse-failed.txt")
            .exists()
    );
    assert!(
        dir.join("000004--role-machine--producer--protocol-retry.txt")
            .exists()
    );
}

// --- git helpers for tool tests that need a real ArtifactView ---

static NEXT_VIEW_ID: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let id = NEXT_VIEW_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "forge-runner-tools-{label}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &PathBuf, args: &[&str]) {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git failed");
}

fn git_rev(dir: &PathBuf) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .expect("git rev-parse failed");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn make_view(label: &str) -> (TempDir, ArtifactView) {
    make_view_with_entries(label, &[("hello.txt", b"hello world\n".as_slice())])
}

fn make_view_with_entries(label: &str, entries: &[(&str, &[u8])]) -> (TempDir, ArtifactView) {
    let temp = TempDir::new(label);
    let seed = temp.0.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "--quiet", "--initial-branch=main"]);
    git(&seed, &["config", "user.name", "Test"]);
    git(&seed, &["config", "user.email", "test@example.invalid"]);
    for (path, content) in entries {
        let full_path = seed.join(path);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full_path, content).unwrap();
    }
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "--quiet", "-m", "init"]);
    let bare = temp.0.join("bare.git");
    Command::new("git")
        .args(["clone", "--quiet", "--bare"])
        .arg(&seed)
        .arg(&bare)
        .status()
        .expect("git clone --bare failed");
    let sha = git_rev(&bare);
    (
        temp,
        ArtifactView {
            repo_path: bare,
            commit_sha: sha,
        },
    )
}

fn producer_prompt_for_targets(
    view: ArtifactView,
    objective: &str,
    target_files: Vec<String>,
) -> String {
    const BUDGET: usize = 16 * 1024;
    let target_views = crate::project::build_file_text_target_views(&view, &target_files, BUDGET);
    let provider =
        ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed safely"}"#]);
    let runner = ProviderRoleRunner::new(&provider);
    runner.run_role(
        RoleRequest {
            role: DeliberationRole::Producer,
            objective: objective.to_string(),
            target_files,
            target_views,
            producer_content: None,
            critic_content: None,
            feedback: vec![],
            node_kind: NodeKind::Work,
            tool_context: Some(RoleToolContext {
                artifact_view: Box::new(view),
            }),
        },
        &crate::telemetry::NoopTelemetry,
    );
    provider.requests.borrow()[0].prompt.clone()
}

fn target_state_section(prompt: &str) -> &str {
    let start = prompt
        .find("Target state view")
        .expect("prompt must include a target-state view");
    let end = prompt[start..]
        .find("\nProducer returns")
        .or_else(|| prompt[start..].find("\nCritic accepts"))
        .or_else(|| prompt[start..].find("\nReferee accepts"))
        .or_else(|| prompt[start..].find("\nAvailable file tools:"))
        .map(|offset| start + offset)
        .unwrap_or(prompt.len());
    &prompt[start..end]
}

#[test]
fn existing_target_file_content_appears_in_producer_prompt() {
    let (_temp, view) = make_view("target-state-existing");

    let prompt =
        producer_prompt_for_targets(view, "update the greeting", vec!["hello.txt".to_string()]);
    let section = target_state_section(&prompt);

    assert!(
        section.contains("- target: hello.txt"),
        "target state must name the structured target; got:\n{section}"
    );
    assert!(
        section.contains("exists: true"),
        "existing target must be marked exists:true; got:\n{section}"
    );
    assert!(
        section.contains("hello world"),
        "existing target text must appear in Producer prompt; got:\n{section}"
    );
}

#[test]
fn missing_target_is_explicitly_marked_absent() {
    let (_temp, view) = make_view("target-state-missing");

    let prompt = producer_prompt_for_targets(
        view,
        "create the missing target",
        vec!["missing.txt".to_string()],
    );
    let section = target_state_section(&prompt);

    assert!(
        section.contains("- target: missing.txt"),
        "target state must name the missing target; got:\n{section}"
    );
    assert!(
        section.contains("exists: false"),
        "missing target must be marked exists:false; got:\n{section}"
    );
    assert!(
        section.contains("representation: absent"),
        "missing target must use absent representation; got:\n{section}"
    );
}

#[test]
fn target_state_uses_structured_target_files_not_prompt_text() {
    let (_temp, view) = make_view_with_entries(
        "target-state-structured",
        &[
            ("structured.txt", b"structured content\n".as_slice()),
            ("mentioned-only.txt", b"prompt-only content\n".as_slice()),
        ],
    );

    let prompt = producer_prompt_for_targets(
        view,
        "Update mentioned-only.txt, but the structured target is authoritative.",
        vec!["structured.txt".to_string()],
    );
    let section = target_state_section(&prompt);

    assert!(
        section.contains("- target: structured.txt"),
        "target state must include structured target; got:\n{section}"
    );
    assert!(
        section.contains("structured content"),
        "target state must include structured target content; got:\n{section}"
    );
    assert!(
        !section.contains("mentioned-only.txt") && !section.contains("prompt-only content"),
        "target state must not be inferred from objective wording; got:\n{section}"
    );
}

#[test]
fn prompt_wording_changes_do_not_affect_target_state() {
    let (_temp, view) = make_view("target-state-wording");
    let target_files = vec!["hello.txt".to_string()];

    let first = producer_prompt_for_targets(
        view.clone(),
        "Please edit hello.txt with concise wording.",
        target_files.clone(),
    );
    let second = producer_prompt_for_targets(
        view,
        "Completely different phrasing that still uses the same target.",
        target_files,
    );

    assert_eq!(
        target_state_section(&first),
        target_state_section(&second),
        "target state must depend on structured target_files, not objective wording"
    );
}

#[test]
fn large_and_unreadable_targets_are_represented_safely() {
    let large = vec![b'x'; 16 * 1024 + 1];
    let binary = [0xff, 0xfe, 0xfd, b'\n'];
    let (_temp, view) = make_view_with_entries(
        "target-state-safe-errors",
        &[
            ("large.txt", large.as_slice()),
            ("binary.dat", binary.as_slice()),
        ],
    );

    let prompt = producer_prompt_for_targets(
        view,
        "inspect target state safely",
        vec!["large.txt".to_string(), "binary.dat".to_string()],
    );
    let section = target_state_section(&prompt);

    assert!(
        section.contains("- target: large.txt") && section.contains("too large"),
        "large target must be summarized without full content; got:\n{section}"
    );
    assert!(
        section.contains("- target: binary.dat")
            && section.contains("binary or non-UTF-8 file cannot be represented as text"),
        "unreadable/binary target must be represented as a safe error; got:\n{section}"
    );
}

fn dummy_view() -> ArtifactView {
    ArtifactView {
        repo_path: PathBuf::from("/nonexistent"),
        commit_sha: "deadbeef".to_string(),
    }
}

fn make_role_request(role: DeliberationRole, objective: &str) -> RoleRequest {
    RoleRequest {
        role,
        objective: objective.to_string(),
        target_files: vec![],
        target_views: vec![],
        producer_content: None,
        critic_content: None,
        feedback: vec![],
        node_kind: NodeKind::Work,
        tool_context: None,
    }
}

fn producer_request(objective: &str) -> RoleRequest {
    make_role_request(DeliberationRole::Producer, objective)
}

fn critic_request(objective: &str, producer_content: &str) -> RoleRequest {
    RoleRequest {
        producer_content: Some(producer_content.to_string()),
        ..make_role_request(DeliberationRole::Critic, objective)
    }
}

fn referee_request(objective: &str, producer_content: &str, critic_content: &str) -> RoleRequest {
    RoleRequest {
        producer_content: Some(producer_content.to_string()),
        critic_content: Some(critic_content.to_string()),
        ..make_role_request(DeliberationRole::Referee, objective)
    }
}

fn plan_request(objective: &str) -> RoleRequest {
    RoleRequest {
        node_kind: NodeKind::Plan,
        ..producer_request(objective)
    }
}

fn with_tool_context(mut request: RoleRequest, view: ArtifactView) -> RoleRequest {
    request.tool_context = Some(RoleToolContext {
        artifact_view: Box::new(view),
    });
    request
}

fn with_dummy_tool_context(request: RoleRequest) -> RoleRequest {
    with_tool_context(request, dummy_view())
}

fn with_target_files(mut request: RoleRequest, target_files: &[&str]) -> RoleRequest {
    request.target_files = target_files.iter().map(|path| path.to_string()).collect();
    request
}

// --- tool loop tests ---

#[test]
fn role_runner_executes_read_file_tool_then_accepts() {
    let (_temp, view) = make_view("read-file-tool");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"read the file"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(producer_request("read hello.txt"), view),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { ref content } if content == "read the file"),
        "expected Accepted after read_file tool loop, got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        2,
        "must call provider twice"
    );
    let second_prompt = &provider.requests.borrow()[1].prompt;
    assert!(
        second_prompt.contains("Framework tool observation:"),
        "second prompt must include tool observation"
    );
    assert!(
        second_prompt.contains("hello world"),
        "observation must include file content"
    );
}

#[test]
fn role_runner_records_write_file_tool_update() {
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"output.txt","content":"hello"}"#,
        r#"{"status":"accepted","content":"wrote the file"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(producer_request("write a file")),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "expected Accepted, got {:?}",
        output.result
    );
    let update = output
        .artifact_update
        .expect("write_file must produce an artifact_update");
    assert_eq!(update.changes.len(), 1);
    match &update.changes[0] {
        FileChange::Write { path, content } => {
            assert_eq!(path, "output.txt");
            assert_eq!(content, "hello");
        }
        other => panic!("expected Write change, got {other:?}"),
    }
}

#[test]
fn role_runner_rejects_tool_when_no_artifact_view() {
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"list_files"}"#,
        r#"{"status":"accepted","content":"used no tools"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        producer_request("do the thing"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { ref content } if content == "used no tools"),
        "tool request without view must produce error observation and allow final result; got {:?}",
        output.result
    );
    assert_eq!(provider.requests.borrow().len(), 2);
    let second_prompt = &provider.requests.borrow()[1].prompt;
    assert!(
        second_prompt.contains("no file tools available"),
        "second prompt must include error observation"
    );
}

#[test]
fn role_runner_stops_at_tool_loop_limit() {
    // Repeated identical list_files calls produce repeated identical observations,
    // so repeated-observation coercion fires after 2 calls and the 3rd call
    // (while coercion is active) immediately fails with a specific protocol error.
    let responses: Vec<&str> = vec![r#"{"tool":"list_files"}"#; 3];
    let provider = ScriptedProvider::from_strs(&responses);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(producer_request("loop forever")),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Failed { ref reason, .. } if reason.contains("repeated")),
        "must fail with repeated-observation error before the generic limit; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        3,
        "provider must be called exactly 3 times (2 duplicate observations + 1 post-coercion tool call)"
    );
}

// Helper: build a bare repo containing `n` distinct files named file0.txt .. file{n-1}.txt.
fn make_view_with_n_files(label: &str, n: usize) -> (TempDir, ArtifactView) {
    let temp = TempDir::new(label);
    let seed = temp.0.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "--quiet", "--initial-branch=main"]);
    git(&seed, &["config", "user.name", "Test"]);
    git(&seed, &["config", "user.email", "test@example.invalid"]);
    for i in 0..n {
        std::fs::write(seed.join(format!("file{i}.txt")), format!("content-{i}\n")).unwrap();
    }
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "--quiet", "-m", "init"]);
    let bare = temp.0.join("bare.git");
    Command::new("git")
        .args(["clone", "--quiet", "--bare"])
        .arg(&seed)
        .arg(&bare)
        .status()
        .expect("git clone --bare failed");
    let sha = git_rev(&bare);
    (
        temp,
        ArtifactView {
            repo_path: bare,
            commit_sha: sha,
        },
    )
}

#[test]
fn role_runner_generic_tool_loop_limit_applies_without_repetition() {
    // MAX_TOOL_STEPS distinct read_file requests each produce unique content;
    // no repeated observation fires. The (MAX_TOOL_STEPS+1)-th call hits the
    // generic loop limit.
    let (_temp, view) = make_view_with_n_files("generic-limit", MAX_TOOL_STEPS);
    let responses: Vec<String> = (0..=MAX_TOOL_STEPS)
        .map(|i| format!(r#"{{"tool":"read_file","path":"file{i}.txt"}}"#))
        .collect();
    let response_strs: Vec<&str> = responses.iter().map(|s| s.as_str()).collect();
    let provider = ScriptedProvider::from_strs(&response_strs);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(producer_request("loop with distinct files"), view),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Failed { ref reason, .. } if reason.contains("tool loop limit")),
        "must fail with generic tool loop limit when observations are distinct; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        MAX_TOOL_STEPS + 1,
        "provider must be called exactly MAX_TOOL_STEPS + 1 times"
    );
}

// ── repeated-observation coercion tests ──────────────────────────────────

#[test]
fn producer_completion_pressure_fires_after_write_file() {
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"output.txt","content":"hello"}"#,
        r#"{"status":"accepted","content":"wrote the file successfully"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(producer_request("write a file")),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "producer must finalize after write_file; got {:?}",
        output.result
    );
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2, "provider must be called twice");
    let second_prompt = &requests[1].prompt;
    assert!(
        second_prompt.contains("The requested change has already been recorded"),
        "second prompt must contain completion-pressure hint; got:\n{second_prompt}"
    );
    assert!(
        !second_prompt.contains("Available file tools:"),
        "second prompt must not advertise tools after completion pressure; got:\n{second_prompt}"
    );
}

#[test]
fn critic_decision_pressure_fires_after_max_read_steps() {
    let (_temp, view) = make_view("critic-decision-pressure");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"the file looks good"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(critic_request("review hello.txt", "draft"), view),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "critic must finalize after read_file steps; got {:?}",
        output.result
    );
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 3, "provider must be called three times");
    let third_prompt = &requests[2].prompt;
    assert!(
        third_prompt.contains("sufficient evidence"),
        "third prompt must contain decision-pressure text; got:\n{third_prompt}"
    );
}

#[test]
fn producer_repeated_identical_read_file_triggers_coercion() {
    let (_temp, view) = make_view("repeated-read-coercion");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"read the same file twice"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(producer_request("inspect hello.txt"), view),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "producer must accept after coercion forces final response; got {:?}",
        output.result
    );
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 3, "provider must be called three times");
    let third_prompt = &requests[2].prompt;
    assert!(
        third_prompt.contains("You have already inspected this information"),
        "third prompt must contain repeated-observation coercion text; got:\n{third_prompt}"
    );
    assert!(
        !third_prompt.contains("Available file tools:"),
        "third prompt must not advertise tools after coercion; got:\n{third_prompt}"
    );
}

#[test]
fn repeated_identical_tool_calls_fail_before_generic_limit() {
    // The producer keeps calling list_files with identical results. The second
    // identical observation triggers coercion. A third tool call (after coercion)
    // fails immediately with a specific protocol error — not the generic limit.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"list_files"}"#,
        r#"{"tool":"list_files"}"#,
        r#"{"tool":"list_files"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(producer_request("loop on list_files")),
        &crate::telemetry::NoopTelemetry,
    );

    let reason = match &output.result {
        RoleResult::Failed { reason, .. } => reason.clone(),
        other => panic!("expected Failed, got {other:?}"),
    };
    assert!(
        reason.contains("repeated"),
        "failure reason must mention 'repeated'; got: {reason}"
    );
    assert!(
        !reason.contains("tool loop limit"),
        "failure must not use generic tool loop limit message; got: {reason}"
    );
    assert_eq!(
        provider.requests.borrow().len(),
        3,
        "only 3 provider calls: duplicate observation fires at call 2, coercion violation at call 3"
    );
}

#[test]
fn existing_valid_tool_use_still_works() {
    // list_files then write_file then accepted — no repeated observations,
    // no coercion, normal completion pressure after the write.
    let (_temp, view) = make_view("valid-tool-use");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"list_files"}"#,
        r#"{"tool":"write_file","path":"result.txt","content":"done"}"#,
        r#"{"status":"accepted","content":"listed files and wrote result"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(producer_request("list then write"), view),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { ref content } if content == "listed files and wrote result"),
        "valid tool sequence must succeed; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        3,
        "all 3 provider calls must be made"
    );
}

// ── placeholder tool echo tests ─────────────────────────────────────────

#[test]
fn placeholder_tool_echo_produces_informative_error_in_retry_prompt() {
    // The model echoes the write_file example verbatim with $TARGET_FILE /
    // $FILE_CONTENT. The retry prompt must mention "placeholder" so the model
    // understands WHY its response was rejected, not just that it was invalid.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"$TARGET_FILE","content":"$FILE_CONTENT"}"#,
        r#"{"status":"accepted","content":"wrote the actual file"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(producer_request("write a file")),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "must recover on retry; got {:?}",
        output.result
    );
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2, "provider must be called twice");
    let retry_prompt = &requests[1].prompt;
    assert!(
        retry_prompt.contains("placeholder"),
        "retry prompt must mention 'placeholder' so the model knows why it was rejected; got:\n{retry_prompt}"
    );
}

#[test]
fn protocol_failure_after_write_reason_is_prefixed() {
    // Producer calls write_file successfully (completion pressure active), then
    // exhausts all protocol retries returning bad JSON. The terminal failure
    // reason must start with "protocol failure after write:" so the classifier
    // can treat it as Retry rather than Terminal.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"result.txt","content":"done"}"#,
        "not json at all",
        "also not json",
        "still not json",
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(producer_request("write and confirm")),
        &crate::telemetry::NoopTelemetry,
    );

    let reason = match &output.result {
        RoleResult::Failed { reason, .. } => reason.clone(),
        other => panic!("expected Failed; got {other:?}"),
    };
    assert!(
        reason.starts_with("protocol failure after write:"),
        "terminal reason must start with 'protocol failure after write:'; got: {reason}"
    );
    assert_eq!(
        provider.requests.borrow().len(),
        4,
        "write_file + 3 failed final-response attempts"
    );
}

#[test]
fn role_runner_uses_provider_response_content() {
    let provider =
        ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"the result"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        producer_request("produce something"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { ref content } if content == "the result"),
        "role runner must use response.content; got {:?}",
        output.result
    );
}

// ── policy: critic write request produces error observation ──────────────

#[test]
fn critic_write_request_produces_error_observation() {
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"output.txt","content":"critic draft"}"#,
        r#"{"status":"rejected","reason":"cannot write"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(critic_request("review the work", "draft")),
        &crate::telemetry::NoopTelemetry,
    );

    // The role must continue (not crash) and the second prompt must include
    // the permission error observation.
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2, "provider must be called twice");
    let second_prompt = &requests[1].prompt;
    assert!(
        second_prompt.contains("not permitted"),
        "second prompt must include write-permission error; got:\n{second_prompt}"
    );
    // No artifact update must be recorded.
    assert!(
        output.artifact_update.is_none(),
        "critic write must not produce an artifact update"
    );
}

// ── observation bounding ─────────────────────────────────────────────────

#[test]
fn format_tool_observation_is_bounded() {
    let large_content = "x".repeat(500);
    let response = FileToolResponse::FileContents {
        path: "big.txt".to_owned(),
        content: large_content,
    };
    let max_obs = 100;
    let observation = format_tool_observation(&response, max_obs);
    assert!(
        observation.len() <= max_obs + "\n[observation truncated]".len(),
        "observation must be bounded; len={}, max={}",
        observation.len(),
        max_obs
    );
    assert!(
        observation.contains("[observation truncated]"),
        "truncation marker must be present; got: {observation:?}"
    );
}

#[test]
fn role_runner_uses_configured_max_tokens() {
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
    let runner = ProviderRoleRunner::new_with_max_tokens(&provider, 256);

    runner.run_role(producer_request("test"), &crate::telemetry::NoopTelemetry);

    let requests = provider.requests.borrow();
    assert_eq!(
        requests[0].max_tokens, 256,
        "configured max_tokens must be forwarded to the provider"
    );
}

#[test]
fn role_prompt_includes_tool_request_as_valid_response_when_tools_available() {
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(producer_request("test with tools")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("tool request"),
        "prompt must describe tool request as a valid response when tools are available"
    );
    assert!(
        prompt.contains("list_files"),
        "prompt must include example tool requests"
    );
}

#[test]
fn role_prompt_has_single_protocol_wrapper() {
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(producer_request("test"), &crate::telemetry::NoopTelemetry);

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    // "Accepted schema:" is the old InstructedProvider outer wrapper text.
    // render_role_prompt uses "Accepted:" (without "schema").
    assert!(
        !prompt.contains("Accepted schema:"),
        "prompt must not contain InstructedProvider outer wrapper text"
    );
    assert!(
        prompt.contains("\"status\""),
        "prompt must still contain the role protocol instructions"
    );
}

#[test]
fn tool_observation_is_bounded_in_role_prompt() {
    // Create an artifact with a file larger than max_observation_bytes (16 KiB).
    let (_temp, view) = {
        let temp = TempDir::new("large-obs");
        let seed = temp.0.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git(&seed, &["init", "--quiet", "--initial-branch=main"]);
        git(&seed, &["config", "user.name", "Test"]);
        git(&seed, &["config", "user.email", "test@example.invalid"]);
        // 20 KiB of content — exceeds the 16 KiB max_observation_bytes default.
        let large = "y".repeat(20 * 1024);
        std::fs::write(seed.join("large.txt"), &large).unwrap();
        git(&seed, &["add", "large.txt"]);
        git(&seed, &["commit", "--quiet", "-m", "add large file"]);
        let bare = temp.0.join("bare.git");
        Command::new("git")
            .args(["clone", "--quiet", "--bare"])
            .arg(&seed)
            .arg(&bare)
            .status()
            .expect("git clone failed");
        let sha = git_rev(&bare);
        (
            temp,
            ArtifactView {
                repo_path: bare,
                commit_sha: sha,
            },
        )
    };

    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"read_file","path":"large.txt"}"#,
        r#"{"status":"accepted","content":"completed"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_tool_context(producer_request("read the large file"), view),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2, "provider must be called twice");
    let second_prompt = &requests[1].prompt;
    assert!(
        second_prompt.contains("[observation truncated]"),
        "large observation must be truncated in the prompt"
    );
    // The tool result section must not contain the full 20 KiB of content.
    let obs_start = second_prompt
        .find("Framework tool observation:")
        .expect("prompt must contain Framework tool observation:");
    let obs_len = second_prompt[obs_start..].len();
    assert!(
        obs_len < 20 * 1024,
        "observation section must be much smaller than 20 KiB; got {obs_len} bytes"
    );
}

#[test]
fn scripted_provider_supports_request_response_objects() {
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        producer_request("anything"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 1);
    assert!(
        !requests[0].prompt.is_empty(),
        "request must carry a prompt"
    );
    assert_eq!(
        requests[0].max_tokens, MAX_RESPONSE_TOKENS,
        "request must carry the runner's max_tokens constant"
    );
}

#[test]
fn role_runner_requests_json_output() {
    use crate::providers::StructuredOutput;

    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        producer_request("write something"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    assert_eq!(
        requests[0].output_schema,
        Some(StructuredOutput::Json),
        "RoleRunner must request Json structured output"
    );
}

// ── prompt/policy consistency ────────────────────────────────────────────

#[test]
fn producer_prompt_lists_write_tools() {
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(producer_request("produce something")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("write_file"),
        "producer prompt must include write_file; got:\n{prompt}"
    );
    assert!(
        prompt.contains("replace_text"),
        "producer prompt must include replace_text; got:\n{prompt}"
    );
    assert!(
        prompt.contains("delete_file"),
        "producer prompt must include delete_file; got:\n{prompt}"
    );
}

#[test]
fn critic_prompt_omits_write_tools() {
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"rejected","reason":"needs work"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(critic_request("review the draft", "draft")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        !prompt.contains("write_file"),
        "critic prompt must not include write_file; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("replace_text"),
        "critic prompt must not include replace_text; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("delete_file"),
        "critic prompt must not include delete_file; got:\n{prompt}"
    );
    assert!(
        prompt.contains("list_files"),
        "critic prompt must include list_files; got:\n{prompt}"
    );
    assert!(
        prompt.contains("read_file"),
        "critic prompt must include read_file; got:\n{prompt}"
    );
}

#[test]
fn referee_prompt_omits_write_tools() {
    // Use a rejection response so the read-file enforcement does not fire
    // (enforcement only applies when the reviewer accepts).
    let provider = ScriptedProvider::from_strs(&[
        r#"{"status":"rejected","reason":"content does not meet requirements"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(referee_request(
            "approve the result",
            "content",
            "looks good",
        )),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        !prompt.contains("write_file"),
        "referee prompt must not include write_file; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("replace_text"),
        "referee prompt must not include replace_text; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("delete_file"),
        "referee prompt must not include delete_file; got:\n{prompt}"
    );
    assert!(
        prompt.contains("list_files"),
        "referee prompt must include list_files; got:\n{prompt}"
    );
    assert!(
        prompt.contains("read_file"),
        "referee prompt must include read_file; got:\n{prompt}"
    );
}

#[test]
fn read_only_role_write_request_still_rejected() {
    // Even when the prompt omits write tools, a malicious/confused model
    // that sends a write request must still be rejected by the executor.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"bad.txt","content":"sneaky"}"#,
        r#"{"status":"rejected","reason":"cannot write"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(critic_request("review", "draft")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2, "provider must be called twice");
    let second_prompt = &requests[1].prompt;
    assert!(
        second_prompt.contains("not permitted"),
        "executor must reject write even when prompt omits write tools; got:\n{second_prompt}"
    );
    assert!(
        output.artifact_update.is_none(),
        "rejected write must not produce an artifact update"
    );
}

// ── regression: echoed placeholder tool requests must not execute ───────
//
// A confused model sometimes echoes the tool-section examples verbatim,
// returning {"tool":"replace_text","path":"output.txt","old":"...","new":"..."}
// or {"tool":"write_file","path":"output.txt","content":"..."}.  These must
// be treated as parse failures and trigger a protocol retry, NOT executed as
// real tool calls.  This was the root cause of the "missing field `status`"
// failure observed in the 2026-06-24 run.

#[test]
fn echoed_tool_placeholder_triggers_parse_failure_not_tool_execution() {
    use crate::telemetry::{TelemetryEvent, VecTelemetry};

    // A confused model sometimes echoes the tool-section examples verbatim.
    // These must be treated as parse failures and trigger a retry, NOT executed.
    for (case, placeholder_response, second_response, expected_content) in [
        (
            "replace_text",
            r#"{"tool":"replace_text","path":"output.txt","old":"...","new":"..."}"#,
            r#"{"status":"accepted","content":"haiku written"}"#,
            "haiku written",
        ),
        (
            "write_file",
            r#"{"tool":"write_file","path":"output.txt","content":"..."}"#,
            r#"{"status":"accepted","content":"completed"}"#,
            "completed",
        ),
    ] {
        let provider = ScriptedProvider::from_strs(&[placeholder_response, second_response]);
        let runner = ProviderRoleRunner::new(&provider);
        let telemetry = VecTelemetry::new();

        let output = runner.run_role(
            make_role_request(DeliberationRole::Producer, "write a file"),
            &telemetry,
        );

        assert!(
            matches!(output.result, RoleResult::Accepted { ref content } if content == expected_content),
            "[{case}] placeholder tool must not execute; got {:?}",
            output.result
        );
        let records = telemetry.records();
        assert!(
            records
                .iter()
                .all(|r| !matches!(r.event, TelemetryEvent::ToolRequested { .. })),
            "[{case}] placeholder tool must not emit ToolRequested"
        );
        assert!(
            records
                .iter()
                .any(|r| matches!(&r.event, TelemetryEvent::ParseFailed { .. })),
            "[{case}] placeholder tool must emit ParseFailed"
        );
    }
}

// ── prompt hardening: no "..." placeholders in any rendered prompt ───────

#[test]
fn no_runtime_prompt_contains_dot_placeholder_json_values() {
    // Render every prompt variant and assert none contains the "..." sentinel
    // as a JSON string value.  "..." in a JSON value is a known trigger for
    // model placeholder-copying (see 2026-06-24 incident).
    let no_dot = |label: &str, prompt: &str| {
        assert!(
            !prompt.contains("\"...\""),
            "{label} must not contain '...' as a JSON value; got:\n{prompt}"
        );
    };

    // Role prompts for all three roles, with and without prior content.
    let default = RolePolicy::default();
    for (role, system, pc, cc) in [
        (
            DeliberationRole::Producer,
            default.worker_producer_system.as_str(),
            None,
            None,
        ),
        (
            DeliberationRole::Critic,
            default.worker_critic_system.as_str(),
            Some("draft"),
            None,
        ),
        (
            DeliberationRole::Referee,
            default.worker_referee_system.as_str(),
            Some("draft"),
            Some("looks good"),
        ),
    ] {
        let prompt =
            render_role_prompt(system, &role, "write a haiku about Rust", pc, cc, &[], &[]);
        no_dot(&format!("{role:?} role prompt"), &prompt);
    }

    // Tool section — write-enabled and read-only.
    let rw = render_tool_section(&FileToolPolicy {
        allow_writes: true,
        ..FileToolPolicy::default()
    });
    let ro = render_tool_section(&FileToolPolicy {
        allow_writes: false,
        ..FileToolPolicy::default()
    });
    no_dot("write-enabled tool section", &rw);
    no_dot("read-only tool section", &ro);

    // Retry prompt (wraps the base role prompt).
    let base = render_role_prompt(
        &default.worker_producer_system,
        &DeliberationRole::Producer,
        "write a haiku",
        None,
        None,
        &[],
        &[],
    );
    let retry = render_retry_prompt(&base, "no JSON object found in role response");
    no_dot("retry prompt", &retry);
}

#[test]
fn producer_prompt_uses_concrete_or_named_tool_examples() {
    let rw = render_tool_section(&FileToolPolicy {
        allow_writes: true,
        ..FileToolPolicy::default()
    });
    // write_file must show a concrete content value, not "...".
    assert!(
        rw.contains("write_file"),
        "write-enabled section must include write_file"
    );
    let write_file_pos = rw.find("write_file").unwrap();
    let after_write = &rw[write_file_pos..];
    assert!(
        !after_write.starts_with(&format!(
            "write_file\",\"path\":\"output.txt\",\"content\":\"...\""
        )) && after_write.contains("content"),
        "write_file example must not use '...' for content; got:\n{after_write}"
    );
    // replace_text must use named <PLACEHOLDER> tokens, not "...".
    assert!(
        rw.contains("replace_text"),
        "write-enabled section must include replace_text"
    );
    assert!(
        !rw.contains("\"old\":\"...\""),
        "replace_text old must not be '...'; got:\n{rw}"
    );
    assert!(
        !rw.contains("\"new\":\"...\""),
        "replace_text new must not be '...'; got:\n{rw}"
    );
}

#[test]
fn role_response_examples_do_not_use_dot_placeholders() {
    let default = RolePolicy::default();
    for (role, system, pc, cc) in [
        (
            DeliberationRole::Producer,
            default.worker_producer_system.as_str(),
            None,
            None,
        ),
        (
            DeliberationRole::Critic,
            default.worker_critic_system.as_str(),
            Some("draft"),
            None,
        ),
        (
            DeliberationRole::Referee,
            default.worker_referee_system.as_str(),
            Some("draft"),
            Some("looks good"),
        ),
    ] {
        let prompt = render_role_prompt(system, &role, "test objective", pc, cc, &[], &[]);
        assert!(
            !prompt.contains("\"content\":\"...\""),
            "{role:?} prompt must not use '...' for accepted content; got:\n{prompt}"
        );
        assert!(
            !prompt.contains("\"reason\":\"...\""),
            "{role:?} prompt must not use '...' for rejected reason; got:\n{prompt}"
        );
    }
    // Retry prompt schema examples also must not use "...".
    let base = render_role_prompt(
        &default.worker_producer_system,
        &DeliberationRole::Producer,
        "test",
        None,
        None,
        &[],
        &[],
    );
    let retry = render_retry_prompt(&base, "parse error");
    assert!(
        !retry.contains("\"content\":\"...\""),
        "retry prompt must not use '...' for accepted content; got:\n{retry}"
    );
    assert!(
        !retry.contains("\"reason\":\"...\""),
        "retry prompt must not use '...' for rejected reason; got:\n{retry}"
    );
}

#[test]
fn prompt_mentions_not_to_copy_example_values() {
    // Every prompt surface that includes JSON examples must explicitly instruct
    // the model not to copy them verbatim.
    let default = RolePolicy::default();
    let base = render_role_prompt(
        &default.worker_producer_system,
        &DeliberationRole::Producer,
        "write a haiku",
        None,
        None,
        &[],
        &[],
    );
    assert!(
        base.contains("Do not copy example values"),
        "role prompt must instruct model not to copy examples; got:\n{base}"
    );

    let rw = render_tool_section(&FileToolPolicy {
        allow_writes: true,
        ..FileToolPolicy::default()
    });
    assert!(
        rw.contains("Do not copy example values"),
        "write-enabled tool section must instruct model not to copy examples; got:\n{rw}"
    );

    let retry = render_retry_prompt(&base, "parse error");
    assert!(
        retry.contains("Do not copy example values"),
        "retry prompt must instruct model not to copy examples; got:\n{retry}"
    );
}

#[test]
fn tool_prompt_matches_policy() {
    let rw_policy = FileToolPolicy {
        allow_writes: true,
        ..FileToolPolicy::default()
    };
    let ro_policy = FileToolPolicy {
        allow_writes: false,
        ..FileToolPolicy::default()
    };

    let rw_section = super::render_tool_section(&rw_policy);
    let ro_section = super::render_tool_section(&ro_policy);

    assert!(
        rw_section.contains("write_file"),
        "allow_writes=true must render write_file"
    );
    assert!(
        rw_section.contains("replace_text"),
        "allow_writes=true must render replace_text"
    );
    assert!(
        rw_section.contains("delete_file"),
        "allow_writes=true must render delete_file"
    );
    assert!(
        !ro_section.contains("write_file"),
        "allow_writes=false must not render write_file"
    );
    assert!(
        !ro_section.contains("replace_text"),
        "allow_writes=false must not render replace_text"
    );
    assert!(
        !ro_section.contains("delete_file"),
        "allow_writes=false must not render delete_file"
    );
    assert!(
        ro_section.contains("list_files"),
        "allow_writes=false must still render list_files"
    );
    assert!(
        ro_section.contains("read_file"),
        "allow_writes=false must still render read_file"
    );
}

#[test]
fn tool_prompt_for_target_main_py_shows_exact_read_file_path() {
    let policy = FileToolPolicy {
        allowed_paths: Some(vec!["main.py".to_string()]),
        ..FileToolPolicy::default()
    };

    let section = super::render_tool_section(&policy);

    assert!(
        section.contains(r#"{"tool":"read_file","path":"main.py"}"#),
        "target-aware tool section must show exact read_file path; got:\n{section}"
    );
    assert!(
        !section.contains(r#"{"tool":"read_file","path":"path/to/file.txt"}"#),
        "target-aware tool section must not show generic read_file placeholder; got:\n{section}"
    );
}

#[test]
fn work_role_prompt_uses_structured_tool_targets() {
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(with_target_files(
            producer_request("Update the program."),
            &["main.py"],
        )),
        &crate::telemetry::NoopTelemetry,
    );

    let prompt = &provider.requests.borrow()[0].prompt;
    assert!(
        prompt.contains(r#"{"tool":"read_file","path":"main.py"}"#),
        "Work prompt must render declared target in read_file example; got:\n{prompt}"
    );
    assert!(
        prompt.contains(r#"{"tool":"write_file","path":"main.py""#),
        "Work prompt must render declared target in write_file example; got:\n{prompt}"
    );
}

#[test]
fn prompt_wording_does_not_control_allowed_paths() {
    let (_temp, view) = make_view("prompt-wording-targets");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"prompt.txt","content":"wrong\n"}"#,
        r#"{"tool":"write_file","path":"main.py","content":"right\n"}"#,
        r#"{"status":"accepted","content":"completed"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(
            with_target_files(
                producer_request("Update the program.\n\nTarget files: prompt.txt"),
                &["main.py"],
            ),
            view,
        ),
        &crate::telemetry::NoopTelemetry,
    );

    let prompt = &provider.requests.borrow()[0].prompt;
    assert!(
        prompt.contains("Target files: main.py"),
        "prompt should render structured targets; got:\n{prompt}"
    );
    assert!(
        prompt.contains("Target files: prompt.txt"),
        "objective wording is still visible as prompt text; got:\n{prompt}"
    );

    let update = output
        .artifact_update
        .expect("structured target write should be recorded");
    assert_eq!(
        update.changes,
        vec![FileChange::Write {
            path: "main.py".to_string(),
            content: "right\n".to_string(),
        }]
    );
}

#[test]
fn production_code_does_not_parse_target_files_prompt_text() {
    let sources = [
        include_str!("runner.rs"),
        include_str!("../node_runner/planner.rs"),
        include_str!("../node_runner/deliberating.rs"),
        include_str!("../machines/deliberation/handler.rs"),
    ]
    .join("\n");

    assert!(!sources.contains(concat!("declared_target", "_files")));
    assert!(!sources.contains(concat!("starts_with(\"", "Target files:")));
    assert!(!sources.contains(concat!("split_once", "(':')")));
}

#[test]
fn work_reviewer_prompt_guides_read_file_to_declared_target() {
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"rejected","reason":"needs work"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(with_target_files(
            critic_request("Review the update.", "updated main.py"),
            &["main.py"],
        )),
        &crate::telemetry::NoopTelemetry,
    );

    let prompt = &provider.requests.borrow()[0].prompt;
    assert!(
        prompt.contains(r#"{"tool":"read_file","path":"main.py"}"#),
        "Work reviewer prompt must guide read_file to declared target; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("write_file"),
        "Work reviewer prompt must remain read-only; got:\n{prompt}"
    );
}

// ── RolePolicy tests ─────────────────────────────────────────────────────

#[test]
fn default_role_policy_matches_current_prompt_behavior() {
    let policy = RolePolicy::default();
    let prompt = render_role_prompt(
        &policy.worker_producer_system,
        &DeliberationRole::Producer,
        "write a haiku",
        None,
        None,
        &[],
        &[],
    );
    assert!(
        prompt.contains("\"status\""),
        "default policy must include JSON status field; got:\n{prompt}"
    );
    assert!(
        prompt.contains("Do not copy example values"),
        "default policy must include copy-guard instruction; got:\n{prompt}"
    );
    assert!(
        prompt.contains("Producer returns accepted content"),
        "default policy must describe Producer role; got:\n{prompt}"
    );
    assert!(
        prompt.contains("Critic accepts"),
        "default policy must describe Critic role; got:\n{prompt}"
    );
    assert!(
        prompt.contains("Referee accepts"),
        "default policy must describe Referee role; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("\"...\""),
        "default policy must not contain dot-placeholder JSON values; got:\n{prompt}"
    );
}

#[test]
fn planner_prompt_uses_planner_policy() {
    let policy = RolePolicy {
        planner_producer_system: "PLANNER_MARKER_XYZ".to_string(),
        ..RolePolicy::default()
    };
    let prompt = render_role_prompt(
        &policy.planner_producer_system,
        &DeliberationRole::Producer,
        "plan the work",
        None,
        None,
        &[],
        &[],
    );
    assert!(
        prompt.contains("PLANNER_MARKER_XYZ"),
        "planner prompt must include planner_producer_system text; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("WORKER_MARKER"),
        "planner prompt must not include worker_producer_system text"
    );
}

#[test]
fn worker_prompt_uses_worker_policy() {
    let policy = RolePolicy {
        worker_producer_system: "WORKER_MARKER_XYZ".to_string(),
        ..RolePolicy::default()
    };
    let prompt = render_role_prompt(
        &policy.worker_producer_system,
        &DeliberationRole::Producer,
        "do the work",
        None,
        None,
        &[],
        &[],
    );
    assert!(
        prompt.contains("WORKER_MARKER_XYZ"),
        "worker prompt must include worker_producer_system text; got:\n{prompt}"
    );
}

#[test]
fn critic_prompt_uses_critic_policy() {
    let policy = RolePolicy {
        worker_critic_system: "CRITIC_MARKER_XYZ".to_string(),
        ..RolePolicy::default()
    };
    let prompt = render_role_prompt(
        &policy.worker_critic_system,
        &DeliberationRole::Critic,
        "review the draft",
        Some("producer draft"),
        None,
        &[],
        &[],
    );
    assert!(
        prompt.contains("CRITIC_MARKER_XYZ"),
        "critic prompt must include worker_critic_system text; got:\n{prompt}"
    );
}

#[test]
fn referee_prompt_uses_referee_policy() {
    let policy = RolePolicy {
        worker_referee_system: "REFEREE_MARKER_XYZ".to_string(),
        ..RolePolicy::default()
    };
    let prompt = render_role_prompt(
        &policy.worker_referee_system,
        &DeliberationRole::Referee,
        "approve the result",
        Some("producer draft"),
        Some("critic review"),
        &[],
        &[],
    );
    assert!(
        prompt.contains("REFEREE_MARKER_XYZ"),
        "referee prompt must include worker_referee_system text; got:\n{prompt}"
    );
}

#[test]
fn default_policy_keeps_json_protocol_instructions() {
    let policy = RolePolicy::default();
    // Worker, Critic, Referee use the status/content wrapper schema.
    for (label, system) in [
        ("worker", policy.worker_producer_system.as_str()),
        ("critic", policy.worker_critic_system.as_str()),
        ("referee", policy.worker_referee_system.as_str()),
    ] {
        let prompt = render_role_prompt(
            system,
            &DeliberationRole::Producer,
            "test",
            None,
            None,
            &[],
            &[],
        );
        assert!(
            prompt.contains("Return exactly one JSON object"),
            "{label} default policy must include JSON-only instruction; got:\n{prompt}"
        );
        assert!(
            prompt.contains("$RESPONSE_SUMMARY"),
            "{label} default policy must include accepted schema placeholder; got:\n{prompt}"
        );
        assert!(
            prompt.contains("$REASON_FOR_REJECTION"),
            "{label} default policy must include rejected schema placeholder; got:\n{prompt}"
        );
    }
    // Planner uses direct PlannerOutput schema — no status/content wrapper.
    let planner_prompt = render_role_prompt(
        &policy.planner_producer_system,
        &DeliberationRole::Producer,
        "test",
        None,
        None,
        &[],
        &[],
    );
    assert!(
        planner_prompt.contains("Return exactly one JSON object"),
        "planner default policy must include JSON-only instruction; got:\n{planner_prompt}"
    );
    assert!(
        planner_prompt.contains("\"tasks\""),
        "planner default policy must include direct tasks schema; got:\n{planner_prompt}"
    );
    assert!(
        !planner_prompt.contains("$RESPONSE_SUMMARY"),
        "planner default policy must not include status/content placeholder; got:\n{planner_prompt}"
    );
}

#[test]
fn role_policy_does_not_change_tool_visibility() {
    // Tool visibility is controlled by FileToolPolicy (file_tool_policy_for_role),
    // not by RolePolicy. Verify that changing system text has no effect.
    let policy = RolePolicy {
        worker_producer_system: "CUSTOM_WORKER".to_string(),
        worker_critic_system: "CUSTOM_CRITIC".to_string(),
        ..RolePolicy::default()
    };
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

    runner.run_role(
        with_dummy_tool_context(producer_request("produce something")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("write_file"),
        "producer must still see write tools regardless of custom policy; got:\n{prompt}"
    );
    assert!(
        prompt.contains("CUSTOM_WORKER"),
        "custom worker_producer_system must appear in producer prompt; got:\n{prompt}"
    );
}

// ── NodeKind policy routing ───────────────────────────────────────────────

#[test]
fn planner_node_uses_planner_policy() {
    let policy = RolePolicy {
        planner_producer_system: "PLANNER_MARKER".to_string(),
        ..RolePolicy::default()
    };
    let tasks_json = r#"{"tasks":[{"id":"t1","objective":"do the work","operation":"modify","targets":["work.txt"],"depends_on":[]}]}"#;
    let provider = ScriptedProvider::from_strs(&[tasks_json]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

    runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("PLANNER_MARKER"),
        "plan node must use planner_producer_system; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("WORKER_MARKER"),
        "plan node must not use worker_producer_system"
    );
}

#[test]
fn work_node_uses_worker_policy() {
    let policy = RolePolicy {
        worker_producer_system: "WORKER_MARKER".to_string(),
        ..RolePolicy::default()
    };
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"work done"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

    runner.run_role(
        producer_request("do the work"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("WORKER_MARKER"),
        "work node must use worker_producer_system; got:\n{prompt}"
    );
}

#[test]
fn plan_critic_uses_planner_critic_policy() {
    let policy = RolePolicy {
        planner_critic_system: "PLANNER_CRITIC_MARKER".to_string(),
        worker_critic_system: "WORKER_CRITIC_MARKER".to_string(),
        ..RolePolicy::default()
    };
    let provider =
        ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"plan review done"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

    runner.run_role(
        RoleRequest {
            node_kind: NodeKind::Plan,
            ..critic_request("review the plan", "plan graph")
        },
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("PLANNER_CRITIC_MARKER"),
        "plan critic must use planner_critic_system; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("WORKER_CRITIC_MARKER"),
        "plan critic must not use worker_critic_system; got:\n{prompt}"
    );
}

#[test]
fn work_critic_uses_worker_critic_policy() {
    let policy = RolePolicy {
        planner_critic_system: "PLANNER_CRITIC_MARKER".to_string(),
        worker_critic_system: "WORKER_CRITIC_MARKER".to_string(),
        ..RolePolicy::default()
    };
    let provider =
        ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"review done"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

    runner.run_role(
        critic_request("review the draft", "draft"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("WORKER_CRITIC_MARKER"),
        "work critic must use worker_critic_system; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("PLANNER_CRITIC_MARKER"),
        "work critic must not use planner_critic_system; got:\n{prompt}"
    );
}

#[test]
fn plan_referee_uses_planner_referee_policy() {
    let policy = RolePolicy {
        planner_referee_system: "PLANNER_REFEREE_MARKER".to_string(),
        worker_referee_system: "WORKER_REFEREE_MARKER".to_string(),
        ..RolePolicy::default()
    };
    let provider =
        ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"plan approved"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

    runner.run_role(
        RoleRequest {
            node_kind: NodeKind::Plan,
            ..referee_request("approve the plan", "plan graph", "plan review")
        },
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("PLANNER_REFEREE_MARKER"),
        "plan referee must use planner_referee_system; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("WORKER_REFEREE_MARKER"),
        "plan referee must not use worker_referee_system; got:\n{prompt}"
    );
}

#[test]
fn work_referee_uses_worker_referee_policy() {
    let policy = RolePolicy {
        planner_referee_system: "PLANNER_REFEREE_MARKER".to_string(),
        worker_referee_system: "WORKER_REFEREE_MARKER".to_string(),
        ..RolePolicy::default()
    };
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"approved"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

    runner.run_role(
        referee_request("approve the result", "content", "review"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("WORKER_REFEREE_MARKER"),
        "work referee must use worker_referee_system; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("PLANNER_REFEREE_MARKER"),
        "work referee must not use planner_referee_system; got:\n{prompt}"
    );
}

#[test]
fn default_policy_preserves_existing_behavior() {
    let policy = RolePolicy::default();
    let tasks_json = r#"{"tasks":[{"id":"t1","objective":"do the work","operation":"modify","targets":["work.txt"],"depends_on":[]}]}"#;
    let provider = ScriptedProvider::from_strs(&[
        tasks_json,
        r#"{"status":"accepted","content":"work done"}"#,
    ]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

    runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );
    runner.run_role(
        producer_request("do the work"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    for (label, req) in [("plan", &requests[0]), ("work", &requests[1])] {
        assert!(
            req.prompt.contains("Return exactly one JSON object"),
            "{label} producer prompt must contain JSON protocol instructions; got:\n{}",
            req.prompt
        );
    }
}

// ── Step 1: planner tool exclusion (runner-level) ────────────────────────

#[test]
fn planner_prompt_omits_tool_section() {
    // When node_kind is Plan and tool_context is None, no tool section appears.
    let tasks_json = r#"{"tasks":[{"id":"t1","objective":"do the work","operation":"modify","targets":["work.txt"],"depends_on":[]}]}"#;
    let provider = ScriptedProvider::from_strs(&[tasks_json]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        !prompt.contains("list_files"),
        "planner prompt must not include tool section; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("write_file"),
        "planner prompt must not include write tools; got:\n{prompt}"
    );
}

#[test]
fn planner_tool_request_produces_error_observation() {
    // Even if a plan-node model emits a tool request, it gets "no file tools available"
    // rather than actual execution, because tool_context is None for plan nodes.
    let tasks_json = r#"{"tasks":[{"id":"t1","objective":"do the thing","operation":"modify","targets":["thing.txt"],"depends_on":[]}]}"#;
    let provider =
        ScriptedProvider::from_strs(&[r#"{"tool":"read_file","path":"hello.txt"}"#, tasks_json]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "plan node must accept valid PlannerOutput after tool error; got {:?}",
        output.result
    );
    let second_prompt = &provider.requests.borrow()[1].prompt;
    assert!(
        second_prompt.contains("no file tools available"),
        "plan tool request must produce error observation; got:\n{second_prompt}"
    );
}

#[test]
fn worker_prompt_still_has_write_tools() {
    // Work nodes with tool_context keep write tools (existing behaviour preserved).
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(producer_request("implement the feature")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("write_file"),
        "worker prompt must still include write_file; got:\n{prompt}"
    );
    assert!(
        prompt.contains("replace_text"),
        "worker prompt must still include replace_text; got:\n{prompt}"
    );
}

// ── Step 2: planner content validation ───────────────────────────────────

#[test]
fn planner_accepts_valid_planner_output() {
    let tasks_json = r#"{"tasks":[{"id":"t1","objective":"do the thing","operation":"modify","targets":["thing.txt"],"depends_on":[]}]}"#;
    let provider = ScriptedProvider::from_strs(&[tasks_json]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "valid PlannerOutput must be accepted without retry; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        1,
        "no retry needed for valid PlannerOutput"
    );
}

#[test]
fn planner_retries_invalid_planner_output() {
    let tasks_json = r#"{"tasks":[{"id":"t1","objective":"do the thing","operation":"modify","targets":["thing.txt"],"depends_on":[]}]}"#;
    let provider = ScriptedProvider::from_strs(&[
        r#"{"status":"accepted","content":"Here is my plan: do things step by step."}"#,
        tasks_json,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "valid PlannerOutput on retry must succeed; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        2,
        "must retry once for invalid planner content"
    );
}

#[test]
fn planner_rejects_prose_content_in_coding_mode() {
    let provider = ScriptedProvider::from_strs(&[
        r#"{"status":"accepted","content":"Plan: first do this, then that."}"#,
        r#"{"status":"accepted","content":"Revised plan: still prose."}"#,
        r#"{"status":"accepted","content":"Final prose attempt."}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Failed { .. }),
        "prose planner content must fail after retries exhausted; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        3,
        "must attempt initial + MAX_PROTOCOL_RETRIES = 3 total calls"
    );
}

// ── Step 3: preamble detection ────────────────────────────────────────────

#[test]
fn role_response_with_preamble_fails() {
    let input = "Here is my answer:\n{\"status\":\"accepted\",\"content\":\"draft\"}";
    let result = parse_role_response(input);
    assert!(
        matches!(result, RoleResult::Failed { .. }),
        "preamble before JSON must be a protocol failure; got {result:?}"
    );
}

#[test]
fn clean_role_response_succeeds() {
    let input = r#"{"status":"accepted","content":"draft output"}"#;
    let result = parse_role_response(input);
    assert!(
        matches!(result, RoleResult::Accepted { ref content } if content == "draft output"),
        "clean JSON response must succeed; got {result:?}"
    );
}

#[test]
fn tool_request_detection_still_works_with_no_preamble() {
    // Tool requests starting with { are still detected and produce an error observation
    // (since tool_context is None), then the model returns a clean result.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"list_files"}"#,
        r#"{"status":"accepted","content":"listed files"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(producer_request("test"), &crate::telemetry::NoopTelemetry);

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "tool request without context must continue to final result; got {:?}",
        output.result
    );
    assert_eq!(provider.requests.borrow().len(), 2);
    assert!(
        provider.requests.borrow()[1]
            .prompt
            .contains("no file tools available"),
        "second prompt must include error observation from tool attempt"
    );
}

#[test]
fn preamble_triggers_retry_in_runner_loop() {
    // Preamble causes parse failure; on retry the model returns clean JSON.
    let provider = ScriptedProvider::from_strs(&[
        "Here is the result:\n{\"status\":\"accepted\",\"content\":\"draft\"}",
        r#"{"status":"accepted","content":"recovered"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        producer_request("produce output"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { ref content } if content == "recovered"),
        "clean JSON on retry must succeed; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        2,
        "must retry once after preamble failure"
    );
}

#[test]
fn planner_output_fallback_no_longer_hides_invalid_plan() {
    // Prose content that used to silently fall back to a single work node
    // now triggers retry and eventual failure.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"status":"accepted","content":"Do the task however you like."}"#,
        r#"{"status":"accepted","content":"Still prose."}"#,
        r#"{"status":"accepted","content":"More prose."}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Failed { .. }),
        "invalid planner content must no longer fall back silently; got {:?}",
        output.result
    );
}

// ── New direct-planner-output tests ──────────────────────────────────────

#[test]
fn planner_prompt_shows_direct_planner_output_schema() {
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tasks":[{"id":"t1","objective":"do the work","operation":"modify","targets":["work.txt"],"depends_on":[]}]}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("\"tasks\""),
        "planner prompt must show direct tasks schema; got:\n{prompt}"
    );
    assert!(
        prompt.contains("\"id\""),
        "planner prompt must show id field; got:\n{prompt}"
    );
    assert!(
        prompt.contains("\"objective\""),
        "planner prompt must show objective field; got:\n{prompt}"
    );
    assert!(
        prompt.contains("\"targets\""),
        "planner prompt must show targets field; got:\n{prompt}"
    );
    assert!(
        prompt.contains("\"depends_on\""),
        "planner prompt must show depends_on field; got:\n{prompt}"
    );
}

#[test]
fn planner_prompt_does_not_show_status_content_schema() {
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tasks":[{"id":"t1","objective":"do the work","operation":"modify","targets":["work.txt"],"depends_on":[]}]}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        !prompt.contains("\"status\""),
        "planner prompt must not show status/content wrapper; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("$RESPONSE_SUMMARY"),
        "planner prompt must not show accepted placeholder; got:\n{prompt}"
    );
}

#[test]
fn invalid_direct_planner_output_retries() {
    // Parses as PlannerOutput but has a self-dependency — validation must retry.
    let invalid_json = r#"{"tasks":[{"id":"loop","objective":"do loop","operation":"modify","targets":["loop.txt"],"depends_on":["loop"]}]}"#;
    let valid_json = r#"{"tasks":[{"id":"t1","objective":"do the work","operation":"modify","targets":["work.txt"],"depends_on":[]}]}"#;
    let provider = ScriptedProvider::from_strs(&[invalid_json, valid_json]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "must accept valid plan after retrying invalid one; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        2,
        "must retry once for validation failure"
    );
}

#[test]
fn planner_does_not_require_content_string_starting_with_brace() {
    // Regression: live failure produced {"status":"accepted","content":"{"}
    // which must fail cleanly, not produce PlanAccepted.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"status":"accepted","content":"{"}"#,
        r#"{"status":"accepted","content":"{"}"#,
        r#"{"status":"accepted","content":"{"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Failed { .. }),
        "status/content wrapper with truncated inner JSON must fail; got {:?}",
        output.result
    );
}

#[test]
fn worker_still_uses_status_content_schema() {
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        producer_request("write some code"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("\"status\""),
        "worker prompt must still contain status/content schema; got:\n{prompt}"
    );
    assert!(
        prompt.contains("$RESPONSE_SUMMARY"),
        "worker prompt must still contain accepted schema placeholder; got:\n{prompt}"
    );
    assert!(
        prompt.contains("$REASON_FOR_REJECTION"),
        "worker prompt must still contain rejected schema placeholder; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("\"tasks\""),
        "worker prompt must not contain the planner tasks schema; got:\n{prompt}"
    );
}

#[test]
fn critic_still_uses_status_content_schema() {
    let provider =
        ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"looks good"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        critic_request("review the draft", "some draft"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("\"status\""),
        "critic prompt must still contain status/content schema; got:\n{prompt}"
    );
    assert!(
        prompt.contains("$RESPONSE_SUMMARY"),
        "critic prompt must still contain accepted schema placeholder; got:\n{prompt}"
    );
}

#[test]
fn referee_still_uses_status_content_schema() {
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"approved"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        referee_request("approve the result", "content", "review"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("\"status\""),
        "referee prompt must still contain status/content schema; got:\n{prompt}"
    );
    assert!(
        prompt.contains("$RESPONSE_SUMMARY"),
        "referee prompt must still contain accepted schema placeholder; got:\n{prompt}"
    );
}

// ── tool observation protocol: anti-echo hardening ───────────────────────

#[test]
fn tool_observation_warns_not_to_copy_observation_json() {
    let (_temp, view) = make_view("obs-warn");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"completed"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_tool_context(producer_request("read hello.txt"), view),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let second_prompt = &requests[1].prompt;
    assert!(
        second_prompt.contains("Framework tool observation:"),
        "observation section must use 'Framework tool observation:' header; got:\n{second_prompt}"
    );
    assert!(
        second_prompt.contains("not a valid response format"),
        "observation section must warn model not to copy it; got:\n{second_prompt}"
    );
}

#[test]
fn successful_replace_text_observation_instructs_final_response() {
    // hello.txt from make_view contains "hello world\n"
    let (_temp, view) = make_view("replace-text-final");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"replace_text","path":"hello.txt","old":"hello world","new":"goodbye"}"#,
        r#"{"status":"accepted","content":"replaced hello with goodbye"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_tool_context(
            producer_request("replace hello with goodbye in hello.txt"),
            view,
        ),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2, "must call provider twice");
    let second_prompt = &requests[1].prompt;
    assert!(
        second_prompt.contains("The requested change has already been recorded."),
        "successful replace_text must include completion-pressure text; got:\n{second_prompt}"
    );
    assert!(
        second_prompt.contains("Do not call any more tools."),
        "successful replace_text must prohibit further tool calls; got:\n{second_prompt}"
    );
    assert!(
        !second_prompt.contains("Available file tools:"),
        "completion-pressure prompt must not include the tool section; got:\n{second_prompt}"
    );
}

#[test]
fn successful_mutation_tool_observation_instructs_final_response() {
    for (case, tool_response, final_response, objective) in [
        (
            "write_file",
            r#"{"tool":"write_file","path":"result.txt","content":"some output"}"#,
            r#"{"status":"accepted","content":"wrote result.txt"}"#,
            "write result.txt",
        ),
        (
            "delete_file",
            r#"{"tool":"delete_file","path":"old.txt"}"#,
            r#"{"status":"accepted","content":"deleted old.txt"}"#,
            "delete old.txt",
        ),
    ] {
        let provider = ScriptedProvider::from_strs(&[tool_response, final_response]);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            with_dummy_tool_context(producer_request(objective)),
            &crate::telemetry::NoopTelemetry,
        );

        let requests = provider.requests.borrow();
        assert_eq!(requests.len(), 2, "[{case}] must call provider twice");
        let second_prompt = &requests[1].prompt;
        assert!(
            second_prompt.contains("The requested change has already been recorded."),
            "[{case}] successful {case} must include completion-pressure text; got:\n{second_prompt}"
        );
        assert!(
            second_prompt.contains("Do not call any more tools."),
            "[{case}] successful {case} must prohibit further tool calls; got:\n{second_prompt}"
        );
        assert!(
            !second_prompt.contains("Available file tools:"),
            "[{case}] completion-pressure prompt must not include the tool section; got:\n{second_prompt}"
        );
    }
}

#[test]
fn read_file_after_mutation_is_completion_pressure_violation() {
    // Sequence: write_file (mutation → CP), read_file (CP violation → retry),
    // accepted. After completion pressure is active, any tool request —
    // including read_file — is treated as a protocol violation.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"data.txt","content":"hello"}"#,
        r#"{"tool":"read_file","path":"data.txt"}"#,
        r#"{"status":"accepted","content":"wrote data.txt"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(producer_request("write data.txt")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 3, "must call provider three times");
    // The third prompt must include the violation note ("tools are no longer available")
    // and must NOT include the tool section (CP rebuilt prompt from core).
    let third_prompt = &requests[2].prompt;
    assert!(
        third_prompt.contains("Tools are no longer available."),
        "read_file during CP must produce violation note; got:\n{third_prompt}"
    );
    assert!(
        !third_prompt.contains("Available file tools:"),
        "CP violation prompt must not contain the tool section; got:\n{third_prompt}"
    );
}

#[test]
fn observation_json_echo_triggers_protocol_retry_not_tool_execution() {
    use crate::telemetry::{TelemetryEvent, VecTelemetry};

    // Sequence: write_file (recorded), then model echoes the observation
    // JSON {"ok":true,"description":"write out.txt"} as its response,
    // then model finally returns accepted JSON.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"out.txt","content":"data"}"#,
        r#"{"ok":true,"description":"write out.txt"}"#,
        r#"{"status":"accepted","content":"completed"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);
    let telemetry = VecTelemetry::new();

    let output = runner.run_role(
        with_dummy_tool_context(producer_request("write out.txt")),
        &telemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "must recover from observation echo via protocol retry; got {:?}",
        output.result
    );
    let records = telemetry.records();
    // Only one ToolRequested event (for write_file) — the echo is NOT a tool call.
    let tool_requested_count = records
        .iter()
        .filter(|r| matches!(r.event, TelemetryEvent::ToolRequested { .. }))
        .count();
    assert_eq!(
        tool_requested_count, 1,
        "observation echo must not trigger ToolRequested; got {tool_requested_count}"
    );
    // The echo must trigger ParseFailed.
    assert!(
        records
            .iter()
            .any(|r| matches!(r.event, TelemetryEvent::ParseFailed { .. })),
        "observation echo must trigger ParseFailed"
    );
    // And ProtocolRetry.
    assert!(
        records
            .iter()
            .any(|r| matches!(r.event, TelemetryEvent::ProtocolRetry { .. })),
        "observation echo must trigger ProtocolRetry"
    );
}

// ── Completion pressure tests ────────────────────────────────────────────

#[test]
fn completion_pressure_hides_tool_section() {
    // After a successful mutation the prompt must not contain the tool section.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"out.txt","content":"data"}"#,
        r#"{"status":"accepted","content":"completed"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(producer_request("write out.txt")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let second_prompt = &requests[1].prompt;
    assert!(
        !second_prompt.contains("Available file tools:"),
        "completion-pressure prompt must not include the tool section; got:\n{second_prompt}"
    );
    assert!(
        !second_prompt.contains("write_file"),
        "completion-pressure prompt must not list write_file; got:\n{second_prompt}"
    );
}

#[test]
fn tool_request_after_completion_pressure_is_rejected() {
    use crate::telemetry::{TelemetryEvent, VecTelemetry};

    // Sequence: write_file (mutation → CP), list_files (CP violation → retry),
    // accepted (final response).
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"out.txt","content":"data"}"#,
        r#"{"tool":"list_files"}"#,
        r#"{"status":"accepted","content":"completed"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);
    let telemetry = VecTelemetry::new();

    let output = runner.run_role(
        with_dummy_tool_context(producer_request("write out.txt")),
        &telemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "must accept after CP violation is retried; got {:?}",
        output.result
    );
    assert_eq!(provider.requests.borrow().len(), 3);

    let records = telemetry.records();
    // list_files during CP must NOT emit ToolRequested.
    let tool_requested_count = records
        .iter()
        .filter(|r| matches!(r.event, TelemetryEvent::ToolRequested { .. }))
        .count();
    assert_eq!(
        tool_requested_count, 1,
        "only write_file must fire ToolRequested; CP violation must not; got {tool_requested_count}"
    );
    // CP violation must emit ParseFailed and ProtocolRetry.
    assert!(
        records.iter().any(
            |r| matches!(&r.event, TelemetryEvent::ParseFailed { parse_error, .. }
                    if parse_error.contains("no tools are available"))
        ),
        "CP violation must emit ParseFailed with 'no tools are available'"
    );
    assert!(
        records
            .iter()
            .any(|r| matches!(r.event, TelemetryEvent::ProtocolRetry { .. })),
        "CP violation must emit ProtocolRetry"
    );
}

#[test]
fn worker_can_return_accepted_after_completion_pressure() {
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"result.txt","content":"output data"}"#,
        r#"{"status":"accepted","content":"wrote result.txt with output data"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(producer_request("write result.txt")),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { ref content }
                if content == "wrote result.txt with output data"),
        "worker must be able to return Accepted after CP; got {:?}",
        output.result
    );
    let update = output
        .artifact_update
        .expect("write_file must produce an artifact update");
    assert_eq!(update.changes.len(), 1);
}

#[test]
fn planner_not_affected_by_completion_pressure() {
    // Plan+Producer: even if the planner returns a mutation-like tool request
    // (which it shouldn't, since tool_context is None), completion pressure
    // must never activate. Here we verify that the Planner takes the direct
    // PlannerOutput path without any CP interference.
    let tasks_json = r#"{"tasks":[{"id":"t1","objective":"do the work","operation":"modify","targets":["work.txt"],"depends_on":[]}]}"#;
    let provider = ScriptedProvider::from_strs(&[tasks_json]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "planner must succeed without CP interference; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        1,
        "planner must complete in one call"
    );
    let prompt = &provider.requests.borrow()[0].prompt;
    assert!(
        !prompt.contains("Do not call any more tools."),
        "planner prompt must not contain CP instruction; got:\n{prompt}"
    );
}

#[test]
fn critic_not_affected_by_completion_pressure() {
    // Critic role: even with tool context, CP must never activate (Critic is not Producer).
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"rejected","reason":"needs work"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(critic_request("review the draft", "draft")),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Rejected { .. }),
        "critic must succeed without CP interference; got {:?}",
        output.result
    );
    let prompt = &provider.requests.borrow()[0].prompt;
    assert!(
        !prompt.contains("Do not call any more tools."),
        "critic prompt must not contain CP instruction; got:\n{prompt}"
    );
}

#[test]
fn referee_not_affected_by_completion_pressure() {
    // Referee role: CP must never activate (Referee is not Producer).
    // Referee must read a file before accepting (enforcement); use a real
    // view so read_file("hello.txt") returns FileContents.
    let (_temp, view) = make_view("referee-no-cp");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(
            referee_request("approve the result", "content", "review"),
            view,
        ),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "referee must succeed without CP interference; got {:?}",
        output.result
    );
    let prompt = &provider.requests.borrow()[0].prompt;
    assert!(
        !prompt.contains("Do not call any more tools."),
        "referee prompt must not contain CP instruction; got:\n{prompt}"
    );
}

// ── write_file example hardening ─────────────────────────────────────────

#[test]
fn write_tool_example_does_not_use_output_txt() {
    let rw = render_tool_section(&FileToolPolicy {
        allow_writes: true,
        ..FileToolPolicy::default()
    });
    let write_file_pos = rw.find("write_file").expect("write_file must appear");
    let after_write = &rw[write_file_pos..];
    let next_brace = after_write
        .find('}')
        .expect("write_file line must have closing brace");
    let write_line = &after_write[..=next_brace];
    assert!(
        !write_line.contains("output.txt"),
        "write_file example must not use 'output.txt' as the path; got:\n{write_line}"
    );
}

#[test]
fn write_tool_example_does_not_use_hello_world() {
    let rw = render_tool_section(&FileToolPolicy {
        allow_writes: true,
        ..FileToolPolicy::default()
    });
    let write_file_pos = rw.find("write_file").expect("write_file must appear");
    let after_write = &rw[write_file_pos..];
    let next_brace = after_write
        .find('}')
        .expect("write_file line must have closing brace");
    let write_line = &after_write[..=next_brace];
    assert!(
        !write_line.contains("Hello, world!"),
        "write_file example must not use 'Hello, world!' as the content; got:\n{write_line}"
    );
}

// ── Decision pressure tests ──────────────────────────────────────────────

#[test]
fn critic_enters_decision_pressure_after_max_read_only_steps() {
    // After exactly MAX_READ_ONLY_TOOL_STEPS tool observations Critic must
    // receive a decision-pressure observation and then return a final result.
    let mut responses: Vec<&str> = vec![r#"{"tool":"list_files"}"#; MAX_READ_ONLY_TOOL_STEPS];
    responses.push(r#"{"status":"rejected","reason":"files look insufficient for the task"}"#);
    let provider = ScriptedProvider::from_strs(&responses);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(critic_request("review the draft", "draft content")),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Rejected { .. }),
        "critic must return final result after decision pressure; got {:?}",
        output.result
    );
    let requests = provider.requests.borrow();
    assert_eq!(
        requests.len(),
        MAX_READ_ONLY_TOOL_STEPS + 1,
        "provider must be called MAX_READ_ONLY_TOOL_STEPS + 1 times"
    );
    let last_prompt = &requests[MAX_READ_ONLY_TOOL_STEPS].prompt;
    assert!(
        last_prompt.contains("sufficient evidence"),
        "decision-pressure prompt must mention 'sufficient evidence'; got:\n{last_prompt}"
    );
    assert!(
        last_prompt.contains("Do not call any more tools."),
        "decision-pressure prompt must prohibit further tools; got:\n{last_prompt}"
    );
}

#[test]
fn referee_enters_decision_pressure_after_max_read_only_steps() {
    // Referee reads a file (step 1) then lists files (step 2 → DP fires),
    // then accepts.  The read_file call satisfies the file-read enforcement
    // and the tool-step count still hits MAX_READ_ONLY_TOOL_STEPS.
    let (_temp, view) = make_view("referee-dp-steps");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"tool":"list_files"}"#,
        r#"{"status":"accepted","content":"reviewed all evidence and approved"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(
            referee_request("approve the result", "content", "review"),
            view,
        ),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "referee must return final result after decision pressure; got {:?}",
        output.result
    );
    let requests = provider.requests.borrow();
    assert_eq!(
        requests.len(),
        MAX_READ_ONLY_TOOL_STEPS + 1,
        "provider must be called MAX_READ_ONLY_TOOL_STEPS + 1 times"
    );
    let last_prompt = &requests[MAX_READ_ONLY_TOOL_STEPS].prompt;
    assert!(
        last_prompt.contains("sufficient evidence"),
        "decision-pressure prompt must mention 'sufficient evidence'; got:\n{last_prompt}"
    );
}

#[test]
fn critic_decision_pressure_hides_tool_section() {
    let mut responses: Vec<&str> = vec![r#"{"tool":"list_files"}"#; MAX_READ_ONLY_TOOL_STEPS];
    responses
        .push(r#"{"status":"rejected","reason":"cannot determine quality without more context"}"#);
    let provider = ScriptedProvider::from_strs(&responses);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(critic_request("review the draft", "draft")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let pressure_prompt = &requests[MAX_READ_ONLY_TOOL_STEPS].prompt;
    assert!(
        !pressure_prompt.contains("Available file tools:"),
        "decision-pressure prompt must not include the tool section; got:\n{pressure_prompt}"
    );
    assert!(
        !pressure_prompt.contains("list_files"),
        "decision-pressure prompt must not list file tools; got:\n{pressure_prompt}"
    );
}

#[test]
fn critic_decision_pressure_rejects_further_tool_calls() {
    use crate::telemetry::{TelemetryEvent, VecTelemetry};

    // After MAX_READ_ONLY_TOOL_STEPS observations the (MAX+1)-th tool call
    // must be a protocol violation, then the model returns a final result.
    let mut responses: Vec<&str> = vec![r#"{"tool":"list_files"}"#; MAX_READ_ONLY_TOOL_STEPS];
    responses.push(r#"{"tool":"list_files"}"#); // violation
    responses.push(r#"{"status":"rejected","reason":"output does not meet requirements"}"#);
    let provider = ScriptedProvider::from_strs(&responses);
    let runner = ProviderRoleRunner::new(&provider);
    let telemetry = VecTelemetry::new();

    let output = runner.run_role(
        with_dummy_tool_context(critic_request("review the draft", "draft")),
        &telemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Rejected { .. }),
        "critic must reject after CP violation is retried; got {:?}",
        output.result
    );
    let records = telemetry.records();
    // The tool call after pressure must NOT emit ToolRequested.
    let tool_requested_count = records
        .iter()
        .filter(|r| matches!(r.event, TelemetryEvent::ToolRequested { .. }))
        .count();
    assert_eq!(
        tool_requested_count, MAX_READ_ONLY_TOOL_STEPS,
        "only the first {MAX_READ_ONLY_TOOL_STEPS} tool calls must emit ToolRequested; got {tool_requested_count}"
    );
    // Violation must emit ParseFailed with 'no tools are available'.
    assert!(
        records.iter().any(
            |r| matches!(&r.event, TelemetryEvent::ParseFailed { parse_error, .. }
                    if parse_error.contains("no tools are available"))
        ),
        "decision-pressure violation must emit ParseFailed with 'no tools are available'"
    );
}

#[test]
fn producer_not_affected_by_decision_pressure() {
    // Producer may use more than MAX_READ_ONLY_TOOL_STEPS distinct read-only
    // tool calls without entering decision pressure (which only applies to
    // Critic and Referee). Each read targets a different file so no repeated-
    // observation coercion fires either.
    let read_count = MAX_READ_ONLY_TOOL_STEPS + 1;
    let (_temp, view) = make_view_with_n_files("producer-no-dp", read_count);
    let mut responses: Vec<String> = (0..read_count)
        .map(|i| format!(r#"{{"tool":"read_file","path":"file{i}.txt"}}"#))
        .collect();
    responses.push(r#"{"status":"accepted","content":"produced the required output"}"#.to_string());
    let response_strs: Vec<&str> = responses.iter().map(|s| s.as_str()).collect();
    let provider = ScriptedProvider::from_strs(&response_strs);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(producer_request("read files and produce output"), view),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "producer must succeed with more than MAX_READ_ONLY_TOOL_STEPS distinct reads; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        read_count + 1,
        "producer must be allowed {read_count} distinct tool calls"
    );
    // None of the prompts must contain decision-pressure text.
    for (i, req) in provider.requests.borrow().iter().enumerate() {
        assert!(
            !req.prompt.contains("sufficient evidence"),
            "producer prompt[{i}] must not contain decision-pressure text; got:\n{}",
            req.prompt
        );
    }
}

#[test]
fn read_only_tool_steps_counter_is_per_invocation() {
    // Each invocation starts with a fresh counter. Two separate invocations
    // each with MAX_READ_ONLY_TOOL_STEPS - 1 tool steps must not trigger pressure.
    for _ in 0..2 {
        let provider = ScriptedProvider::from_strs(&[
            r#"{"tool":"list_files"}"#,
            r#"{"status":"rejected","reason":"draft does not satisfy the requirements"}"#,
        ]);
        let runner = ProviderRoleRunner::new(&provider);

        let output = runner.run_role(
            with_dummy_tool_context(critic_request("review the draft", "draft")),
            &crate::telemetry::NoopTelemetry,
        );

        assert!(
            matches!(output.result, RoleResult::Rejected { .. }),
            "critic with 1 tool step must succeed without decision pressure; got {:?}",
            output.result
        );
        let requests = provider.requests.borrow();
        assert_eq!(requests.len(), 2, "provider must be called twice");
        let second_prompt = &requests[1].prompt;
        assert!(
            !second_prompt.contains("sufficient evidence"),
            "second prompt must not contain decision-pressure text; got:\n{second_prompt}"
        );
    }
}

// ── read-file enforcement tests ───────────────────────────────────────────

#[test]
fn work_reviewer_must_read_file_before_accepting() {
    // Reviewer (Critic) first accepts without reading; enforcement fires and
    // a retry prompt is issued.  On the retry the reviewer calls read_file,
    // then accepts.  The final result must be Accepted.
    use crate::telemetry::{TelemetryEvent, VecTelemetry};

    let (_temp, view) = make_view("reviewer-read-enforce");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"status":"accepted","content":"looks good"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"confirmed after reading"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);
    let telemetry = VecTelemetry::new();

    let output = runner.run_role(
        with_tool_context(critic_request("review the work", "some content"), view),
        &telemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "reviewer must eventually accept after reading; got {:?}",
        output.result
    );
    let records = telemetry.into_records();
    let retries: Vec<_> = records
        .iter()
        .filter(|r| matches!(r.event, TelemetryEvent::ProtocolRetry { .. }))
        .collect();
    assert_eq!(
        retries.len(),
        1,
        "exactly one ProtocolRetry must be emitted for the enforcement violation"
    );
}

#[test]
fn work_reviewer_exhausts_retries_without_reading_fails() {
    // Reviewer accepts without reading on every attempt; after
    // MAX_PROTOCOL_RETRIES+1 tries the role must fail.
    let (_temp, view) = make_view("reviewer-exhaust-retries");
    let mut responses = vec![];
    for _ in 0..=MAX_PROTOCOL_RETRIES + 1 {
        responses.push(r#"{"status":"accepted","content":"looks good"}"#);
    }
    let provider = ScriptedProvider::from_strs(&responses);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(critic_request("review the work", "some content"), view),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Failed { .. }),
        "reviewer that never reads must fail after exhausting retries; got {:?}",
        output.result
    );
    if let RoleResult::Failed { reason, .. } = &output.result {
        assert!(
            reason.contains("reading"),
            "failure reason must mention reading; got: {reason}"
        );
    }
}

#[test]
fn plan_reviewer_can_accept_without_reading_files() {
    // Plan-node reviewers judge structure, not file contents.
    // The read-file enforcement must NOT apply to them.
    let provider =
        ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"plan is sound"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        RoleRequest {
            node_kind: NodeKind::Plan,
            ..critic_request("review the plan", "plan output")
        },
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "plan reviewer must accept without needing to read a file; got {:?}",
        output.result
    );
}

#[test]
fn work_reviewer_without_tool_context_can_accept() {
    // When tool_context is None the reviewer has no file tools; the
    // read-file enforcement must not apply in that case.
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"approved"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        referee_request("approve the result", "content", "review"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "reviewer without tool context must accept without enforcement; got {:?}",
        output.result
    );
}

// ── read-file enforcement regression tests ────────────────────────────────

#[test]
fn failed_read_file_does_not_satisfy_enforcement() {
    // A read_file that returns a failure (absolute path escapes workspace root)
    // must NOT set read_file_executed. The enforcement must fire even though
    // read_file was attempted, and the error message must include the role name
    // and the count of failed attempts.
    use crate::telemetry::{TelemetryEvent, VecTelemetry};

    let (_temp, view) = make_view("failed-read-enforcement");
    let provider = ScriptedProvider::from_strs(&[
        // Critic attempts read_file with an absolute path → fails (escapes workspace)
        r#"{"tool":"read_file","path":"/absolute/path/that/escapes"}"#,
        // Critic accepts without having successfully read → enforcement fires
        r#"{"status":"accepted","content":"looks good to me here"}"#,
        // After enforcement retry the Critic reads the valid file
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        // Critic accepts after successful read
        r#"{"status":"accepted","content":"confirmed after reading the file"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);
    let telemetry = VecTelemetry::new();

    let output = runner.run_role(
        with_tool_context(critic_request("review the work", "some content"), view),
        &telemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "Critic must accept after retry with successful read; got {:?}",
        output.result
    );
    let records = telemetry.into_records();
    let parse_failed = records
        .iter()
        .find(|r| matches!(r.event, TelemetryEvent::ParseFailed { .. }));
    assert!(
        parse_failed.is_some(),
        "ParseFailed must be emitted when read_file was attempted but failed"
    );
    if let Some(r) = parse_failed {
        if let TelemetryEvent::ParseFailed { parse_error, .. } = &r.event {
            assert!(
                parse_error.contains("Critic"),
                "error must name the role; got: {parse_error}"
            );
            assert!(
                parse_error.contains("1 read_file attempt(s) were made but all failed"),
                "error must report failed attempt count; got: {parse_error}"
            );
        }
    }
}

#[test]
fn failed_reads_exhaust_budget_enforcement_fails_directly() {
    // Two read_file calls fail with different errors (no repeated-obs coercion),
    // exhausting the reviewer tool budget (decision pressure fires).
    // The Critic then accepts → enforcement fires with final_response_only=true.
    // The fix: enforcement must fail DIRECTLY rather than issuing a must-read
    // retry that would contradict the blocked-tool state.
    // Outcome: exactly 3 provider calls (not 5+), clear failure message.
    let (_temp, view) = make_view("failed-reads-budget");
    let provider = ScriptedProvider::from_strs(&[
        // Read 1: absolute path → "path escapes the workspace root"
        r#"{"tool":"read_file","path":"/absolute/path"}"#,
        // Read 2: relative non-existent path → "file not found"
        // Different observation from Read 1 → no repeated-obs coercion.
        // read_only_tool_steps reaches MAX_READ_ONLY_TOOL_STEPS → decision pressure.
        r#"{"tool":"read_file","path":"nonexistent.txt"}"#,
        // Critic accepts under decision pressure → enforcement fires,
        // final_response_only=true → fail directly (no must-read retry issued).
        r#"{"status":"accepted","content":"looks good to me here"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(critic_request("review the work", "some content"), view),
        &crate::telemetry::NoopTelemetry,
    );

    let RoleResult::Failed { reason, .. } = &output.result else {
        panic!(
            "Critic must fail when all reads failed and tool budget exhausted; got {:?}",
            output.result
        );
    };
    assert!(
        reason.contains("Critic"),
        "failure reason must name the role; got: {reason}"
    );
    assert!(
        reason.contains("2 read_file attempt(s) were made but all failed"),
        "failure reason must report failed attempt count; got: {reason}"
    );
    assert_eq!(
        provider.requests.borrow().len(),
        3,
        "must be exactly 3 provider calls (no extra must-read retry after decision pressure)"
    );
}

#[test]
fn read_file_flag_survives_protocol_retry() {
    // Critic reads successfully (read_file_executed set), then returns bad JSON
    // triggering a protocol retry, then accepts. The read flag must survive the
    // protocol retry so that enforcement does not fire on the final accept.
    let (_temp, view) = make_view("read-flag-survives-retry");
    let provider = ScriptedProvider::from_strs(&[
        // Critic reads hello.txt → FileContents → read_file_executed = true
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        // Protocol failure (response does not start with '{')
        "not valid json at all here",
        // Critic accepts after protocol retry; flag was set → no enforcement
        r#"{"status":"accepted","content":"confirmed after reading the file"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(critic_request("review the work", "some content"), view),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "Critic must accept when read flag survived protocol retry; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        3,
        "provider must be called exactly 3 times (read + bad-json retry + final)"
    );
}

#[test]
fn referee_read_file_satisfies_enforcement() {
    // Referee must also read at least one file before accepting on Work nodes.
    // A single successful read must satisfy the enforcement for the Referee role.
    let (_temp, view) = make_view("referee-read-satisfies");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"referee confirmed the file contents"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(
            referee_request("approve the work", "content", "review"),
            view,
        ),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "Referee must accept after a successful read_file; got {:?}",
        output.result
    );
}
