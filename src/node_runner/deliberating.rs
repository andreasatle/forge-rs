//! NodeRunner backed by DeliberationMachine.

use crate::artifacts::{ArtifactUpdate, ArtifactView};
use crate::engine::{Machine, Transition, run_machine_with_telemetry};
use crate::machines::deliberation::{
    DeliberationEffect, DeliberationEvent, DeliberationMachine, DeliberationRequest,
    DeliberationState, DeliberationTerminalOutput, ProviderBackedDeliberationHandler,
};
use crate::machines::scheduler::{
    FailureKind, ModelTier, NodeFailure, NodeKind, RecoveryAction, WorkOutput,
};
use crate::providers::ProviderClient;
use crate::roles::RolePolicy;
use crate::telemetry::{TelemetryEvent, TelemetryRecord, TelemetrySink};

use super::classify::{classify_deliberation_failure, recovery_label};
use super::planner::try_fast_plan;
use super::runner::NodeRunner;
use super::types::{NodeRunRequest, NodeRunResult, NodeRunWorkResult};

/// Runs a node by driving a [`DeliberationMachine`] with a real provider.
///
/// Holds a separate provider and token budget for each [`ModelTier`]. On each
/// `run_node` call the runner inspects `request.model_tier` and routes to either
/// `cheap_provider` or `strong_provider`. When no strong provider is configured
/// the caller should pass the same provider for both tiers.
///
/// The final producer content is mapped to [`NodeRunResult`] by kind: plan nodes
/// produce one child work node whose objective is the producer content; work nodes
/// return the producer content as their summary and write it to `output.txt` in an
/// [`ArtifactUpdate`]. No JSON interpretation happens here — that boundary belongs
/// to the deliberation role handler.
///
/// When the request carries an [`ArtifactView`], a brief file listing (and
/// `README.md` if present) is prepended to the deliberation objective so the
/// producer has file context without any workspace mutation.
pub struct DeliberatingNodeRunner<C, S> {
    cheap_provider: C,
    strong_provider: S,
    cheap_max_tokens: u32,
    strong_max_tokens: u32,
    role_policy: RolePolicy,
    requires_tests: bool,
}

impl<C, S> DeliberatingNodeRunner<C, S> {
    /// Build a runner with separate cheap and strong providers.
    ///
    /// When no distinct strong provider is available, pass the same provider
    /// (or a reference to it) for both parameters — selection will still be
    /// explicit in the call site rather than accidental.
    pub fn new(cheap_provider: C, strong_provider: S) -> Self {
        Self {
            cheap_provider,
            strong_provider,
            cheap_max_tokens: 1024,
            strong_max_tokens: 1024,
            role_policy: RolePolicy::default(),
            requires_tests: false,
        }
    }

    /// Set the token budget forwarded to cheap-tier role calls.
    pub fn with_cheap_max_tokens(mut self, max_tokens: u32) -> Self {
        self.cheap_max_tokens = max_tokens;
        self
    }

    /// Set the token budget forwarded to strong-tier role calls.
    pub fn with_strong_max_tokens(mut self, max_tokens: u32) -> Self {
        self.strong_max_tokens = max_tokens;
        self
    }

    /// Override the role prompt policy supplied to each role invocation.
    ///
    /// The policy is cloned once per node run and forwarded to the deliberation
    /// handler. The default is [`RolePolicy::default()`], which preserves the
    /// hardcoded behaviour.
    pub fn with_role_policy(mut self, policy: RolePolicy) -> Self {
        self.role_policy = policy;
        self
    }

    /// Require planner output for code changes to include test-related targets.
    pub fn with_requires_tests(mut self, requires_tests: bool) -> Self {
        self.requires_tests = requires_tests;
        self
    }
}

struct DeliberatingMachine<'a, P: ProviderClient> {
    handler: ProviderBackedDeliberationHandler<&'a P>,
    telemetry: &'a dyn TelemetrySink,
}

impl<'a, P: ProviderClient> DeliberatingMachine<'a, P> {
    /// Returns the artifact update accumulated by file tool loops during the
    /// machine run, clearing the internal buffer.
    fn take_artifact_update(&self) -> Option<ArtifactUpdate> {
        self.handler.take_artifact_update()
    }
}

impl<'a, P: ProviderClient> Machine for DeliberatingMachine<'a, P> {
    type State = DeliberationState;
    type Event = DeliberationEvent;
    type Effect = DeliberationEffect;
    type Output = DeliberationTerminalOutput;

    fn start_event(&self) -> DeliberationEvent {
        DeliberationEvent::Start
    }

    fn transition(
        &self,
        state: DeliberationState,
        event: DeliberationEvent,
    ) -> Transition<DeliberationState, DeliberationEffect> {
        DeliberationMachine.transition(state, event)
    }

    fn handle_effect(&self, effect: DeliberationEffect) -> DeliberationEvent {
        self.handler
            .handle_effect_with_telemetry(effect, self.telemetry)
    }

    fn output(&self, state: &DeliberationState) -> Option<DeliberationTerminalOutput> {
        DeliberationMachine.output(state)
    }

    fn name(&self) -> String {
        "DeliberationMachine".to_string()
    }
}

impl<C: ProviderClient, S: ProviderClient> NodeRunner for DeliberatingNodeRunner<C, S> {
    fn run_node(&self, request: NodeRunRequest, telemetry: &dyn TelemetrySink) -> NodeRunResult {
        // Fast path: bypass LLM for plan nodes whose objective names exactly one source file.
        if request.kind == NodeKind::Plan
            && let Some(plan) = try_fast_plan(&request.objective, self.requires_tests)
        {
            let task_count = plan.children.len();
            telemetry.record(TelemetryRecord::new(
                "DeliberatingNodeRunner",
                TelemetryEvent::FastPlanUsed { task_count },
            ));
            return NodeRunResult::PlanAccepted(plan);
        }

        match request.model_tier {
            ModelTier::Cheap => run_with_provider(
                &self.cheap_provider,
                request,
                self.cheap_max_tokens,
                &self.role_policy,
                self.requires_tests,
                telemetry,
            ),
            ModelTier::Strong => run_with_provider(
                &self.strong_provider,
                request,
                self.strong_max_tokens,
                &self.role_policy,
                self.requires_tests,
                telemetry,
            ),
        }
    }
}

fn run_with_provider<P: ProviderClient>(
    provider: &P,
    request: NodeRunRequest,
    max_tokens: u32,
    policy: &RolePolicy,
    requires_tests: bool,
    telemetry: &dyn TelemetrySink,
) -> NodeRunResult {
    let top_objective = request.objective.clone();
    let existing_files: Vec<String> = request
        .artifact_view
        .as_ref()
        .and_then(|v| v.list_files().ok())
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let plan_validation_context = if request.kind == NodeKind::Plan {
        Some((top_objective, existing_files, requires_tests))
    } else if existing_files.is_empty() && !requires_tests {
        None
    } else {
        Some((top_objective, existing_files, requires_tests))
    };

    let objective = enrich_objective(&request, requires_tests);
    let delib_request = DeliberationRequest {
        objective,
        max_revisions: 1,
    };
    let initial_state = DeliberationState::Ready {
        request: delib_request,
    };
    let machine = DeliberatingMachine {
        handler: ProviderBackedDeliberationHandler::new_with_view(
            provider,
            request.artifact_view.clone(),
            max_tokens,
            request.kind.clone(),
            policy.clone(),
            plan_validation_context,
        ),
        telemetry,
    };
    let (output, machine) = run_machine_with_telemetry(machine, initial_state, telemetry);
    let tool_artifact_update = machine.take_artifact_update();
    map_output(output, request.kind, tool_artifact_update, telemetry)
}

