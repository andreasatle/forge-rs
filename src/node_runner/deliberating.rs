//! NodeRunner backed by DeliberationMachine.

use crate::artifacts::{ArtifactUpdate, ArtifactView, FileChange};
use crate::engine::{Machine, Transition, run_machine_with_telemetry};
use crate::machines::deliberation::{
    DeliberationEffect, DeliberationEvent, DeliberationMachine, DeliberationRequest,
    DeliberationState, DeliberationTerminalOutput, ProviderBackedDeliberationHandler,
};
use crate::machines::scheduler::{
    ModelTier, NodeFailure, NodeId, NodeKind, NodeRequest, PlanOutput, RecoveryAction, WorkOutput,
};
use crate::providers::ProviderClient;
use crate::telemetry::TelemetrySink;

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
        match request.model_tier {
            ModelTier::Cheap => run_with_provider(
                &self.cheap_provider,
                request,
                self.cheap_max_tokens,
                telemetry,
            ),
            ModelTier::Strong => run_with_provider(
                &self.strong_provider,
                request,
                self.strong_max_tokens,
                telemetry,
            ),
        }
    }
}

fn run_with_provider<P: ProviderClient>(
    provider: &P,
    request: NodeRunRequest,
    max_tokens: u32,
    telemetry: &dyn TelemetrySink,
) -> NodeRunResult {
    let objective = enrich_objective(&request);
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
        ),
        telemetry,
    };
    let (output, machine) = run_machine_with_telemetry(machine, initial_state, telemetry);
    let tool_artifact_update = machine.take_artifact_update();
    map_output(output, request.kind, tool_artifact_update)
}

/// Returns the objective string, optionally prefixed with artifact file context.
fn enrich_objective(request: &NodeRunRequest) -> String {
    let Some(view) = &request.artifact_view else {
        return request.objective.clone();
    };
    let context = build_artifact_context(view);
    if context.is_empty() {
        return request.objective.clone();
    }
    format!("{context}\n\nObjective: {}", request.objective)
}

/// Builds a short context string from a read-only artifact view.
///
/// Lists all files and, if `README.md` is present, includes its content.
/// Returns an empty string when the view has no files or when git fails.
fn build_artifact_context(view: &ArtifactView) -> String {
    let files = match view.list_files() {
        Ok(f) if !f.is_empty() => f,
        _ => return String::new(),
    };
    let mut parts = Vec::new();
    let listing: Vec<String> = files.iter().map(|p| format!("  {}", p.display())).collect();
    parts.push(format!("Files:\n{}", listing.join("\n")));
    if let Ok(readme) = view.read_file("README.md") {
        parts.push(format!("README.md:\n{readme}"));
    }
    parts.join("\n\n")
}

fn map_output(
    output: DeliberationTerminalOutput,
    kind: NodeKind,
    tool_artifact_update: Option<ArtifactUpdate>,
) -> NodeRunResult {
    match output {
        DeliberationTerminalOutput::Complete(out) => match kind {
            NodeKind::Plan => NodeRunResult::PlanAccepted(PlanOutput {
                children: vec![NodeRequest {
                    id: NodeId("child-1".to_string()),
                    kind: NodeKind::Work,
                    objective: out.content,
                    dependencies: vec![],
                }],
            }),
            NodeKind::Work => NodeRunResult::WorkAccepted(NodeRunWorkResult {
                work: WorkOutput {
                    summary: out.content.clone(),
                },
                // When tool calls produced file changes, use those changes as
                // the artifact update. Otherwise fall back to writing the
                // producer content to output.txt.
                artifact_update: if let Some(update) = tool_artifact_update {
                    Some(update)
                } else {
                    Some(ArtifactUpdate {
                        changes: vec![FileChange::Write {
                            path: "output.txt".to_owned(),
                            content: out.content,
                        }],
                    })
                },
            }),
        },
        DeliberationTerminalOutput::Failed { reason } => NodeRunResult::Failed(NodeFailure {
            reason: reason.clone(),
            recovery: RecoveryAction::Terminal {
                message: format!("deliberation failed: {reason}"),
            },
        }),
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
    use crate::artifacts::ArtifactView;
    use crate::machines::scheduler::ModelTier;
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
        let provider = ScriptedProvider::from_strs(&[
            r#"{"status":"accepted","content":"draft"}"#,
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
        assert_eq!(plan.children[0].objective, "draft");
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
                .reason
                .contains("provider error (Retryable): connection refused")
        );
        assert!(matches!(failure.recovery, RecoveryAction::Terminal { .. }));
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
                .reason
                .contains("provider error (Retryable): connection refused")
        );
        let RecoveryAction::Terminal { message } = failure.recovery else {
            panic!("expected terminal recovery");
        };
        assert!(
            message.contains("deliberation failed: provider error (Retryable): connection refused")
        );
    }

    #[test]
    fn deliberating_runner_revision_uses_latest_producer_content() {
        let provider = ScriptedProvider::from_strs(&[
            r#"{"status":"accepted","content":"draft v1"}"#,
            r#"{"status":"accepted","content":"review"}"#,
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
        assert!(matches!(failure.recovery, RecoveryAction::Terminal { .. }));
    }

    // --- new tests ---

    #[test]
    fn deliberating_work_result_contains_artifact_update() {
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
        let update = work_result
            .artifact_update
            .expect("DeliberatingNodeRunner must produce an ArtifactUpdate for work nodes");
        assert_eq!(update.changes.len(), 1);
        match &update.changes[0] {
            FileChange::Write { path, content } => {
                assert_eq!(path, "output.txt");
                assert_eq!(content, "output content");
            }
            other => panic!("expected Write change, got {other:?}"),
        }
    }

    #[test]
    fn artifact_view_context_is_visible_to_deliberation_prompt() {
        let temp = TempDir::new("prompt-context");
        let view = make_artifact_view(&temp, "hello.txt", "world\n");

        let provider = RecordingProvider::from_strs(&[
            r#"{"status":"accepted","content":"draft"}"#,
            r#"{"status":"accepted","content":"review ok"}"#,
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
            r#"{"status":"accepted","content":"result"}"#,
            r#"{"status":"accepted","content":"review"}"#,
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
        // Critic and Referee return simple accepted.
        let provider = ScriptedProvider::from_strs(&[
            r#"{"tool":"write_file","path":"result.txt","content":"done"}"#,
            r#"{"status":"accepted","content":"I wrote result.txt"}"#,
            r#"{"status":"accepted","content":"looks good"}"#,
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

    // --- model-tier routing tests ---

    #[test]
    fn cheap_tier_uses_cheap_provider() {
        // Strong has no responses; calling it would panic. Proves routing is correct.
        let cheap = ScriptedProvider::from_strs(&[
            r#"{"status":"accepted","content":"result"}"#,
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
            r#"{"status":"accepted","content":"result"}"#,
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
            r#"{"status":"accepted","content":"result"}"#,
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
}