/// Returns the objective string, optionally prefixed with artifact file context.
fn enrich_objective(request: &NodeRunRequest, requires_tests: bool) -> String {
    let testing_context = if requires_tests {
        Some(
            "Testing requirement: project validation includes a test command. Code changes require corresponding tests, and plans for code changes must include at least one test-related target.".to_string(),
        )
    } else {
        None
    };
    let Some(view) = &request.artifact_view else {
        return match testing_context {
            Some(context) => format!("{context}\n\nObjective: {}", request.objective),
            None => request.objective.clone(),
        };
    };
    let context = build_artifact_context(view);
    if context.is_empty() {
        return match testing_context {
            Some(testing_context) => {
                format!("{testing_context}\n\nObjective: {}", request.objective)
            }
            None => request.objective.clone(),
        };
    }
    match testing_context {
        Some(testing_context) => {
            format!(
                "{context}\n\n{testing_context}\n\nObjective: {}",
                request.objective
            )
        }
        None => format!("{context}\n\nObjective: {}", request.objective),
    }
}

/// Builds a short context string from a read-only artifact view.
///
/// Lists all files under a heading that signals they already exist and must
/// not be recreated unless the objective explicitly names them. If `README.md`
/// is present its content is included after the listing.
///
/// Returns an empty string when the view has no files or when git fails.
fn build_artifact_context(view: &ArtifactView) -> String {
    let files = match view.list_files() {
        Ok(f) if !f.is_empty() => f,
        _ => return String::new(),
    };
    let mut parts = Vec::new();
    let listing: Vec<String> = files.iter().map(|p| format!("  {}", p.display())).collect();
    parts.push(format!(
        "Existing project files (already initialized — do not create tasks to recreate \
         or reinitialize these files unless the objective explicitly names them as targets):\n{}",
        listing.join("\n")
    ));
    if let Ok(readme) = view.read_file("README.md") {
        parts.push(format!("README.md:\n{readme}"));
    }
    parts.join("\n\n")
}

fn map_output(
    output: DeliberationTerminalOutput,
    kind: NodeKind,
    tool_artifact_update: Option<ArtifactUpdate>,
    telemetry: &dyn TelemetrySink,
) -> NodeRunResult {
    match output {
        DeliberationTerminalOutput::Complete(out) => match kind {
            NodeKind::Plan => map_plan_output(out.content, telemetry),
            NodeKind::Work => NodeRunResult::WorkAccepted(NodeRunWorkResult {
                work: WorkOutput {
                    summary: out.content,
                },
                artifact_update: tool_artifact_update,
            }),
        },
        DeliberationTerminalOutput::Failed { kind, reason } => {
            let recovery = classify_deliberation_failure(kind, &reason);
            telemetry.record(TelemetryRecord::new(
                "DeliberatingNodeRunner",
                TelemetryEvent::FailureClassified {
                    reason: reason.clone(),
                    recovery: recovery_label(&recovery).to_string(),
                },
            ));
            NodeRunResult::Failed(NodeFailure {
                kind,
                message: reason,
                recovery,
            })
        }
    }
}

/// Map a plan node's raw content to a [`NodeRunResult`].
///
/// Attempts to parse `content` as a structured [`PlannerOutput`] JSON object.
///
/// - If parsing succeeds and the graph is structurally valid: emits
///   `PlannerOutputParsed` and returns `PlanAccepted` with one `NodeRequest`
///   per task. No-recreate validation has already been enforced by the handler
///   before the content reaches this function.
/// - If parsing succeeds but structural validation fails: emits
///   `PlannerOutputValidationFailed` and returns `Failed` with `Terminal`
///   recovery.
/// - If parsing fails (prose or unexpected schema): emits
///   `PlannerOutputFallback` and returns `Failed` with `Terminal` recovery.
fn map_plan_output(content: String, telemetry: &dyn TelemetrySink) -> NodeRunResult {
    use crate::node_runner::planner::{
        parse_planner_content, planner_output_to_plan_output, validate_planner_output,
    };

    match parse_planner_content(&content) {
        Some(planner_out) => match validate_planner_output(&planner_out) {
            Ok(()) => {
                let task_count = planner_out.tasks.len();
                let dependency_count: usize =
                    planner_out.tasks.iter().map(|t| t.depends_on.len()).sum();
                telemetry.record(TelemetryRecord::new(
                    "DeliberatingNodeRunner",
                    TelemetryEvent::PlannerOutputParsed {
                        task_count,
                        dependency_count,
                    },
                ));
                NodeRunResult::PlanAccepted(planner_output_to_plan_output(planner_out))
            }
            Err(e) => {
                let reason = e.to_string();
                telemetry.record(TelemetryRecord::new(
                    "DeliberatingNodeRunner",
                    TelemetryEvent::PlannerOutputValidationFailed {
                        reason: reason.clone(),
                    },
                ));
                NodeRunResult::Failed(NodeFailure {
                    kind: FailureKind::PlannerValidationFailure,
                    message: reason.clone(),
                    recovery: RecoveryAction::Terminal {
                        message: format!("planner output validation failed: {reason}"),
                    },
                })
            }
        },
        None => {
            // Planner content was not valid PlannerOutput JSON.
            // This path should be unreachable when runner validation is active,
            // but if reached it must fail loudly rather than silently substituting
            // a single work node.
            let reason = "planner content is not valid PlannerOutput JSON".to_string();
            telemetry.record(TelemetryRecord::new(
                "DeliberatingNodeRunner",
                TelemetryEvent::PlannerOutputFallback,
            ));
            NodeRunResult::Failed(NodeFailure {
                kind: FailureKind::PlannerValidationFailure,
                message: reason.clone(),
                recovery: RecoveryAction::Terminal {
                    message: format!("planner output invalid: {reason}"),
                },
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::artifacts::{ArtifactView, FileChange};
    use crate::machines::scheduler::{ModelTier, NodeId};
    use crate::providers::{ProviderError, ProviderErrorKind, ProviderRequest, ProviderResponse};
    use crate::telemetry::NoopTelemetry;

    /// Provider that records the `max_tokens` from the first request it receives.
    struct CapturingProvider {
        max_tokens: RefCell<Option<u32>>,
        responses: RefCell<VecDeque<String>>,
    }

    impl CapturingProvider {
        fn from_strs(responses: &[&str]) -> Self {
            Self {
                max_tokens: RefCell::new(None),
                responses: RefCell::new(responses.iter().map(|s| s.to_string()).collect()),
            }
        }

        fn captured_max_tokens(&self) -> Option<u32> {
            *self.max_tokens.borrow()
        }
    }

    impl ProviderClient for CapturingProvider {
        fn call(&self, req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
            if self.max_tokens.borrow().is_none() {
                *self.max_tokens.borrow_mut() = Some(req.max_tokens);
            }
            let content = self
                .responses
                .borrow_mut()
                .pop_front()
                .expect("CapturingProvider: responses exhausted");
            Ok(ProviderResponse {
                content,
                finish_reason: None,
            })
        }
    }

    struct ScriptedProvider {
        responses: RefCell<VecDeque<Result<String, ProviderError>>>,
    }

    impl ScriptedProvider {
        fn from_strs(responses: &[&str]) -> Self {
            Self {
                responses: RefCell::new(responses.iter().map(|s| Ok(s.to_string())).collect()),
            }
        }

        fn failing(kind: ProviderErrorKind, message: &str) -> Self {
            Self {
                responses: RefCell::new(VecDeque::from([Err(ProviderError {
                    kind,
                    message: message.to_string(),
                })])),
            }
        }
    }

    impl ProviderClient for ScriptedProvider {
        fn call(&self, _req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
            self.responses
                .borrow_mut()
                .pop_front()
                .expect("ScriptedProvider: responses exhausted")
                .map(|content| ProviderResponse {
                    content,
                    finish_reason: None,
                })
        }
    }

    /// Provider that records every prompt it receives, then returns a fixed response.
    struct RecordingProvider {
        prompts: RefCell<Vec<String>>,
        responses: RefCell<VecDeque<String>>,
    }

    impl RecordingProvider {
        fn from_strs(responses: &[&str]) -> Self {
            Self {
                prompts: RefCell::new(Vec::new()),
                responses: RefCell::new(responses.iter().map(|s| s.to_string()).collect()),
            }
        }

        fn recorded_prompts(&self) -> Vec<String> {
            self.prompts.borrow().clone()
        }
    }

    impl ProviderClient for RecordingProvider {
        fn call(&self, req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
            self.prompts.borrow_mut().push(req.prompt.clone());
            let content = self
                .responses
                .borrow_mut()
                .pop_front()
                .expect("RecordingProvider: responses exhausted");
            Ok(ProviderResponse {
                content,
                finish_reason: None,
            })
        }
    }

    fn plan_request(objective: &str) -> NodeRunRequest {
        NodeRunRequest {
            kind: NodeKind::Plan,
            objective: objective.to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
            artifact_view: None,
        }
    }

    fn work_request(objective: &str) -> NodeRunRequest {
        NodeRunRequest {
            kind: NodeKind::Work,
            objective: objective.to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
            artifact_view: None,
        }
    }

    fn strong_work_request(objective: &str) -> NodeRunRequest {
        NodeRunRequest {
            kind: NodeKind::Work,
            objective: objective.to_string(),
            model_tier: ModelTier::Strong,
            attempt: 0,
            artifact_view: None,
        }
    }

    // --- git helpers for artifact_view tests ---

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let seq = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "forge-deliberating-{label}-{}-{seq}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("failed to create temp dir");
            Self(path)
        }

        fn join(&self, s: &str) -> PathBuf {
            self.0.join(s)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn git(path: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("failed to run git");
        assert!(status.success(), "git {} failed", args.join(" "));
    }

    fn git_output(path: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("failed to run git");
        assert!(out.status.success(), "git {} failed", args.join(" "));
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// Creates a bare git repo with a single committed file and returns an ArtifactView.
    fn make_artifact_view(temp: &TempDir, filename: &str, content: &str) -> ArtifactView {
        let seed = temp.join("seed");
        fs::create_dir_all(&seed).unwrap();
        git(&seed, &["init", "--quiet", "--initial-branch=main"]);
        git(&seed, &["config", "user.name", "Test"]);
        git(&seed, &["config", "user.email", "test@example.invalid"]);
        fs::write(seed.join(filename), content).unwrap();
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "--quiet", "-m", "init"]);
        let bare = temp.join("artifact.git");
        let status = Command::new("git")
            .args(["clone", "--quiet", "--bare"])
            .arg(&seed)
            .arg(&bare)
            .status()
            .expect("git clone --bare failed");
        assert!(status.success());
        let commit_sha = git_output(&bare, &["rev-parse", "HEAD"]);
        ArtifactView {
            repo_path: bare,
            commit_sha,
        }
    }

    // --- existing tests (updated for new WorkAccepted shape) ---

    #[test]
    fn deliberating_runner_plan_returns_plan_output() {
        let tasks_json = r#"{"tasks":[{"id":"task-1","objective":"the actual work","operation":"modify","targets":["work.txt"],"depends_on":[]}]}"#;
        let provider = ScriptedProvider::from_strs(&[
            tasks_json,
            r#"{"status":"accepted","content":"looks good"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let result = runner.run_node(plan_request("plan the work"), &NoopTelemetry);
        let NodeRunResult::PlanAccepted(plan) = result else {
            panic!("expected PlanAccepted");
        };
        assert_eq!(plan.children.len(), 1);
        assert_eq!(plan.children[0].kind, NodeKind::Work);
        assert_eq!(
            plan.children[0].objective,
            "the actual work\n\nTarget files: work.txt"
        );
    }

    #[test]
    fn deliberating_runner_work_returns_work_output() {
        let provider = ScriptedProvider::from_strs(&[
            r#"{"status":"accepted","content":"finished the task"}"#,
            r#"{"status":"accepted","content":"review ok"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let result = runner.run_node(work_request("write some code"), &NoopTelemetry);
        let NodeRunResult::WorkAccepted(work_result) = result else {
            panic!("expected WorkAccepted");
        };
        assert_eq!(work_result.work.summary, "finished the task");
    }

    #[test]
    fn deliberating_runner_provider_failure_returns_failed() {
        let provider = ScriptedProvider::failing(
            ProviderErrorKind::Retryable,
            "connection refused on http://localhost:8080/completion",
        );
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let result = runner.run_node(work_request("do something"), &NoopTelemetry);
        let NodeRunResult::Failed(failure) = result else {
            panic!("expected Failed");
        };
        assert!(
            failure
                .message
                .contains("provider error (Retryable): connection refused")
        );
        assert!(matches!(failure.recovery, RecoveryAction::Retry { .. }));
    }

    #[test]
    fn deliberating_runner_preserves_deliberation_failure_reason() {
        let provider = ScriptedProvider::failing(
            ProviderErrorKind::Retryable,
            "connection refused on http://localhost:8080/completion",
        );
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let result = runner.run_node(work_request("do something"), &NoopTelemetry);
        let NodeRunResult::Failed(failure) = result else {
            panic!("expected Failed");
        };
        assert!(
            failure
                .message
                .contains("provider error (Retryable): connection refused")
        );
        let RecoveryAction::Retry { message } = failure.recovery else {
            panic!("expected retry recovery for retryable provider error");
        };
        assert!(
            message.contains("provider error (Retryable): connection refused"),
            "retry message must include the original reason; got: {message}"
        );
    }

    #[test]
    fn deliberating_runner_revision_uses_latest_producer_content() {
        let provider = ScriptedProvider::from_strs(&[
            r#"{"status":"accepted","content":"draft v1"}"#,
            r#"{"status":"accepted","content":"review done"}"#,
            r#"{"status":"rejected","reason":"needs work"}"#,
            r#"{"status":"accepted","content":"draft v2"}"#,
            r#"{"status":"accepted","content":"review ok"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let result = runner.run_node(work_request("refine the plan"), &NoopTelemetry);
        let NodeRunResult::WorkAccepted(work_result) = result else {
            panic!("expected WorkAccepted");
        };
        assert_eq!(work_result.work.summary, "draft v2");
    }

    #[test]
    fn deliberating_runner_preserves_deliberation_failure() {
        let provider = ScriptedProvider::from_strs(&[
            "not valid json at all",
            "still not valid json",
            "also not valid json",
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let result = runner.run_node(work_request("do something"), &NoopTelemetry);
        let NodeRunResult::Failed(failure) = result else {
            panic!("expected Failed");
        };
        assert!(matches!(failure.recovery, RecoveryAction::Retry { .. }));
    }

    // --- new tests ---

    #[test]
    fn worker_without_tool_update_does_not_create_output_txt() {
        let provider = ScriptedProvider::from_strs(&[
            r#"{"status":"accepted","content":"output content"}"#,
            r#"{"status":"accepted","content":"review ok"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let result = runner.run_node(work_request("produce some output"), &NoopTelemetry);
        let NodeRunResult::WorkAccepted(work_result) = result else {
            panic!("expected WorkAccepted");
        };
        assert!(
            work_result.artifact_update.is_none(),
            "worker with no tool calls must produce no artifact_update; got {:?}",
            work_result.artifact_update
        );
    }

    #[test]
    fn worker_summary_is_not_converted_to_artifact_update() {
        let provider = ScriptedProvider::from_strs(&[
            r#"{"status":"accepted","content":"summary of the work done"}"#,
            r#"{"status":"accepted","content":"review ok"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let result = runner.run_node(work_request("do some work"), &NoopTelemetry);
        let NodeRunResult::WorkAccepted(work_result) = result else {
            panic!("expected WorkAccepted");
        };
        assert_eq!(work_result.work.summary, "summary of the work done");
        assert!(
            work_result.artifact_update.is_none(),
            "summary must not be converted to an artifact update; got {:?}",
            work_result.artifact_update
        );
    }

    #[test]
    fn artifact_view_context_is_visible_to_deliberation_prompt() {
        let temp = TempDir::new("prompt-context");
        let view = make_artifact_view(&temp, "hello.txt", "world\n");

        // Critic and Referee are Work reviewers and must call read_file before
        // accepting.  Add read_file("hello.txt") calls for each reviewer.
        let provider = RecordingProvider::from_strs(&[
            r#"{"status":"accepted","content":"draft output"}"#,
            r#"{"tool":"read_file","path":"hello.txt"}"#,
            r#"{"status":"accepted","content":"review ok"}"#,
            r#"{"tool":"read_file","path":"hello.txt"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let request = NodeRunRequest {
            kind: NodeKind::Work,
            objective: "do the thing".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
            artifact_view: Some(view),
        };
        runner.run_node(request, &NoopTelemetry);

        let prompts = provider.recorded_prompts();
        assert!(!prompts.is_empty(), "provider must have received prompts");
        let first = &prompts[0];
        assert!(
            first.contains("hello.txt"),
            "first prompt must list artifact files; got:\n{first}"
        );
        assert!(
            first.contains("do the thing"),
            "first prompt must include the original objective; got:\n{first}"
        );
    }

    #[test]
    fn deliberating_runner_threads_max_tokens_to_provider() {
        let provider = CapturingProvider::from_strs(&[
            r#"{"status":"accepted","content":"task completed"}"#,
            r#"{"status":"accepted","content":"review done"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider).with_cheap_max_tokens(256);
        runner.run_node(work_request("test threading"), &NoopTelemetry);

        assert_eq!(
            provider.captured_max_tokens(),
            Some(256),
            "with_cheap_max_tokens must propagate to the provider request"
        );
    }

    #[test]
    fn deliberating_work_result_includes_tool_artifact_update() {
        let temp = TempDir::new("tool-artifact-update");
        let view = make_artifact_view(&temp, "hello.txt", "world\n");

        // Producer: first call returns write_file, second returns accepted.
        // Critic and Referee must call read_file before accepting (enforcement).
        let provider = ScriptedProvider::from_strs(&[
            r#"{"tool":"write_file","path":"result.txt","content":"done"}"#,
            r#"{"status":"accepted","content":"I wrote result.txt"}"#,
            r#"{"tool":"read_file","path":"hello.txt"}"#,
            r#"{"status":"accepted","content":"looks good"}"#,
            r#"{"tool":"read_file","path":"hello.txt"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let request = NodeRunRequest {
            kind: NodeKind::Work,
            objective: "write a result file".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
            artifact_view: Some(view),
        };
        let result = runner.run_node(request, &NoopTelemetry);

        let NodeRunResult::WorkAccepted(work_result) = result else {
            panic!("expected WorkAccepted");
        };
        assert_eq!(
            work_result.work.summary, "I wrote result.txt",
            "summary must be the accepted content, not the tool request"
        );
        let update = work_result
            .artifact_update
            .expect("tool write_file must produce an artifact_update");
        assert_eq!(
            update.changes.len(),
            1,
            "must have exactly one pending change"
        );
        match &update.changes[0] {
            FileChange::Write { path, content } => {
                assert_eq!(path, "result.txt");
                assert_eq!(content, "done");
            }
            other => panic!("expected Write change from tool, got {other:?}"),
        }
    }

    // --- recovery classification runtime tests ---

    #[test]
    fn retryable_failure_produces_retry_action() {
        let provider = ScriptedProvider::failing(
            ProviderErrorKind::Retryable,
            "connection refused on http://localhost:8080/completion",
        );
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let telemetry = crate::telemetry::VecTelemetry::new();
        let result = runner.run_node(work_request("do something"), &telemetry);
        let NodeRunResult::Failed(failure) = result else {
            panic!("expected Failed");
        };
        assert!(
            matches!(failure.recovery, RecoveryAction::Retry { .. }),
            "retryable provider error must produce Retry recovery"
        );
        let records = telemetry.into_records();
        let classified = records.iter().find(|r| {
            matches!(
                r.event,
                crate::telemetry::TelemetryEvent::FailureClassified { .. }
            )
        });
        assert!(
            classified.is_some(),
            "must emit FailureClassified telemetry"
        );
        if let Some(r) = classified {
            match &r.event {
                crate::telemetry::TelemetryEvent::FailureClassified { recovery, .. } => {
                    assert_eq!(recovery, "Retry");
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn semantic_failure_produces_elevate_action() {
        // Revision limit exhaustion is a semantic failure. The runner allows 1 revision,
        // so both Referee rejections are needed to exhaust the budget and produce
        // "revision limit exhausted: ..." → ElevateModel.
        let provider = ScriptedProvider::from_strs(&[
            // Round 1: Producer → Critic → Referee rejects → revision loop.
            r#"{"status":"accepted","content":"draft v1"}"#,
            r#"{"status":"accepted","content":"review ok"}"#,
            r#"{"status":"rejected","reason":"needs improvement"}"#,
            // Round 2: Producer → Critic → Referee rejects → budget exhausted.
            r#"{"status":"accepted","content":"draft v2"}"#,
            r#"{"status":"accepted","content":"review ok"}"#,
            r#"{"status":"rejected","reason":"still not good enough"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let telemetry = crate::telemetry::VecTelemetry::new();
        let result = runner.run_node(work_request("do something"), &telemetry);
        let NodeRunResult::Failed(failure) = result else {
            panic!("expected Failed; got success or plan");
        };
        assert!(
            matches!(failure.recovery, RecoveryAction::ElevateModel { .. }),
            "semantic failure must produce ElevateModel recovery; got {:?}",
            failure.recovery
        );
        let records = telemetry.into_records();
        let classified = records.iter().find(|r| {
            matches!(
                r.event,
                crate::telemetry::TelemetryEvent::FailureClassified { .. }
            )
        });
        assert!(
            classified.is_some(),
            "must emit FailureClassified telemetry"
        );
        if let Some(r) = classified {
            match &r.event {
                crate::telemetry::TelemetryEvent::FailureClassified { recovery, .. } => {
                    assert_eq!(recovery, "ElevateModel");
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn terminal_failure_produces_terminal_action() {
        let provider = ScriptedProvider::failing(ProviderErrorKind::Terminal, "invalid api key");
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let telemetry = crate::telemetry::VecTelemetry::new();
        let result = runner.run_node(work_request("do something"), &telemetry);
        let NodeRunResult::Failed(failure) = result else {
            panic!("expected Failed");
        };
        assert!(
            matches!(failure.recovery, RecoveryAction::Terminal { .. }),
            "fatal auth failure must produce Terminal recovery"
        );
        let records = telemetry.into_records();
        let classified = records.iter().find(|r| {
            matches!(
                r.event,
                crate::telemetry::TelemetryEvent::FailureClassified { .. }
            )
        });
        assert!(
            classified.is_some(),
            "must emit FailureClassified telemetry"
        );
        if let Some(r) = classified {
            match &r.event {
                crate::telemetry::TelemetryEvent::FailureClassified { recovery, .. } => {
                    assert_eq!(recovery, "Terminal");
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn deliberation_failure_produces_elevate_action_independent_of_message_text() {
        // Referee rejects with "task too large" twice, exhausting the revision budget.
        // The failure reason is "revision limit exhausted: task too large".
        // The classifier checks task-shape signals (Split) before revision-exhaustion
        // (ElevateModel), so "task too large" wins and maps to Split.
        let provider = ScriptedProvider::from_strs(&[
            // Round 1: Producer → Critic → Referee rejects "task too large" → revision loop.
            r#"{"status":"accepted","content":"draft v1"}"#,
            r#"{"status":"accepted","content":"review ok"}"#,
            r#"{"status":"rejected","reason":"task too large"}"#,
            // Round 2: Producer → Critic → Referee rejects "task too large" → budget exhausted.
            r#"{"status":"accepted","content":"draft v2"}"#,
            r#"{"status":"accepted","content":"review ok"}"#,
            r#"{"status":"rejected","reason":"task too large"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let telemetry = crate::telemetry::VecTelemetry::new();
        let result = runner.run_node(work_request("do something"), &telemetry);
        let NodeRunResult::Failed(failure) = result else {
            panic!("expected Failed");
        };
        assert!(
            matches!(failure.recovery, RecoveryAction::ElevateModel { .. }),
            "typed deliberation failure must produce ElevateModel recovery; got {:?}",
            failure.recovery
        );
        let records = telemetry.into_records();
        let classified = records.iter().find(|r| {
            matches!(
                r.event,
                crate::telemetry::TelemetryEvent::FailureClassified { .. }
            )
        });
        assert!(
            classified.is_some(),
            "must emit FailureClassified telemetry"
        );
        if let Some(r) = classified {
            match &r.event {
                crate::telemetry::TelemetryEvent::FailureClassified { recovery, .. } => {
                    assert_eq!(recovery, "ElevateModel");
                }
                _ => unreachable!(),
            }
        }
    }

    // --- planner output tests ---

    #[test]
    fn prose_planner_content_triggers_retry_and_fails() {
        // Step 2: prose content is no longer silently accepted as a single work node.
        // The runner validates the accepted content as PlannerOutput and retries.
        // After MAX_PROTOCOL_RETRIES the plan node returns Failed.
        let provider = ScriptedProvider::from_strs(&[
            r#"{"status":"accepted","content":"Just do the work however you see fit."}"#,
            r#"{"status":"accepted","content":"Still prose, not JSON."}"#,
            r#"{"status":"accepted","content":"Also prose."}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let telemetry = crate::telemetry::VecTelemetry::new();
        let result = runner.run_node(plan_request("plan the work"), &telemetry);

        let NodeRunResult::Failed(_) = result else {
            panic!("expected Failed after prose planner content exhausts retries");
        };

        // PlannerOutputFallback must NOT be emitted: validation fails in the runner before
        // map_plan_output is reached.
        let records = telemetry.into_records();
        let has_fallback = records.iter().any(|r| {
            matches!(
                r.event,
                crate::telemetry::TelemetryEvent::PlannerOutputFallback
            )
        });
        assert!(
            !has_fallback,
            "PlannerOutputFallback must not be emitted when runner validation fails first"
        );
    }

    #[test]
    fn structured_planner_output_creates_multiple_work_nodes() {
        let tasks_json = r#"{"tasks":[{"id":"alpha","objective":"do alpha","operation":"modify","targets":["alpha.txt"],"depends_on":[]},{"id":"beta","objective":"do beta","operation":"modify","targets":["beta.txt"],"depends_on":["alpha"]}]}"#;
        let provider = ScriptedProvider::from_strs(&[
            tasks_json,
            r#"{"status":"accepted","content":"looks good"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let telemetry = crate::telemetry::VecTelemetry::new();
        let result = runner.run_node(plan_request("plan the work"), &telemetry);

        let NodeRunResult::PlanAccepted(plan) = result else {
            panic!("expected PlanAccepted");
        };
        assert_eq!(plan.children.len(), 2, "must produce two work nodes");
        assert_eq!(plan.children[0].id, NodeId("alpha".to_string()));
        assert_eq!(
            plan.children[0].objective,
            "do alpha\n\nTarget files: alpha.txt"
        );
        assert!(plan.children[0].dependencies.is_empty());
        assert_eq!(plan.children[1].id, NodeId("beta".to_string()));
        assert_eq!(
            plan.children[1].objective,
            "do beta\n\nTarget files: beta.txt"
        );
        assert_eq!(
            plan.children[1].dependencies,
            vec![NodeId("alpha".to_string())]
        );

        let records = telemetry.into_records();
        let parsed = records.iter().find(|r| {
            matches!(
                r.event,
                crate::telemetry::TelemetryEvent::PlannerOutputParsed { .. }
            )
        });
        assert!(parsed.is_some(), "must emit PlannerOutputParsed telemetry");
        if let Some(r) = parsed {
            match &r.event {
                crate::telemetry::TelemetryEvent::PlannerOutputParsed {
                    task_count,
                    dependency_count,
                } => {
                    assert_eq!(*task_count, 2);
                    assert_eq!(*dependency_count, 1);
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn invalid_structured_plan_returns_failed() {
        // Parses as PlannerOutput but has a self-dependency — validation must fail loudly.
        // All three producer attempts return the same invalid plan, exhausting retries.
        let tasks_json = r#"{"tasks":[{"id":"x","objective":"do x","operation":"modify","targets":["x.txt"],"depends_on":["x"]}]}"#;
        let provider = ScriptedProvider::from_strs(&[
            tasks_json, // Producer attempt 1
            tasks_json, // Producer attempt 2 (retry)
            tasks_json, // Producer attempt 3 (retry)
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let telemetry = crate::telemetry::VecTelemetry::new();
        let result = runner.run_node(plan_request("plan the work"), &telemetry);

        let NodeRunResult::Failed(failure) = result else {
            panic!("expected Failed for invalid structured plan");
        };
        assert!(
            failure.message.contains("self-dependency"),
            "failure reason must describe the validation error; got: {}",
            failure.message
        );
        assert!(matches!(failure.recovery, RecoveryAction::Terminal { .. }));

        // Step 2: validation failure is now recorded as ParseFailed in the runner layer.
        let records = telemetry.into_records();
        let has_parse_failed = records.iter().any(|r| {
            matches!(
                r.event,
                crate::telemetry::TelemetryEvent::ParseFailed { .. }
            )
        });
        assert!(
            has_parse_failed,
            "must emit ParseFailed telemetry for planner validation failure"
        );
    }

    // --- role policy threading tests ---

    #[test]
    fn runtime_uses_project_adapter_role_policy() {
        use crate::project::DefaultProjectAdapter;
        use crate::project::ProjectAdapter;

        // Simulate the runtime: get policy from adapter, wire into runner.
        let adapter = DefaultProjectAdapter;
        let policy = adapter.role_policy();

        // A custom marker in a policy derived from the adapter should reach the prompt.
        let custom_policy = crate::roles::RolePolicy {
            worker_producer_system: "ADAPTER_MARKER_TEST".to_string(),
            ..policy
        };

        let provider = RecordingProvider::from_strs(&[
            r#"{"status":"accepted","content":"completed"}"#,
            r#"{"status":"accepted","content":"review ok"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
        ]);
        let runner =
            DeliberatingNodeRunner::new(&provider, &provider).with_role_policy(custom_policy);
        runner.run_node(work_request("test policy wiring"), &NoopTelemetry);

        let prompts = provider.recorded_prompts();
        assert!(!prompts.is_empty(), "provider must have received prompts");
        assert!(
            prompts[0].contains("ADAPTER_MARKER_TEST"),
            "adapter role policy must reach the provider prompt; got:\n{}",
            prompts[0]
        );
    }

    // --- model-tier routing tests ---

    #[test]
    fn cheap_tier_uses_cheap_provider() {
        // Strong has no responses; calling it would panic. Proves routing is correct.
        let cheap = ScriptedProvider::from_strs(&[
            r#"{"status":"accepted","content":"task completed"}"#,
            r#"{"status":"accepted","content":"review ok"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
        ]);
        let strong = ScriptedProvider::from_strs(&[]);
        let runner = DeliberatingNodeRunner::new(&cheap, &strong);
        let result = runner.run_node(work_request("cheap tier test"), &NoopTelemetry);
        assert!(
            matches!(result, NodeRunResult::WorkAccepted(_)),
            "cheap tier must route to cheap provider and succeed"
        );
    }

    #[test]
    fn strong_tier_uses_strong_provider() {
        // Cheap has no responses; calling it would panic. Proves routing is correct.
        let cheap = ScriptedProvider::from_strs(&[]);
        let strong = ScriptedProvider::from_strs(&[
            r#"{"status":"accepted","content":"task completed"}"#,
            r#"{"status":"accepted","content":"review ok"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(&cheap, &strong);
        let result = runner.run_node(strong_work_request("strong tier test"), &NoopTelemetry);
        assert!(
            matches!(result, NodeRunResult::WorkAccepted(_)),
            "strong tier must route to strong provider and succeed"
        );
    }

    #[test]
    fn strong_tier_uses_strong_token_budget() {
        // Cheap has no responses — if it were called the test would panic.
        let cheap = CapturingProvider::from_strs(&[]);
        let strong = CapturingProvider::from_strs(&[
            r#"{"status":"accepted","content":"task completed"}"#,
            r#"{"status":"accepted","content":"review ok"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(&cheap, &strong)
            .with_cheap_max_tokens(512)
            .with_strong_max_tokens(2048);
        runner.run_node(strong_work_request("token budget test"), &NoopTelemetry);

        assert_eq!(
            strong.captured_max_tokens(),
            Some(2048),
            "strong tier must use strong_max_tokens"
        );
        assert_eq!(
            cheap.captured_max_tokens(),
            None,
            "cheap provider must not be called for a strong-tier request"
        );
    }

    // ── read-file enforcement regression test ─────────────────────────────────

    #[test]
    fn referee_reads_file_and_rejects_default_content_causes_node_failure() {
        // Regression for: Referee accepted even though main.py still contained the
        // default initialized program instead of the required haiku.
        //
        // The Referee must call read_file and inspect file contents before deciding.
        // When the file contents do not satisfy the objective the Referee must reject.
        // Two rounds of rejection (max_revisions = 1) exhaust the revision budget
        // and the node must fail — WorkAccepted must never be returned.
        let temp = TempDir::new("referee-default-content");
        let view = make_artifact_view(&temp, "main.py", r#"print("Hello from forge-lang-init!")"#);

        // Round 1: Producer claims done, Critic reads and accepts, Referee reads
        // main.py, sees default content, and rejects.
        // Round 2: same sequence; budget is now exhausted → node fails.
        let provider = ScriptedProvider::from_strs(&[
            // Round 1
            r#"{"status":"accepted","content":"I wrote the haiku"}"#,
            r#"{"tool":"read_file","path":"main.py"}"#,
            r#"{"status":"accepted","content":"file is present"}"#,
            r#"{"tool":"read_file","path":"main.py"}"#,
            r#"{"status":"rejected","reason":"main.py still has default init content, not a haiku"}"#,
            // Round 2
            r#"{"status":"accepted","content":"I wrote the haiku"}"#,
            r#"{"tool":"read_file","path":"main.py"}"#,
            r#"{"status":"accepted","content":"file is present"}"#,
            r#"{"tool":"read_file","path":"main.py"}"#,
            r#"{"status":"rejected","reason":"main.py still has default init content, not a haiku"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let request = NodeRunRequest {
            kind: NodeKind::Work,
            objective: "Write a haiku about Python state machines in main.py".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
            artifact_view: Some(view),
        };
        let result = runner.run_node(request, &NoopTelemetry);

        assert!(
            matches!(result, NodeRunResult::Failed(_)),
            "node must fail when Referee rejects incorrect file contents"
        );
    }

    #[test]
    fn producer_read_file_does_not_satisfy_critic_read_requirement() {
        // The read_file_executed flag is scoped per role invocation.
        // Even if the Producer successfully read a file, the Critic's own
        // invocation starts fresh with read_file_executed = false.
        // A Critic that never reads must fail the enforcement regardless of
        // what the Producer did.
        let temp = TempDir::new("producer-read-no-critic");
        let view = make_artifact_view(&temp, "hello.txt", "hello world\n");

        // Producer: reads hello.txt (success), then accepts.
        // Critic: accepts three times without reading, exhausting protocol retries.
        // The deliberation must fail — not succeed because Producer already read.
        let provider = ScriptedProvider::from_strs(&[
            r#"{"tool":"read_file","path":"hello.txt"}"#,
            r#"{"status":"accepted","content":"I read the file and it looks correct"}"#,
            // Critic attempt 1: enforcement fires, must-read retry issued
            r#"{"status":"accepted","content":"looks good to me here ok"}"#,
            // Critic attempt 2: enforcement fires again
            r#"{"status":"accepted","content":"still looks good to me now"}"#,
            // Critic attempt 3: enforcement fires, protocol_attempt > MAX_PROTOCOL_RETRIES → fail
            r#"{"status":"accepted","content":"I accept this work done now"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let request = NodeRunRequest {
            kind: NodeKind::Work,
            objective: "write the work".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
            artifact_view: Some(view),
        };
        let result = runner.run_node(request, &NoopTelemetry);

        assert!(
            matches!(result, NodeRunResult::Failed(_)),
            "node must fail when Critic never reads, even though Producer did"
        );
    }

    // ── no-recreate validation recovery regression ────────────────────────────

    #[test]
    fn planner_no_recreate_violation_sends_revision_feedback_and_retries() {
        // Regression: planner first outputs a task for '.gitignore' (existing project
        // file not mentioned in the objective). Validation rejects it and sends structured
        // feedback. Planner revises to only include the main.py task. Run continues to
        // PlanAccepted — the run must NOT terminate with a terminal failure.
        let temp = TempDir::new("no-recreate-retry");
        let view = make_artifact_view(&temp, ".gitignore", "*.pyc\n__pycache__/\n");

        // First planner response: includes .gitignore task (violates no-recreate).
        let bad_plan = r#"{"tasks":[{"id":"task-1","objective":"Create .gitignore file for the project.","operation":"modify","targets":[".gitignore"],"depends_on":[]},{"id":"task-2","objective":"Write main.py with the haiku.","operation":"modify","targets":["main.py"],"depends_on":[]}]}"#;
        // Second planner response (after revision feedback): only the main.py task.
        let good_plan = r#"{"tasks":[{"id":"task-1","objective":"Write main.py with the haiku.","operation":"modify","targets":["main.py"],"depends_on":[]}]}"#;

        let provider = ScriptedProvider::from_strs(&[
            bad_plan,  // Plan+Producer attempt 1 — fails no-recreate, handler retries
            good_plan, // Plan+Producer attempt 2 (with feedback) — passes
            r#"{"status":"accepted","content":"plan looks good"}"#, // Plan+Critic
            r#"{"status":"accepted","content":"plan approved"}"#, // Plan+Referee
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let request = NodeRunRequest {
            kind: NodeKind::Plan,
            // Objective does not name a specific file so the fast path does not apply
            // and the LLM planner is called with the scripted responses.
            objective: "Write a haiku about Python state machines.".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
            artifact_view: Some(view),
        };
        let result = runner.run_node(request, &NoopTelemetry);

        let NodeRunResult::PlanAccepted(plan) = result else {
            panic!("expected PlanAccepted after planner revision");
        };
        assert_eq!(
            plan.children.len(),
            1,
            "revised plan must contain only the main.py task"
        );
        assert_eq!(
            plan.children[0].objective, "Write main.py with the haiku.\n\nTarget files: main.py",
            "task objective must match the revised plan"
        );
    }

    #[test]
    fn planner_missing_test_target_sends_revision_feedback_and_retries() {
        let bad_plan = r#"{"tasks":[{"id":"task-1","objective":"Modify main.py to return the haiku.","operation":"modify","targets":["main.py"],"depends_on":[]}]}"#;
        let good_plan = r#"{"tasks":[{"id":"task-1","objective":"Modify main.py to return the haiku.","operation":"modify","targets":["main.py"],"depends_on":[]},{"id":"task-2","objective":"Add tests for the main.py haiku behavior.","operation":"modify","targets":["test_main.py"],"depends_on":["task-1"]}]}"#;

        let provider = ScriptedProvider::from_strs(&[
            bad_plan,  // Plan+Producer attempt 1 — fails test-target validation
            good_plan, // Plan+Producer attempt 2 (with feedback) — passes
            r#"{"status":"accepted","content":"plan looks good"}"#, // Plan+Critic
            r#"{"status":"accepted","content":"plan approved"}"#, // Plan+Referee
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider).with_requires_tests(true);
        let request = NodeRunRequest {
            kind: NodeKind::Plan,
            // Objective does not name a specific file so the fast path does not apply
            // and the LLM planner is called with the scripted responses.
            objective: "Print a short haiku about state machines.".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
            artifact_view: None,
        };
        let result = runner.run_node(request, &NoopTelemetry);

        let NodeRunResult::PlanAccepted(plan) = result else {
            panic!("expected PlanAccepted after planner adds test target");
        };
        assert_eq!(plan.children.len(), 2);
        assert!(
            plan.children
                .iter()
                .any(|child| child.objective.contains("Target files: test_main.py")),
            "revised plan must include a test_main.py target"
        );
    }

    #[test]
    fn planner_explicit_target_violation_sends_revision_feedback_and_retries() {
        let bad_plan = r#"{"tasks":[{"id":"task-1","objective":"Modify main.py to return the haiku.","operation":"modify","targets":["main.py"],"depends_on":[]},{"id":"task-2","objective":"Modify project configuration.","operation":"modify","targets":["pyproject.toml"],"depends_on":[]},{"id":"task-3","objective":"Add tests for the main.py haiku behavior.","operation":"create","targets":["test_main.py"],"depends_on":["task-1"]}]}"#;
        let good_plan = r#"{"tasks":[{"id":"task-1","objective":"Modify main.py to return the haiku.","operation":"modify","targets":["main.py"],"depends_on":[]},{"id":"task-2","objective":"Add tests for the main.py haiku behavior.","operation":"create","targets":["test_main.py"],"depends_on":["task-1"]}]}"#;

        let provider = RecordingProvider::from_strs(&[
            bad_plan,  // Plan+Producer attempt 1 — fails explicit-target validation
            good_plan, // Plan+Producer attempt 2 (with feedback) — passes
            r#"{"status":"accepted","content":"plan looks good"}"#, // Plan+Critic
            r#"{"status":"accepted","content":"plan approved"}"#, // Plan+Referee
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider).with_requires_tests(true);
        let request = NodeRunRequest {
            kind: NodeKind::Plan,
            // Two explicit files → fast path does not apply (needs exactly one source file);
            // ExplicitTargetViolation still fires when the planner targets pyproject.toml.
            objective: "Modify main.py and utils.py to print a short haiku.".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
            artifact_view: None,
        };
        let result = runner.run_node(request, &NoopTelemetry);

        let NodeRunResult::PlanAccepted(plan) = result else {
            panic!("expected PlanAccepted after planner removes pyproject.toml");
        };
        assert_eq!(plan.children.len(), 2);
        assert!(
            plan.children
                .iter()
                .any(|child| child.objective.contains("Target files: main.py")),
            "revised plan must include main.py"
        );
        assert!(
            plan.children
                .iter()
                .any(|child| child.objective.contains("Target files: test_main.py")),
            "revised plan must include test_main.py"
        );
        assert!(
            plan.children
                .iter()
                .all(|child| !child.objective.contains("pyproject.toml")),
            "revised plan must reject pyproject.toml"
        );

        let prompts = provider.recorded_prompts();
        assert!(
            prompts.iter().any(|prompt| prompt.contains(
                "The objective explicitly targets main.py, utils.py. \
                 Remove all non-test targets except main.py, utils.py."
            )),
            "retry prompt must contain exact explicit-target feedback; got: {prompts:#?}"
        );
    }

    #[test]
    fn planner_no_recreate_violation_exhausts_retries_returns_failed() {
        // When the planner keeps including tasks for existing files after MAX retries,
        // the run must fail — not silently accept the bad plan.
        let temp = TempDir::new("no-recreate-exhausted");
        let view = make_artifact_view(&temp, ".gitignore", "*.pyc\n");

        // All three producer responses include the .gitignore task.
        let bad_plan = r#"{"tasks":[{"id":"task-1","objective":"Create .gitignore file.","operation":"modify","targets":[".gitignore"],"depends_on":[]}]}"#;

        let provider = ScriptedProvider::from_strs(&[
            bad_plan, // attempt 1
            bad_plan, // attempt 2 (retry 1)
            bad_plan, // attempt 3 (retry 2 — MAX_NO_RECREATE_RETRIES)
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let request = NodeRunRequest {
            kind: NodeKind::Plan,
            // Objective does not name a specific file so the fast path does not apply
            // and the LLM planner is called until retries are exhausted.
            objective: "Write a haiku about Python state machines.".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
            artifact_view: Some(view),
        };
        let result = runner.run_node(request, &NoopTelemetry);

        assert!(
            matches!(result, NodeRunResult::Failed(_)),
            "plan must fail when no-recreate retries are exhausted"
        );
        if let NodeRunResult::Failed(failure) = result {
            assert!(
                failure.message.contains(".gitignore") || failure.message.contains("no-recreate"),
                "failure reason must mention the offending file or constraint; got: {}",
                failure.message
            );
        }
    }

    // ── fast plan integration tests ──────────────────────────────────────────────

    #[test]
    fn fast_plan_bypasses_provider_and_emits_telemetry() {
        // When the objective names exactly one source file the fast path must
        // produce PlanAccepted without calling the provider at all.
        // ScriptedProvider panics when exhausted — no responses means any call is a bug.
        let provider = ScriptedProvider::from_strs(&[]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let telemetry = crate::telemetry::VecTelemetry::new();
        let result = runner.run_node(
            NodeRunRequest {
                kind: NodeKind::Plan,
                objective: "Create a simple Python program in main.py that prints a greeting."
                    .to_string(),
                model_tier: ModelTier::Cheap,
                attempt: 0,
                artifact_view: None,
            },
            &telemetry,
        );

        let NodeRunResult::PlanAccepted(plan) = result else {
            panic!("expected PlanAccepted from fast path; got another variant");
        };
        assert_eq!(plan.children.len(), 1, "no tests required → one work task");
        assert!(
            plan.children[0].objective.contains("Target files: main.py"),
            "fast plan work task must target main.py"
        );

        let records = telemetry.into_records();
        let fast_plan_event = records.iter().find(|r| {
            matches!(
                r.event,
                crate::telemetry::TelemetryEvent::FastPlanUsed { .. }
            )
        });
        assert!(
            fast_plan_event.is_some(),
            "fast path must emit FastPlanUsed telemetry"
        );
        if let Some(r) = fast_plan_event {
            match &r.event {
                crate::telemetry::TelemetryEvent::FastPlanUsed { task_count } => {
                    assert_eq!(*task_count, 1);
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn fast_plan_with_tests_required_adds_test_task_and_emits_telemetry() {
        let provider = ScriptedProvider::from_strs(&[]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider).with_requires_tests(true);
        let telemetry = crate::telemetry::VecTelemetry::new();
        let result = runner.run_node(
            NodeRunRequest {
                kind: NodeKind::Plan,
                objective: "Create a simple Python program in main.py that prints a greeting."
                    .to_string(),
                model_tier: ModelTier::Cheap,
                attempt: 0,
                artifact_view: None,
            },
            &telemetry,
        );

        let NodeRunResult::PlanAccepted(plan) = result else {
            panic!("expected PlanAccepted from fast path");
        };
        assert_eq!(plan.children.len(), 2, "tests required → two work tasks");
        assert!(
            plan.children
                .iter()
                .any(|c| c.objective.contains("Target files: main.py")),
            "must have a main.py work task"
        );
        assert!(
            plan.children
                .iter()
                .any(|c| c.objective.contains("Target files: test_main.py")),
            "must have a test_main.py task"
        );

        let records = telemetry.into_records();
        let fast_plan_event = records.iter().find(|r| {
            matches!(
                r.event,
                crate::telemetry::TelemetryEvent::FastPlanUsed { .. }
            )
        });
        assert!(
            fast_plan_event.is_some(),
            "fast path with tests must emit FastPlanUsed telemetry"
        );
        if let Some(r) = fast_plan_event {
            match &r.event {
                crate::telemetry::TelemetryEvent::FastPlanUsed { task_count } => {
                    assert_eq!(*task_count, 2);
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn multi_file_objective_falls_through_to_llm_planner() {
        // Two explicit source files → fast path returns None → LLM planner called.
        // Targets must be within the allowed set (main.py, utils.py) to pass validation.
        let tasks_json = r#"{"tasks":[{"id":"w","objective":"add logging","operation":"modify","targets":["main.py"],"depends_on":[]}]}"#;
        let provider = ScriptedProvider::from_strs(&[
            tasks_json,
            r#"{"status":"accepted","content":"plan looks good"}"#,
            r#"{"status":"accepted","content":"plan approved"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let result = runner.run_node(
            NodeRunRequest {
                kind: NodeKind::Plan,
                objective: "Modify main.py and utils.py to add logging.".to_string(),
                model_tier: ModelTier::Cheap,
                attempt: 0,
                artifact_view: None,
            },
            &NoopTelemetry,
        );
        assert!(
            matches!(result, NodeRunResult::PlanAccepted(_)),
            "multi-file objective must fall through to the LLM planner and succeed"
        );
    }
}
