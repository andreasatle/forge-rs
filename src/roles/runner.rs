//! Provider-backed role execution.
//!
//! `RoleRunner` owns one complete role round-trip: render prompt, call provider,
//! parse JSON, retry on protocol failure. The deliberation layer above sees only
//! `RoleRequest` in and `RoleResult` out.

use serde::Deserialize;

use crate::artifacts::{ArtifactError, ArtifactRead, ArtifactUpdate};
#[cfg(doc)]
use crate::artifacts::{ArtifactView, StagedArtifactView};
use crate::machines::deliberation::event::RoleResult;
use crate::machines::deliberation::state::{DeliberationRole, RevisionFeedback};
use crate::machines::scheduler::{FailureKind, NodeKind};
use crate::node_runner::planner::{try_parse_planner_response, validate_planner_output};
use crate::providers::{ProviderClient, ProviderErrorKind, ProviderRequest, StructuredOutput};
use crate::roles::policy::RolePolicy;
use crate::services::extract_json_object;
use crate::telemetry::{TelemetryEvent, TelemetryRecord, TelemetrySink};
use crate::tools::{
    FileToolExecutor, FileToolPolicy, FileToolRequest, FileToolResponse, parse_tool_request,
};

/// A read-only view of the artifact made available to role tool loops.
pub struct RoleToolContext {
    /// The artifact snapshot the role may read from and accumulate changes against.
    /// May be a plain [`ArtifactView`] (for Producer) or a [`StagedArtifactView`]
    /// that includes the Producer's pending writes (for Critic and Referee).
    pub artifact_view: Box<dyn ArtifactRead>,
}

impl std::fmt::Debug for RoleToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoleToolContext")
            .field("artifact_view", &"<dyn ArtifactRead>")
            .finish()
    }
}

/// All inputs needed to execute one role invocation.
#[derive(Debug)]
pub struct RoleRequest {
    /// The role to invoke.
    pub role: DeliberationRole,
    /// The objective to pass to the role.
    pub objective: String,
    /// Structured target files this role should use for target-aware tooling.
    pub target_files: Vec<String>,
    /// Content produced by the Producer. `None` when dispatching Producer.
    pub producer_content: Option<String>,
    /// Content produced by the Critic. `None` when dispatching Producer or Critic.
    pub critic_content: Option<String>,
    /// Accumulated Referee rejection feedback. Empty on the first pass.
    pub feedback: Vec<RevisionFeedback>,
    /// Whether the role is acting on a planner or worker node.
    /// Selects the matching node-kind-specific system prompt from the policy.
    pub node_kind: NodeKind,
    /// File tool context. When `Some`, the role may issue tool requests before
    /// returning a final result. When `None`, tool request JSON is still detected
    /// but produces an error observation rather than a real tool execution.
    pub tool_context: Option<RoleToolContext>,
}

/// The output of a completed role invocation.
pub struct RoleRunOutput {
    /// The semantic result returned by the role.
    pub result: RoleResult,
    /// Pending file changes accumulated by tool calls during the role loop.
    ///
    /// `None` when no tool calls were made or when no artifact view was
    /// provided. Non-empty changes are returned even on protocol failure so
    /// that callers can decide what to do with partial work.
    pub artifact_update: Option<ArtifactUpdate>,
}

/// Execute one role invocation end-to-end and return its result.
pub trait RoleRunner {
    /// Run the role described by `request` and record protocol telemetry.
    fn run_role(&self, request: RoleRequest, telemetry: &dyn TelemetrySink) -> RoleRunOutput;
}

/// Provider-backed implementation of [`RoleRunner`].
///
/// Wraps a [`ProviderClient`] and owns all role-layer logic: prompt rendering,
/// provider invocation, JSON extraction/parsing, and protocol retry.
pub struct ProviderRoleRunner<P> {
    provider: P,
    max_tokens: u32,
    policy: RolePolicy,
}

impl<P> ProviderRoleRunner<P> {
    /// Wrap a provider in a new runner using the default token budget and default policy.
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            max_tokens: MAX_RESPONSE_TOKENS,
            policy: RolePolicy::default(),
        }
    }

    /// Wrap a provider in a new runner with an explicit token budget and default policy.
    pub fn new_with_max_tokens(provider: P, max_tokens: u32) -> Self {
        Self {
            provider,
            max_tokens,
            policy: RolePolicy::default(),
        }
    }

    /// Wrap a provider in a new runner with an explicit policy and default token budget.
    pub fn new_with_policy(provider: P, policy: RolePolicy) -> Self {
        Self {
            provider,
            max_tokens: MAX_RESPONSE_TOKENS,
            policy,
        }
    }

    /// Replace the current policy, returning the updated runner.
    pub fn with_policy(mut self, policy: RolePolicy) -> Self {
        self.policy = policy;
        self
    }
}

/// Maximum number of tokens to request per provider call.
const MAX_RESPONSE_TOKENS: u32 = 1024;

/// Maximum number of additional provider calls after the initial response has
/// failed protocol parsing or validation.
const MAX_PROTOCOL_RETRIES: usize = 2;

/// Maximum number of tool calls within a single role invocation before the
/// loop is declared a protocol failure.
const MAX_TOOL_STEPS: usize = 5;

/// Maximum number of tool observations allowed for Critic and Referee before
/// decision pressure activates and further tool calls are prohibited.
const MAX_READ_ONLY_TOOL_STEPS: usize = 2;

/// Maximum text bytes included per target-state entry in the prompt view.
///
/// This is intentionally separate from tool access. Tools retain their own
/// read/write limits; the target-state view is just prompt context.
const MAX_TARGET_STATE_TEXT_BYTES: usize = 16 * 1024;

/// Minimum number of non-whitespace characters required in accepted content or
/// a rejection reason. Responses shorter than this are degenerate (e.g. `{`,
/// `ok`) and are treated as protocol failures so the retry loop can recover.
const MIN_CONTENT_LENGTH: usize = 8;

#[derive(Debug, Clone, Eq, PartialEq)]
struct TargetStateView {
    entries: Vec<TargetStateEntry>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct TargetStateEntry {
    target: String,
    exists: bool,
    representation: TargetStateRepresentation,
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum TargetStateRepresentation {
    Text { content: String },
    Absent,
    TooLarge { bytes: usize, limit: usize },
    Error { summary: String },
}

/// Encapsulates all mutable protocol counters and pressure flags for one role invocation.
///
/// The runner initialises one instance per `run_role` call and delegates every
/// state mutation to methods on this struct instead of manipulating the variables
/// directly. This is a pure data-extraction refactor to prepare for a future
/// explicit protocol state machine.
struct ProtocolState {
    /// Current protocol attempt number (1-based; starts at 1).
    protocol_attempt: usize,
    /// Total tool steps executed so far in this invocation.
    tool_steps: usize,
    /// Read-only tool steps (Critic/Referee only; drives decision pressure).
    read_only_tool_steps: usize,
    /// True once decision pressure has been activated.
    decision_pressure_active: bool,
    /// True when any pressure mode (completion, decision, or repeated-observation
    /// coercion) is active; gates further tool calls and selects prompt style.
    final_response_only: bool,
    /// `(tool, observation)` fingerprints seen in this invocation.
    seen_tool_fingerprints: std::collections::HashSet<String>,
    /// True once repeated-observation coercion has been activated.
    repeated_observation_coercion_active: bool,
    /// True once a `read_file` call has returned `FileContents`.
    read_file_executed: bool,
    /// Number of `read_file` calls attempted via the executor (failed or successful).
    read_file_attempted: usize,
    /// Whether this invocation is Work+Producer (eligible for completion pressure).
    is_work_producer: bool,
    /// Whether this invocation is Critic or Referee (eligible for decision pressure).
    is_read_only_reviewer: bool,
    /// Whether this invocation must read at least one file before accepting.
    ///
    /// True for Work-node Critic and Referee when a tool executor is available.
    requires_read_enforcement: bool,
}

impl ProtocolState {
    fn new(
        is_work_producer: bool,
        is_read_only_reviewer: bool,
        requires_read_enforcement: bool,
    ) -> Self {
        Self {
            protocol_attempt: 1,
            tool_steps: 0,
            read_only_tool_steps: 0,
            decision_pressure_active: false,
            final_response_only: false,
            seen_tool_fingerprints: std::collections::HashSet::new(),
            repeated_observation_coercion_active: false,
            read_file_executed: false,
            read_file_attempted: 0,
            is_work_producer,
            is_read_only_reviewer,
            requires_read_enforcement,
        }
    }

    /// Returns `true` when tool calls are currently permitted (not in any pressure mode).
    fn allow_tool_call(&self) -> bool {
        !self.final_response_only
    }

    /// Returns `true` when another provider call (protocol retry) is permitted.
    fn allow_model_call(&self) -> bool {
        self.protocol_attempt <= MAX_PROTOCOL_RETRIES
    }

    /// Records the start of a tool call; increments the absolute tool-step counter.
    fn record_tool_call(&mut self) {
        self.tool_steps += 1;
    }

    /// Records a `read_file` attempt via the executor.
    ///
    /// Must only be called from inside the `Some(exec)` branch so that the count
    /// matches the original invariant: `read_file_attempted` only counts
    /// executor-backed calls.
    fn record_read_file_attempt(&mut self) {
        self.read_file_attempted += 1;
    }

    /// Returns `true` when the absolute tool-step limit has been reached.
    fn tool_loop_limit_reached(&self) -> bool {
        self.tool_steps > MAX_TOOL_STEPS
    }

    /// Records the result of a completed tool call and updates all pressure flags.
    ///
    /// `fingerprint` is `"{tool_name}\n{observation}"`. `mutation_recorded` is
    /// `true` when the tool produced `FileToolResponse::UpdateRecorded`.
    /// `read_file_succeeded` is `true` when a `read_file` returned `FileContents`.
    ///
    /// Also resets `protocol_attempt` to 1 since a successful tool step restarts
    /// the protocol-retry counter for the next model call.
    fn record_tool_result(
        &mut self,
        fingerprint: String,
        mutation_recorded: bool,
        read_file_succeeded: bool,
    ) {
        if read_file_succeeded {
            self.read_file_executed = true;
        }
        if self.is_work_producer && mutation_recorded {
            self.enter_completion_pressure();
        }
        if self.is_read_only_reviewer {
            self.read_only_tool_steps += 1;
            if self.read_only_tool_steps >= MAX_READ_ONLY_TOOL_STEPS {
                self.enter_decision_pressure();
            }
        }
        if !self.seen_tool_fingerprints.insert(fingerprint) && !self.final_response_only {
            self.repeated_observation_coercion_active = true;
            self.final_response_only = true;
        }
        self.protocol_attempt = 1;
    }

    /// Activates completion pressure: Work+Producer has recorded a successful mutation.
    fn enter_completion_pressure(&mut self) {
        self.final_response_only = true;
    }

    /// Activates decision pressure: Critic/Referee has exhausted the read-only tool budget.
    fn enter_decision_pressure(&mut self) {
        self.final_response_only = true;
        self.decision_pressure_active = true;
    }

    /// Advances the protocol-attempt counter after a parse or validation failure.
    fn record_protocol_failure(&mut self) {
        self.protocol_attempt += 1;
    }

    fn is_decision_pressure_active(&self) -> bool {
        self.decision_pressure_active
    }

    fn is_repeated_observation_coercion_active(&self) -> bool {
        self.repeated_observation_coercion_active
    }

    fn read_file_attempted(&self) -> usize {
        self.read_file_attempted
    }

    fn current_attempt(&self) -> usize {
        self.protocol_attempt
    }

    /// Returns `true` when reviewer read enforcement is active and the reviewer
    /// has not yet successfully read any file. The caller must additionally check
    /// that the parsed result is `Accepted` before treating this as a violation.
    fn reviewer_accepted_without_reading(&self) -> bool {
        self.requires_read_enforcement && !self.read_file_executed
    }

    /// Returns `true` when a reviewer-must-read violation should fail immediately
    /// rather than issuing a retry prompt.
    ///
    /// This fires when tools are blocked (no retry would help) or the protocol
    /// retry budget is already exhausted.
    fn reviewer_accept_must_fail_immediately(&self) -> bool {
        !self.allow_tool_call() || !self.allow_model_call()
    }
}

impl<P: ProviderClient> RoleRunner for ProviderRoleRunner<P> {
    fn run_role(&self, request: RoleRequest, telemetry: &dyn TelemetrySink) -> RoleRunOutput {
        let subsource = role_subsource(&request.role);
        let has_tools = request.tool_context.is_some();

        let policy = file_tool_policy_for_request(&request);
        let target_state = request
            .tool_context
            .as_ref()
            .and_then(|ctx| build_target_state_view(&*ctx.artifact_view, &request.target_files));

        let system = match (&request.node_kind, &request.role) {
            (NodeKind::Plan, DeliberationRole::Producer) => &self.policy.planner_producer_system,
            (NodeKind::Plan, DeliberationRole::Critic) => &self.policy.planner_critic_system,
            (NodeKind::Plan, DeliberationRole::Referee) => &self.policy.planner_referee_system,
            (NodeKind::Work, DeliberationRole::Producer) => &self.policy.worker_producer_system,
            (NodeKind::Work, DeliberationRole::Critic) => &self.policy.worker_critic_system,
            (NodeKind::Work, DeliberationRole::Referee) => &self.policy.worker_referee_system,
        };

        let core_prompt = render_role_prompt(
            system,
            &request.role,
            &render_objective_for_prompt(&request.objective, &request.target_files),
            request.producer_content.as_deref(),
            request.critic_content.as_deref(),
            &request.feedback,
            target_state.as_ref(),
        );
        let base_prompt = if has_tools {
            format!("{core_prompt}\n\n{}", render_tool_section(&policy))
        } else {
            core_prompt.clone()
        };

        let mut executor: Option<FileToolExecutor> = request
            .tool_context
            .map(|ctx| FileToolExecutor::with_policy(ctx.artifact_view, policy));

        let mut current_prompt = base_prompt.clone();
        // Accumulated observation sections, tracked separately so the prompt can be
        // rebuilt without the tool section when completion pressure is active.
        let mut observation_suffix = String::new();

        // Completion pressure applies only to Work+Producer after a successful mutation.
        let is_work_producer = request.node_kind == NodeKind::Work
            && matches!(request.role, DeliberationRole::Producer);
        // Decision pressure applies to Critic and Referee after bounded read-only tool use.
        let is_read_only_reviewer = matches!(
            request.role,
            DeliberationRole::Critic | DeliberationRole::Referee
        );
        // Work-node Critic and Referee must call read_file at least once before
        // accepting. list_files alone is insufficient — the model must inspect
        // actual file contents. This enforcement only applies when tools are
        // available; plan-node reviewers judge structure, not file contents.
        let requires_read_enforcement =
            request.node_kind == NodeKind::Work && is_read_only_reviewer && has_tools;

        let mut proto = ProtocolState::new(
            is_work_producer,
            is_read_only_reviewer,
            requires_read_enforcement,
        );

        loop {
            telemetry.record(TelemetryRecord::new_with_subsource(
                "RoleMachine",
                subsource,
                TelemetryEvent::RolePromptRendered {
                    prompt: current_prompt.clone(),
                    attempt_count: proto.current_attempt(),
                },
            ));

            let response = match self.provider.call(ProviderRequest {
                prompt: current_prompt.clone(),
                max_tokens: self.max_tokens,
                output_schema: Some(StructuredOutput::Json),
            }) {
                Ok(r) => r,
                Err(err) => {
                    let kind = match err.kind {
                        ProviderErrorKind::Retryable | ProviderErrorKind::Timeout => {
                            FailureKind::ProviderFailure
                        }
                        ProviderErrorKind::Terminal => FailureKind::ProviderTerminalFailure,
                    };
                    return RoleRunOutput {
                        result: RoleResult::Failed {
                            kind,
                            reason: format!("provider error ({:?}): {}", err.kind, err.message),
                        },
                        artifact_update: extract_update(&mut executor),
                    };
                }
            };

            telemetry.record(TelemetryRecord::new_with_subsource(
                "RoleMachine",
                subsource,
                TelemetryEvent::ProviderResponseReceived {
                    raw_response: response.content.clone(),
                    attempt_count: proto.current_attempt(),
                },
            ));

            // Check for a tool request before trying to parse as a role result.
            let trimmed = strip_code_fence(response.content.trim());
            if let Some(json_str) = extract_json_object(trimmed)
                && let Ok(tool_req) = parse_tool_request(json_str)
            {
                // In completion-pressure mode, all tool requests are protocol violations.
                if !proto.allow_tool_call() {
                    let parse_error = if proto.is_repeated_observation_coercion_active() {
                        "protocol error: repeated identical tool observations; model continued calling tools after coercion".to_string()
                    } else {
                        "tool request received while no tools are available".to_string()
                    };
                    telemetry.record(TelemetryRecord::new_with_subsource(
                        "RoleMachine",
                        subsource,
                        TelemetryEvent::ParseFailed {
                            raw_response: response.content.clone(),
                            parse_error: parse_error.clone(),
                            attempt_count: proto.current_attempt(),
                        },
                    ));
                    if proto.is_repeated_observation_coercion_active() || !proto.allow_model_call()
                    {
                        return RoleRunOutput {
                            result: RoleResult::Failed {
                                kind: FailureKind::ProtocolFailure,
                                reason: parse_error,
                            },
                            artifact_update: extract_update(&mut executor),
                        };
                    }
                    proto.record_protocol_failure();
                    telemetry.record(TelemetryRecord::new_with_subsource(
                        "RoleMachine",
                        subsource,
                        TelemetryEvent::ProtocolRetry {
                            parse_error: parse_error.clone(),
                            attempt_count: proto.current_attempt(),
                        },
                    ));
                    let violation_note = if proto.is_decision_pressure_active() {
                        render_decision_pressure_violation_note()
                    } else {
                        render_completion_pressure_violation_note()
                    };
                    observation_suffix = format!("{observation_suffix}\n\n{violation_note}");
                    current_prompt = format!("{core_prompt}{observation_suffix}");
                    continue;
                }

                proto.record_tool_call();
                let tool_name = tool_name_of(&tool_req);
                telemetry.record(TelemetryRecord::new_with_subsource(
                    "RoleMachine",
                    subsource,
                    TelemetryEvent::ToolRequested {
                        tool: tool_name.clone(),
                    },
                ));

                if proto.tool_loop_limit_reached() {
                    telemetry.record(TelemetryRecord::new_with_subsource(
                        "RoleMachine",
                        subsource,
                        TelemetryEvent::ToolLoopLimitReached,
                    ));
                    return RoleRunOutput {
                        result: RoleResult::Failed {
                            kind: FailureKind::ToolFailure,
                            reason: "tool loop limit reached".to_string(),
                        },
                        artifact_update: extract_update(&mut executor),
                    };
                }

                let is_read_file_req = matches!(&tool_req, FileToolRequest::ReadFile { .. });
                let mut read_file_succeeded = false;
                let (observation, mutation_recorded) = match &mut executor {
                    Some(exec) => {
                        if is_read_file_req {
                            proto.record_read_file_attempt();
                        }
                        let max_obs = exec.policy().max_observation_bytes;
                        let response = exec.execute(tool_req);
                        if is_read_file_req
                            && matches!(response, FileToolResponse::FileContents { .. })
                        {
                            read_file_succeeded = true;
                        }
                        let recorded = matches!(response, FileToolResponse::UpdateRecorded { .. });
                        (format_tool_observation(&response, max_obs), recorded)
                    }
                    None => (
                        r#"{"ok":false,"error":"no file tools available"}"#.to_string(),
                        false,
                    ),
                };

                // Compute fingerprint before tool_name is moved into the telemetry record.
                let fingerprint = format!("{tool_name}\n{observation}");

                telemetry.record(TelemetryRecord::new_with_subsource(
                    "RoleMachine",
                    subsource,
                    TelemetryEvent::ToolReturned {
                        tool: tool_name,
                        result: observation.clone(),
                    },
                ));

                proto.record_tool_result(fingerprint, mutation_recorded, read_file_succeeded);

                let obs_section = if !proto.allow_tool_call() {
                    if proto.is_repeated_observation_coercion_active() {
                        format_repeated_observation_coercion_section(&observation)
                    } else if proto.is_decision_pressure_active() {
                        format_decision_pressure_section(&observation)
                    } else {
                        format_completion_pressure_section(&observation)
                    }
                } else {
                    format_observation_section(&observation, mutation_recorded)
                };

                observation_suffix = format!("{observation_suffix}\n\n{obs_section}");
                current_prompt = if !proto.allow_tool_call() {
                    format!("{core_prompt}{observation_suffix}")
                } else {
                    format!("{current_prompt}\n\n{obs_section}")
                };
                continue;
            }

            // Not a tool request — select parser based on role and node kind.
            if request.node_kind == NodeKind::Plan
                && matches!(request.role, DeliberationRole::Producer)
            {
                // Direct PlannerOutput path: no status/content wrapper.
                match try_parse_planner_response(&response.content) {
                    Ok(planner_out) => match validate_planner_output(&planner_out) {
                        Ok(()) => {
                            telemetry.record(TelemetryRecord::new_with_subsource(
                                "RoleMachine",
                                subsource,
                                TelemetryEvent::ParseSucceeded {
                                    attempt_count: proto.current_attempt(),
                                },
                            ));
                            let canonical = serde_json::to_string(&planner_out)
                                .expect("validated PlannerOutput must serialize");
                            return RoleRunOutput {
                                result: RoleResult::Accepted { content: canonical },
                                artifact_update: extract_update(&mut executor),
                            };
                        }
                        Err(e) => {
                            let err = format!("planner output validation failed: {e}");
                            telemetry.record(TelemetryRecord::new_with_subsource(
                                "RoleMachine",
                                subsource,
                                TelemetryEvent::ParseFailed {
                                    raw_response: response.content.clone(),
                                    parse_error: err.clone(),
                                    attempt_count: proto.current_attempt(),
                                },
                            ));
                            if !proto.allow_model_call() {
                                return RoleRunOutput {
                                    result: RoleResult::Failed {
                                        kind: FailureKind::PlannerValidationFailure,
                                        reason: err,
                                    },
                                    artifact_update: extract_update(&mut executor),
                                };
                            }
                            proto.record_protocol_failure();
                            telemetry.record(TelemetryRecord::new_with_subsource(
                                "RoleMachine",
                                subsource,
                                TelemetryEvent::ProtocolRetry {
                                    parse_error: err.clone(),
                                    attempt_count: proto.current_attempt(),
                                },
                            ));
                            current_prompt = render_planner_retry_prompt(&base_prompt, &err);
                        }
                    },
                    Err(parse_error) => {
                        telemetry.record(TelemetryRecord::new_with_subsource(
                            "RoleMachine",
                            subsource,
                            TelemetryEvent::ParseFailed {
                                raw_response: response.content.clone(),
                                parse_error: parse_error.clone(),
                                attempt_count: proto.current_attempt(),
                            },
                        ));
                        if !proto.allow_model_call() {
                            return RoleRunOutput {
                                result: RoleResult::Failed {
                                    kind: FailureKind::ProtocolFailure,
                                    reason: parse_error,
                                },
                                artifact_update: extract_update(&mut executor),
                            };
                        }
                        proto.record_protocol_failure();
                        telemetry.record(TelemetryRecord::new_with_subsource(
                            "RoleMachine",
                            subsource,
                            TelemetryEvent::ProtocolRetry {
                                parse_error: parse_error.clone(),
                                attempt_count: proto.current_attempt(),
                            },
                        ));
                        current_prompt = render_planner_retry_prompt(&base_prompt, &parse_error);
                    }
                }
            } else {
                // Standard role result path for Worker, Critic, and Referee.
                match try_parse_role_response(&response.content) {
                    Ok(result) => {
                        // Enforce that Work-node reviewers read at least one file before
                        // accepting. list_files alone is not sufficient — the reviewer
                        // must inspect actual file contents to verify the objective.
                        if proto.reviewer_accepted_without_reading()
                            && matches!(result, RoleResult::Accepted { .. })
                        {
                            let attempt_note = if proto.read_file_attempted() == 0 {
                                "no read_file was attempted".to_string()
                            } else {
                                format!(
                                    "{} read_file attempt(s) were made but all failed",
                                    proto.read_file_attempted()
                                )
                            };
                            let parse_error = format!(
                                "{subsource} accepted without successfully reading any file \
                                ({attempt_note}); use read_file with a valid relative path \
                                to inspect the work before deciding"
                            );
                            telemetry.record(TelemetryRecord::new_with_subsource(
                                "RoleMachine",
                                subsource,
                                TelemetryEvent::ParseFailed {
                                    raw_response: response.content.clone(),
                                    parse_error: parse_error.clone(),
                                    attempt_count: proto.current_attempt(),
                                },
                            ));
                            // When tools are blocked (decision or completion pressure active)
                            // there is no point issuing a must-read retry prompt — the model
                            // cannot call tools and would only generate further protocol errors.
                            // Fail directly with a clear reason in that case.
                            if proto.reviewer_accept_must_fail_immediately() {
                                return RoleRunOutput {
                                    result: RoleResult::Failed {
                                        kind: FailureKind::ProtocolFailure,
                                        reason: parse_error,
                                    },
                                    artifact_update: extract_update(&mut executor),
                                };
                            }
                            proto.record_protocol_failure();
                            telemetry.record(TelemetryRecord::new_with_subsource(
                                "RoleMachine",
                                subsource,
                                TelemetryEvent::ProtocolRetry {
                                    parse_error: parse_error.clone(),
                                    attempt_count: proto.current_attempt(),
                                },
                            ));
                            current_prompt =
                                render_reviewer_must_read_prompt(&base_prompt, &parse_error);
                            continue;
                        }
                        telemetry.record(TelemetryRecord::new_with_subsource(
                            "RoleMachine",
                            subsource,
                            TelemetryEvent::ParseSucceeded {
                                attempt_count: proto.current_attempt(),
                            },
                        ));
                        return RoleRunOutput {
                            result,
                            artifact_update: extract_update(&mut executor),
                        };
                    }
                    Err(parse_error) => {
                        // Replace the generic serde error with a more informative
                        // message when the model echoed a tool-section placeholder
                        // (e.g. $TARGET_FILE / $FILE_CONTENT) verbatim.
                        let placeholder_echo = detect_placeholder_tool_echo(trimmed);
                        let effective_error: String = match &placeholder_echo {
                            Some(pe) => format!("placeholder tool request: {pe}"),
                            None => parse_error,
                        };
                        telemetry.record(TelemetryRecord::new_with_subsource(
                            "RoleMachine",
                            subsource,
                            TelemetryEvent::ParseFailed {
                                raw_response: response.content.clone(),
                                parse_error: effective_error.clone(),
                                attempt_count: proto.current_attempt(),
                            },
                        ));
                        if !proto.allow_model_call() {
                            // A parse failure after completion pressure means the
                            // write was already recorded but the model could not
                            // confirm it. Label the reason so the node-runner
                            // classifier can treat it as Retry rather than Terminal.
                            let terminal_reason = if !proto.allow_tool_call() {
                                format!("protocol failure after write: {effective_error}")
                            } else {
                                effective_error
                            };
                            return RoleRunOutput {
                                result: RoleResult::Failed {
                                    kind: FailureKind::ProtocolFailure,
                                    reason: terminal_reason,
                                },
                                artifact_update: extract_update(&mut executor),
                            };
                        }
                        proto.record_protocol_failure();
                        telemetry.record(TelemetryRecord::new_with_subsource(
                            "RoleMachine",
                            subsource,
                            TelemetryEvent::ProtocolRetry {
                                parse_error: effective_error.clone(),
                                attempt_count: proto.current_attempt(),
                            },
                        ));
                        current_prompt = if !proto.allow_tool_call() {
                            render_completion_pressure_retry_prompt(
                                &core_prompt,
                                &observation_suffix,
                                &effective_error,
                            )
                        } else {
                            render_retry_prompt(&base_prompt, &effective_error)
                        };
                    }
                }
            }
        }
    }
}

/// Consumes the executor and returns its pending update, or `None` when empty.
fn extract_update(executor: &mut Option<FileToolExecutor>) -> Option<ArtifactUpdate> {
    executor.take().and_then(|e| {
        let update = e.into_update();
        if update.changes.is_empty() {
            None
        } else {
            Some(update)
        }
    })
}

/// Returns a short name for a tool request (used in telemetry).
fn tool_name_of(req: &crate::tools::FileToolRequest) -> String {
    use crate::tools::FileToolRequest;
    match req {
        FileToolRequest::ListFiles => "list_files",
        FileToolRequest::ReadFile { .. } => "read_file",
        FileToolRequest::WriteFile { .. } => "write_file",
        FileToolRequest::ReplaceText { .. } => "replace_text",
        FileToolRequest::DeleteFile { .. } => "delete_file",
    }
    .to_string()
}

/// Returns a [`FileToolPolicy`] appropriate for `role`.
///
/// Producer may read and write. Critic and Referee are read-only.
fn file_tool_policy_for_role(role: &DeliberationRole) -> FileToolPolicy {
    match role {
        DeliberationRole::Producer => FileToolPolicy::default(),
        DeliberationRole::Critic | DeliberationRole::Referee => FileToolPolicy {
            allow_writes: false,
            ..FileToolPolicy::default()
        },
    }
}

fn file_tool_policy_for_request(request: &RoleRequest) -> FileToolPolicy {
    let mut policy = file_tool_policy_for_role(&request.role);
    if request.node_kind == NodeKind::Work && !request.target_files.is_empty() {
        policy.allowed_paths = Some(request.target_files.clone());
    }
    policy
}

fn build_target_state_view(
    artifact_view: &dyn ArtifactRead,
    target_files: &[String],
) -> Option<TargetStateView> {
    if target_files.is_empty() {
        return None;
    }

    let listed_paths = artifact_view.list_files().ok();
    let entries = target_files
        .iter()
        .map(|target| build_target_state_entry(artifact_view, listed_paths.as_deref(), target))
        .collect();
    Some(TargetStateView { entries })
}

fn build_target_state_entry(
    artifact_view: &dyn ArtifactRead,
    listed_paths: Option<&[std::path::PathBuf]>,
    target: &str,
) -> TargetStateEntry {
    match artifact_view.read_file(target) {
        Ok(content) if content.len() <= MAX_TARGET_STATE_TEXT_BYTES => TargetStateEntry {
            target: target.to_string(),
            exists: true,
            representation: TargetStateRepresentation::Text { content },
        },
        Ok(content) => TargetStateEntry {
            target: target.to_string(),
            exists: true,
            representation: TargetStateRepresentation::TooLarge {
                bytes: content.len(),
                limit: MAX_TARGET_STATE_TEXT_BYTES,
            },
        },
        Err(ArtifactError::FileNotFound) => TargetStateEntry {
            target: target.to_string(),
            exists: false,
            representation: TargetStateRepresentation::Absent,
        },
        Err(error) => {
            let exists = listed_paths
                .map(|paths| {
                    paths
                        .iter()
                        .any(|path| path.to_string_lossy().as_ref() == target)
                })
                .unwrap_or(false);
            TargetStateEntry {
                target: target.to_string(),
                exists,
                representation: TargetStateRepresentation::Error {
                    summary: safe_target_state_error(error),
                },
            }
        }
    }
}

fn safe_target_state_error(error: ArtifactError) -> String {
    let message = error.to_string();
    if message.contains("utf-8") || message.contains("utf8") {
        "binary or non-UTF-8 file cannot be represented as text".to_string()
    } else {
        message
    }
}

fn render_objective_for_prompt(objective: &str, target_files: &[String]) -> String {
    if target_files.is_empty() {
        objective.to_string()
    } else {
        format!("{objective}\n\nTarget files: {}", target_files.join(", "))
    }
}

/// Serialises a [`FileToolResponse`] as a compact JSON observation string,
/// capped to `max_observation_bytes`.
fn format_tool_observation(response: &FileToolResponse, max_observation_bytes: usize) -> String {
    let json = match response {
        FileToolResponse::FileList { paths } => {
            let files: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
            serde_json::to_string(&serde_json::json!({"ok": true, "files": files}))
                .unwrap_or_else(|_| r#"{"ok":true}"#.to_string())
        }
        FileToolResponse::FileContents { content, .. } => {
            serde_json::to_string(&serde_json::json!({"ok": true, "content": content}))
                .unwrap_or_else(|_| r#"{"ok":true}"#.to_string())
        }
        FileToolResponse::UpdateRecorded { description } => {
            serde_json::to_string(&serde_json::json!({"ok": true, "description": description}))
                .unwrap_or_else(|_| r#"{"ok":true}"#.to_string())
        }
        FileToolResponse::Failed { reason } => {
            serde_json::to_string(&serde_json::json!({"ok": false, "error": reason}))
                .unwrap_or_else(|_| r#"{"ok":false}"#.to_string())
        }
    };
    cap_observation(json, max_observation_bytes)
}

/// Truncates `s` to at most `max_bytes` bytes, appending a marker so the
/// model knows the observation was cut.
fn cap_observation(s: String, max_bytes: usize) -> String {
    const MARKER: &str = "\n[observation truncated]";
    if s.len() <= max_bytes {
        return s;
    }
    let keep = max_bytes.saturating_sub(MARKER.len());
    let mut boundary = keep.min(s.len());
    while boundary > 0 && !s.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}{MARKER}", &s[..boundary])
}

/// Wraps a tool observation with protocol guidance for the model.
///
/// `mutation_recorded` is true when the preceding tool was a successful write,
/// replace, or delete — i.e., `FileToolResponse::UpdateRecorded`. In that case
/// a stronger hint is appended telling the model to return final accepted JSON
/// without further reads unless strictly necessary.
fn format_observation_section(observation: &str, mutation_recorded: bool) -> String {
    let base = format!(
        "Framework tool observation:\n{observation}\n\
         This is framework output, not a valid response format.\n\
         If the requested change is complete, return exactly:\n\
         {{\"status\":\"accepted\",\"content\":\"Summarize the completed change.\"}}\n\
         Only call another tool if more information is strictly required."
    );
    if mutation_recorded {
        format!(
            "{base}\n\
             The change was recorded successfully.\n\
             If no further file inspection is strictly required, return final accepted JSON now."
        )
    } else {
        base
    }
}

/// Returns the tool-availability section appended to a prompt when tools are enabled.
///
/// Write tools (`write_file`, `replace_text`, `delete_file`) are only included
/// when `policy.allow_writes` is true, keeping the advertised schema consistent
/// with what the executor will actually permit.
fn render_tool_section(policy: &FileToolPolicy) -> String {
    let example_path = policy
        .allowed_paths
        .as_ref()
        .and_then(|paths| paths.first())
        .map(String::as_str)
        .unwrap_or("path/to/file.txt");
    let mut s = String::from(
        "Available file tools:\n\
         {\"tool\":\"list_files\"}\n",
    );
    if let Some(allowed) = &policy.allowed_paths {
        s.push_str(&format!("Allowed target files: {}\n", allowed.join(", ")));
    }
    s.push_str(&format!(
        "{{\"tool\":\"read_file\",\"path\":\"{example_path}\"}}\n"
    ));
    if policy.allow_writes {
        s.push_str(&format!(
            "{{\"tool\":\"write_file\",\"path\":\"{example_path}\",\"content\":\"$FILE_CONTENT\"}}\n\
             {{\"tool\":\"replace_text\",\"path\":\"{example_path}\",\"old\":\"$EXACT_EXISTING_TEXT\",\"new\":\"$REPLACEMENT_TEXT\"}}\n\
             {{\"tool\":\"delete_file\",\"path\":\"{example_path}\"}}\n"
        ));
    }
    s.push_str(
        "You may return either:\n\
         1. a tool request JSON, or\n\
         2. a final role result JSON.\n\
         Return exactly one JSON object.\n\
         Do not copy example values. Replace them with actual file paths and content.",
    );
    s
}

fn role_subsource(role: &DeliberationRole) -> &'static str {
    match role {
        DeliberationRole::Producer => "Producer",
        DeliberationRole::Critic => "Critic",
        DeliberationRole::Referee => "Referee",
    }
}

fn render_retry_prompt(original_prompt: &str, parse_error: &str) -> String {
    format!(
        "{original_prompt}\n\n\
         Your previous response could not be parsed: {parse_error}\n\
         Return only one JSON object matching one of these schemas:\n\
         {{\"status\":\"accepted\",\"content\":\"$RESPONSE_SUMMARY\"}}\n\
         {{\"status\":\"rejected\",\"reason\":\"$REASON_FOR_REJECTION\"}}\n\
         Do not copy example values. Replace them with task-specific content."
    )
}

fn render_reviewer_must_read_prompt(original_prompt: &str, parse_error: &str) -> String {
    format!(
        "{original_prompt}\n\n\
         Your previous response tried to accept without reading any file: {parse_error}\n\
         You must use read_file to inspect the relevant file contents before deciding.\n\
         Read the specific file(s) the producer was expected to modify, then return your decision.\n\
         Return a tool request to read the relevant file(s)."
    )
}

fn render_planner_retry_prompt(original_prompt: &str, parse_error: &str) -> String {
    format!(
        "{original_prompt}\n\n\
         Your previous response could not be parsed: {parse_error}\n\
         Return only one JSON object matching this schema:\n\
         {{\"tasks\":[{{\"id\":\"task-id\",\"objective\":\"Task objective.\",\"depends_on\":[]}}]}}\n\
         Do not copy example values. Replace them with actual task IDs and objectives."
    )
}

/// Formats the observation section that signals completion-pressure mode.
///
/// Called instead of [`format_observation_section`] after the first successful
/// mutation on a Work+Producer node. Removes tool-calling as a valid next step
/// and directs the model to return its final role result.
fn format_completion_pressure_section(observation: &str) -> String {
    format!(
        "Framework tool observation:\n{observation}\n\
         This is framework output, not a valid response format.\n\
         The requested change has already been recorded.\n\
         Do not call any more tools.\n\
         Return exactly one of:\n\
         {{\"status\":\"accepted\",\"content\":\"$RESPONSE_SUMMARY\"}}\n\
         {{\"status\":\"rejected\",\"reason\":\"$REASON_FOR_REJECTION\"}}\n\
         Do not copy example values. Replace them with task-specific content."
    )
}

/// Formats the observation section that signals decision-pressure mode for
/// read-only reviewer roles (Critic, Referee).
///
/// Called instead of [`format_observation_section`] once a Critic or Referee
/// has exhausted its `MAX_READ_ONLY_TOOL_STEPS` budget. Removes tool-calling
/// as a valid next step and directs the model to return its final role result.
fn format_decision_pressure_section(observation: &str) -> String {
    format!(
        "Framework tool observation:\n{observation}\n\
         This is framework output, not a valid response format.\n\
         You have gathered sufficient evidence to make a decision.\n\
         Do not call any more tools.\n\
         Return exactly one of:\n\
         {{\"status\":\"accepted\",\"content\":\"$RESPONSE_SUMMARY\"}}\n\
         {{\"status\":\"rejected\",\"reason\":\"$REASON_FOR_REJECTION\"}}\n\
         Do not copy example values. Replace them with task-specific content."
    )
}

/// Returns the note appended to the prompt when the model sends a tool request
/// while completion pressure is active.
fn render_completion_pressure_violation_note() -> String {
    "Tools are no longer available.\n\
     The requested change has already been recorded.\n\
     Return a final role response:\n\
     {\"status\":\"accepted\",\"content\":\"$RESPONSE_SUMMARY\"}\n\
     {\"status\":\"rejected\",\"reason\":\"$REASON_FOR_REJECTION\"}\n\
     Do not copy example values. Replace them with task-specific content."
        .to_string()
}

/// Returns the note appended to the prompt when a Critic or Referee sends a
/// tool request while decision pressure is active.
fn render_decision_pressure_violation_note() -> String {
    "Tools are no longer available.\n\
     You have gathered sufficient evidence to make a decision.\n\
     Return a final role response:\n\
     {\"status\":\"accepted\",\"content\":\"$RESPONSE_SUMMARY\"}\n\
     {\"status\":\"rejected\",\"reason\":\"$REASON_FOR_REJECTION\"}\n\
     Do not copy example values. Replace them with task-specific content."
        .to_string()
}

/// Formats the observation section that signals repeated-observation coercion.
///
/// Called when the same (tool, observation) pair is seen for the second time
/// within a single role invocation. After this section is appended, any further
/// tool request is an immediate protocol error.
fn format_repeated_observation_coercion_section(observation: &str) -> String {
    format!(
        "Framework tool observation:\n{observation}\n\
         You have already inspected this information. Do not call more tools.\n\
         Return accepted or rejected JSON now.\n\
         Return exactly one of:\n\
         {{\"status\":\"accepted\",\"content\":\"$RESPONSE_SUMMARY\"}}\n\
         {{\"status\":\"rejected\",\"reason\":\"$REASON_FOR_REJECTION\"}}\n\
         Do not copy example values. Replace them with task-specific content."
    )
}

/// Returns `Some(err_msg)` when `s` contains a JSON object that `parse_tool_request`
/// recognises as a tool-request form but rejects because it contains placeholder
/// values (e.g. `$TARGET_FILE`, `$FILE_CONTENT`).
///
/// When `Some` is returned the caller should use the returned message as the
/// effective parse error, replacing the misleading serde "missing field `status`"
/// error that would otherwise be shown to the model on retry.
fn detect_placeholder_tool_echo(s: &str) -> Option<String> {
    let json = extract_json_object(s)?;
    match parse_tool_request(json) {
        Err(e) if e.contains("placeholder") => Some(e),
        _ => None,
    }
}

/// Builds a protocol-retry prompt for use in completion-pressure mode.
///
/// Uses `core` (the role prompt without the tool section) plus `observation_suffix`
/// so the model is not shown stale tool definitions after a mutation has been recorded.
fn render_completion_pressure_retry_prompt(
    core: &str,
    observation_suffix: &str,
    parse_error: &str,
) -> String {
    format!(
        "{core}{observation_suffix}\n\n\
         Your previous response could not be parsed: {parse_error}\n\
         Tools are no longer available.\n\
         Return exactly one of:\n\
         {{\"status\":\"accepted\",\"content\":\"$RESPONSE_SUMMARY\"}}\n\
         {{\"status\":\"rejected\",\"reason\":\"$REASON_FOR_REJECTION\"}}\n\
         Do not copy example values. Replace them with task-specific content."
    )
}

/// Build a prompt for a single role invocation.
///
/// `system` is the role-specific instruction from [`RolePolicy`] and is
/// appended as the final paragraph. Callers are responsible for selecting
/// the correct policy field for the current role.
fn render_role_prompt(
    system: &str,
    role: &DeliberationRole,
    objective: &str,
    producer_content: Option<&str>,
    critic_content: Option<&str>,
    feedback: &[RevisionFeedback],
    target_state: Option<&TargetStateView>,
) -> String {
    let mut parts = Vec::new();
    parts.push(format!("Objective: {objective}"));
    parts.push(format!("Role: {role:?}"));
    if let Some(view) = target_state {
        parts.push(render_target_state_view(view));
    }
    if let Some(pc) = producer_content {
        parts.push(format!("Producer content: {pc}"));
    }
    if let Some(cc) = critic_content {
        parts.push(format!("Critic content: {cc}"));
    }
    if !feedback.is_empty() {
        let reasons: Vec<&str> = feedback.iter().map(|f| f.reason.as_str()).collect();
        parts.push(format!("Revision feedback: {}", reasons.join("; ")));
    }
    parts.push(system.to_string());
    parts.join("\n")
}

fn render_target_state_view(view: &TargetStateView) -> String {
    let mut lines = vec![
        "Target state view (built from structured target_files):".to_string(),
        "This view is prompt context only; file tools remain the source of operational access."
            .to_string(),
    ];
    for entry in &view.entries {
        lines.push(format!("- target: {}", entry.target));
        lines.push(format!("  exists: {}", entry.exists));
        match &entry.representation {
            TargetStateRepresentation::Text { content } => {
                lines.push(format!(
                    "  representation: file text ({} bytes)",
                    content.len()
                ));
                lines.push("  content:".to_string());
                lines.push(indent_target_state_content(content));
            }
            TargetStateRepresentation::Absent => {
                lines.push("  representation: absent".to_string());
            }
            TargetStateRepresentation::TooLarge { bytes, limit } => {
                lines.push(format!(
                    "  representation: too large to include safely ({bytes} bytes; limit {limit} bytes)"
                ));
            }
            TargetStateRepresentation::Error { summary } => {
                lines.push(format!("  representation: error: {summary}"));
            }
        }
    }
    lines.join("\n")
}

fn indent_target_state_content(content: &str) -> String {
    if content.is_empty() {
        return "    ".to_string();
    }
    content
        .lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Internal serde type for JSON role responses from the provider.
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum JsonRoleResponse {
    Accepted { content: String },
    Rejected { reason: String },
}

/// Returns `true` if `value` exactly matches a framework placeholder token.
///
/// Framework placeholders follow the convention `$[A-Z_]+`: a dollar sign
/// followed by one or more uppercase ASCII letters or underscores. Only exact
/// matches are rejected — strings that merely *contain* a dollar sign are not.
fn is_framework_placeholder(value: &str) -> bool {
    let s = value.trim();
    s.starts_with('$') && s.len() > 1 && s[1..].bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
}

fn try_parse_role_response(raw_response: &str) -> Result<RoleResult, String> {
    let text = strip_code_fence(raw_response.trim());
    // Require the response to start directly with a JSON object.
    // Chat-style preamble ("Here is my answer: {...}") rewards incorrect behavior.
    if !text.starts_with('{') {
        return Err(
            "role response must start with a JSON object; preamble text is not permitted"
                .to_string(),
        );
    }
    let json_str = match extract_json_object(text) {
        Some(s) => s,
        None => {
            return Err("no JSON object found in role response".to_string());
        }
    };
    let result = match serde_json::from_str::<JsonRoleResponse>(json_str) {
        Ok(JsonRoleResponse::Accepted { content }) => {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                return Err("accepted response has empty content".to_string());
            } else if trimmed == "..." {
                return Err("role response has placeholder accepted content".to_string());
            } else if is_framework_placeholder(&content) {
                return Err(format!(
                    "role response returned framework placeholder: {content}"
                ));
            } else if trimmed.len() < MIN_CONTENT_LENGTH {
                return Err(format!(
                    "accepted content is too short to be a meaningful summary ({} chars)",
                    trimmed.len()
                ));
            } else {
                RoleResult::Accepted { content }
            }
        }
        Ok(JsonRoleResponse::Rejected { reason }) => {
            let trimmed = reason.trim();
            if trimmed.is_empty() || trimmed == "..." {
                return Err("role response has placeholder reason".to_string());
            } else if is_framework_placeholder(&reason) {
                return Err(format!(
                    "role response returned framework placeholder: {reason}"
                ));
            } else if trimmed.len() < MIN_CONTENT_LENGTH {
                return Err(format!(
                    "rejection reason is too short to be meaningful ({} chars)",
                    trimmed.len()
                ));
            } else {
                RoleResult::Rejected { reason }
            }
        }
        Err(err) => return Err(format!("JSON parse error: {err}")),
    };
    Ok(result)
}

/// Strip a leading ` ```json ` or ` ``` ` fence and its matching closing ` ``` `.
fn strip_code_fence(s: &str) -> &str {
    let s = s.trim();
    let after_open = if let Some(rest) = s.strip_prefix("```json") {
        rest
    } else if let Some(rest) = s.strip_prefix("```") {
        rest
    } else {
        return s;
    };
    let after_newline = after_open.trim_start_matches('\r').trim_start_matches('\n');
    if let Some(body) = after_newline.strip_suffix("```") {
        body.trim()
    } else {
        after_newline.trim()
    }
}

#[cfg(test)]
fn parse_role_response(raw_response: &str) -> RoleResult {
    try_parse_role_response(raw_response).unwrap_or_else(|reason| RoleResult::Failed {
        kind: FailureKind::ProtocolFailure,
        reason,
    })
}

#[cfg(test)]
impl ProtocolState {
    fn read_file_executed(&self) -> bool {
        self.read_file_executed
    }
}

#[cfg(test)]
mod protocol_state_tests {
    use super::*;

    fn work_producer() -> ProtocolState {
        ProtocolState::new(true, false, false)
    }

    fn work_reviewer() -> ProtocolState {
        ProtocolState::new(false, true, true)
    }

    fn plain_producer() -> ProtocolState {
        ProtocolState::new(false, false, false)
    }

    #[test]
    fn tool_budget_exhaustion() {
        let mut proto = work_producer();
        for _ in 0..=MAX_TOOL_STEPS {
            proto.record_tool_call();
        }
        assert!(
            proto.tool_loop_limit_reached(),
            "tool_loop_limit_reached must be true after MAX_TOOL_STEPS+1 calls"
        );
        assert!(
            !proto.tool_loop_limit_reached() || {
                let mut p2 = work_producer();
                for _ in 0..MAX_TOOL_STEPS {
                    p2.record_tool_call();
                }
                !p2.tool_loop_limit_reached()
            },
            "tool_loop_limit_reached must be false before the limit is crossed"
        );
    }

    #[test]
    fn tool_budget_not_reached_before_limit() {
        let mut proto = work_producer();
        for _ in 0..MAX_TOOL_STEPS {
            proto.record_tool_call();
        }
        assert!(
            !proto.tool_loop_limit_reached(),
            "tool_loop_limit_reached must be false at exactly MAX_TOOL_STEPS calls"
        );
    }

    #[test]
    fn protocol_retry_budget_exhaustion() {
        let mut proto = plain_producer();
        assert!(proto.allow_model_call(), "model call allowed initially");
        for _ in 0..MAX_PROTOCOL_RETRIES {
            proto.record_protocol_failure();
        }
        assert!(
            !proto.allow_model_call(),
            "allow_model_call must be false after MAX_PROTOCOL_RETRIES failures"
        );
    }

    #[test]
    fn protocol_retry_budget_not_exhausted_before_limit() {
        let mut proto = plain_producer();
        for _ in 0..MAX_PROTOCOL_RETRIES - 1 {
            proto.record_protocol_failure();
        }
        assert!(
            proto.allow_model_call(),
            "allow_model_call must be true before MAX_PROTOCOL_RETRIES failures"
        );
    }

    #[test]
    fn completion_pressure_fires_after_write() {
        let mut proto = work_producer();
        assert!(proto.allow_tool_call(), "tools allowed initially");
        proto.record_tool_result("write_file\n{ok}".to_string(), true, false);
        assert!(
            !proto.allow_tool_call(),
            "tools must be blocked after a successful mutation (completion pressure)"
        );
    }

    #[test]
    fn completion_pressure_does_not_fire_for_reviewer() {
        // Reviewers cannot mutate, so completion pressure must not fire even if
        // mutation_recorded is somehow passed as true.
        let mut proto = work_reviewer();
        proto.record_tool_result("write_file\n{ok}".to_string(), true, false);
        // Decision pressure fires instead (read_only_tool_steps reaches 1, still < MAX).
        // But completion pressure specifically must not be set.
        // The easiest check: tool calls are blocked iff decision pressure fired.
        // With MAX_READ_ONLY_TOOL_STEPS = 2, one step is not yet enough for decision pressure.
        assert!(
            proto.allow_tool_call(),
            "reviewer must not enter completion pressure after one step; got blocked early"
        );
    }

    #[test]
    fn decision_pressure_fires_after_read_budget() {
        let mut proto = work_reviewer();
        for i in 0..MAX_READ_ONLY_TOOL_STEPS {
            proto.record_tool_result(format!("read_file\nobs{i}"), false, true);
        }
        assert!(
            !proto.allow_tool_call(),
            "tools must be blocked after MAX_READ_ONLY_TOOL_STEPS (decision pressure)"
        );
        assert!(
            proto.is_decision_pressure_active(),
            "decision_pressure_active must be set"
        );
    }

    #[test]
    fn decision_pressure_not_active_before_budget() {
        let mut proto = work_reviewer();
        for i in 0..MAX_READ_ONLY_TOOL_STEPS - 1 {
            proto.record_tool_result(format!("read_file\nobs{i}"), false, true);
        }
        assert!(
            proto.allow_tool_call(),
            "tools must still be allowed before the read budget is exhausted"
        );
        assert!(
            !proto.is_decision_pressure_active(),
            "decision pressure must not be active before the budget is exhausted"
        );
    }

    #[test]
    fn repeated_identical_observation_triggers_coercion() {
        let mut proto = plain_producer();
        proto.record_tool_result("list_files\n{files:[]}".to_string(), false, false);
        assert!(
            proto.allow_tool_call(),
            "tools allowed after first observation"
        );
        assert!(
            !proto.is_repeated_observation_coercion_active(),
            "coercion must not be active after unique observation"
        );
        // Second identical fingerprint triggers coercion.
        proto.record_tool_result("list_files\n{files:[]}".to_string(), false, false);
        assert!(
            !proto.allow_tool_call(),
            "tools must be blocked after repeated observation"
        );
        assert!(
            proto.is_repeated_observation_coercion_active(),
            "repeated_observation_coercion_active must be set"
        );
    }

    #[test]
    fn distinct_observations_do_not_trigger_coercion() {
        let mut proto = plain_producer();
        proto.record_tool_result("read_file\ncontent-a".to_string(), false, true);
        proto.record_tool_result("read_file\ncontent-b".to_string(), false, true);
        assert!(
            !proto.is_repeated_observation_coercion_active(),
            "distinct observations must not trigger coercion"
        );
    }

    #[test]
    fn failed_read_file_does_not_satisfy_evidence_requirement() {
        let mut proto = work_reviewer();
        // Simulate a failed read_file: record attempt but not success.
        proto.record_read_file_attempt();
        proto.record_tool_result(
            "read_file\n{ok:false,error:not found}".to_string(),
            false,
            false,
        );
        assert!(
            !proto.read_file_executed(),
            "read_file_executed must remain false when read_file failed"
        );
        assert!(
            proto.reviewer_accepted_without_reading(),
            "reviewer_accepted_without_reading must be true when no successful read occurred"
        );
    }

    #[test]
    fn successful_read_file_satisfies_evidence_requirement() {
        let mut proto = work_reviewer();
        proto.record_read_file_attempt();
        proto.record_tool_result(
            "read_file\n{ok:true,content:hello}".to_string(),
            false,
            true,
        );
        assert!(
            proto.read_file_executed(),
            "read_file_executed must be true after a successful read"
        );
        assert!(
            !proto.reviewer_accepted_without_reading(),
            "reviewer_accepted_without_reading must be false after a successful read"
        );
    }

    #[test]
    fn reviewer_accept_must_fail_immediately_when_tools_blocked() {
        let mut proto = work_reviewer();
        for i in 0..MAX_READ_ONLY_TOOL_STEPS {
            proto.record_tool_result(format!("read_file\nobs{i}"), false, false);
        }
        // Decision pressure active; tools blocked.
        assert!(
            proto.reviewer_accept_must_fail_immediately(),
            "must fail immediately when tools are blocked"
        );
    }

    #[test]
    fn reviewer_accept_must_fail_immediately_when_retries_exhausted() {
        let mut proto = work_reviewer();
        for _ in 0..MAX_PROTOCOL_RETRIES {
            proto.record_protocol_failure();
        }
        assert!(
            proto.reviewer_accept_must_fail_immediately(),
            "must fail immediately when protocol retry budget is exhausted"
        );
    }

    #[test]
    fn reviewer_accept_must_not_fail_immediately_when_healthy() {
        let proto = work_reviewer();
        assert!(
            !proto.reviewer_accept_must_fail_immediately(),
            "must not fail immediately on a fresh reviewer state"
        );
    }

    #[test]
    fn requires_read_enforcement_false_skips_reviewer_check() {
        // is_read_only_reviewer=true but requires_read_enforcement=false (plan-node reviewer).
        let proto = ProtocolState::new(false, true, false);
        assert!(
            !proto.reviewer_accepted_without_reading(),
            "reviewer_accepted_without_reading must be false when enforcement is disabled"
        );
    }

    #[test]
    fn protocol_attempt_resets_after_tool_result() {
        let mut proto = plain_producer();
        proto.record_protocol_failure();
        proto.record_protocol_failure();
        assert_eq!(proto.current_attempt(), 3);
        // A successful tool step resets the counter.
        proto.record_tool_result("list_files\n{files:[]}".to_string(), false, false);
        assert_eq!(
            proto.current_attempt(),
            1,
            "protocol_attempt must reset to 1 after a successful tool step"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::artifacts::{ArtifactView, FileChange};
    use crate::machines::scheduler::NodeKind;
    use crate::providers::types::{ProviderError, ProviderErrorKind, ProviderResponse};

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
    fn json_accepted_empty_content_fails() {
        let result = parse_role_response(r#"{"status":"accepted","content":""}"#);
        assert!(
            matches!(result, RoleResult::Failed { .. }),
            "empty content must produce Failed, got {result:?}"
        );
    }

    #[test]
    fn json_accepted_placeholder_content_fails_without_including_raw() {
        let result = parse_role_response(r#"{"status":"accepted","content":"..."}"#);
        let RoleResult::Failed { reason, .. } = result else {
            panic!("placeholder '...' content must produce Failed, got {result:?}");
        };
        assert!(
            reason.contains("placeholder"),
            "failure reason must mention 'placeholder'; got: {reason}"
        );
        assert!(!reason.contains("raw:"));
    }

    #[test]
    fn json_rejected_empty_reason_fails() {
        let result = parse_role_response(r#"{"status":"rejected","reason":""}"#);
        assert!(
            matches!(result, RoleResult::Failed { .. }),
            "empty reason must produce Failed, got {result:?}"
        );
    }

    #[test]
    fn json_rejected_placeholder_reason_fails_without_including_raw() {
        let result = parse_role_response(r#"{"status":"rejected","reason":"..."}"#);
        let RoleResult::Failed { reason, .. } = result else {
            panic!("placeholder '...' reason must produce Failed, got {result:?}");
        };
        assert!(
            reason.contains("placeholder"),
            "failure reason must mention 'placeholder'; got: {reason}"
        );
        assert!(!reason.contains("raw:"));
    }

    #[test]
    fn placeholder_summary_is_rejected() {
        let result = parse_role_response(r#"{"status":"accepted","content":"$RESPONSE_SUMMARY"}"#);
        let RoleResult::Failed { reason, .. } = result else {
            panic!("framework placeholder content must produce Failed, got {result:?}");
        };
        assert!(
            reason.contains("framework placeholder"),
            "failure reason must mention 'framework placeholder'; got: {reason}"
        );
    }

    #[test]
    fn placeholder_reason_is_rejected() {
        let result =
            parse_role_response(r#"{"status":"rejected","reason":"$REASON_FOR_REJECTION"}"#);
        let RoleResult::Failed { reason, .. } = result else {
            panic!("framework placeholder reason must produce Failed, got {result:?}");
        };
        assert!(
            reason.contains("framework placeholder"),
            "failure reason must mention 'framework placeholder'; got: {reason}"
        );
    }

    #[test]
    fn dollar_reason_placeholder_is_rejected() {
        // "$REASON" is exactly MIN_CONTENT_LENGTH chars so it slips past the
        // length guard; it must be caught by is_framework_placeholder.
        let result = parse_role_response(r#"{"status":"rejected","reason":"$REASON"}"#);
        let RoleResult::Failed { reason, .. } = result else {
            panic!(
                r#"placeholder {{"status":"rejected","reason":"$REASON"}} must produce Failed, got {result:?}"#
            );
        };
        assert!(
            reason.contains("framework placeholder"),
            "failure reason must mention 'framework placeholder'; got: {reason}"
        );
    }

    #[test]
    fn dollar_response_summary_placeholder_is_rejected() {
        let result = parse_role_response(r#"{"status":"accepted","content":"$RESPONSE_SUMMARY"}"#);
        let RoleResult::Failed { reason, .. } = result else {
            panic!("$RESPONSE_SUMMARY placeholder must produce Failed, got {result:?}");
        };
        assert!(
            reason.contains("framework placeholder"),
            "failure reason must mention 'framework placeholder'; got: {reason}"
        );
    }

    #[test]
    fn dollar_reason_for_rejection_placeholder_is_rejected() {
        let result =
            parse_role_response(r#"{"status":"rejected","reason":"$REASON_FOR_REJECTION"}"#);
        let RoleResult::Failed { reason, .. } = result else {
            panic!("$REASON_FOR_REJECTION placeholder must produce Failed, got {result:?}");
        };
        assert!(
            reason.contains("framework placeholder"),
            "failure reason must mention 'framework placeholder'; got: {reason}"
        );
    }

    // --- minimum-length guard tests ---

    #[test]
    fn single_brace_accepted_content_fails() {
        let result = parse_role_response(r#"{"status":"accepted","content":"{"}"#);
        let RoleResult::Failed { reason, .. } = result else {
            panic!("single-char content must produce Failed, got {result:?}");
        };
        assert!(
            reason.contains("too short"),
            "failure reason must mention 'too short'; got: {reason}"
        );
    }

    #[test]
    fn two_char_accepted_content_fails() {
        let result = parse_role_response(r#"{"status":"accepted","content":"ok"}"#);
        let RoleResult::Failed { reason, .. } = result else {
            panic!("two-char content must produce Failed, got {result:?}");
        };
        assert!(
            reason.contains("too short"),
            "failure reason must mention 'too short'; got: {reason}"
        );
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
    fn single_brace_rejection_reason_fails() {
        let result = parse_role_response(r#"{"status":"rejected","reason":"{"}"#);
        let RoleResult::Failed { reason, .. } = result else {
            panic!("single-char reason must produce Failed, got {result:?}");
        };
        assert!(
            reason.contains("too short"),
            "failure reason must mention 'too short'; got: {reason}"
        );
    }

    #[test]
    fn two_char_rejection_reason_fails() {
        let result = parse_role_response(r#"{"status":"rejected","reason":"ok"}"#);
        let RoleResult::Failed { reason, .. } = result else {
            panic!("two-char reason must produce Failed, got {result:?}");
        };
        assert!(
            reason.contains("too short"),
            "failure reason must mention 'too short'; got: {reason}"
        );
    }

    #[test]
    fn min_length_boundary_accepted_content_passes() {
        // Exactly MIN_CONTENT_LENGTH characters must be accepted.
        let content = "a".repeat(super::MIN_CONTENT_LENGTH);
        let input = format!(r#"{{"status":"accepted","content":"{content}"}}"#);
        let result = parse_role_response(&input);
        assert!(
            matches!(result, RoleResult::Accepted { .. }),
            "content at exactly MIN_CONTENT_LENGTH must be accepted, got {result:?}"
        );
    }

    #[test]
    fn min_length_boundary_rejection_reason_passes() {
        // Exactly MIN_CONTENT_LENGTH characters must be accepted.
        let reason = "a".repeat(super::MIN_CONTENT_LENGTH);
        let input = format!(r#"{{"status":"rejected","reason":"{reason}"}}"#);
        let result = parse_role_response(&input);
        assert!(
            matches!(result, RoleResult::Rejected { .. }),
            "reason at exactly MIN_CONTENT_LENGTH must be accepted, got {result:?}"
        );
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
        let result = parse_role_response(
            r#"{"status":"accepted","content":"<html><body>ok</body></html>"}"#,
        );
        assert!(
            matches!(result, RoleResult::Accepted { .. }),
            "HTML-like content must be accepted, got {result:?}"
        );
    }

    #[test]
    fn xml_like_content_is_allowed() {
        let result = parse_role_response(
            r#"{"status":"accepted","content":"<root><item>data</item></root>"}"#,
        );
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
        let input =
            "{\"status\":\"accepted\",\"content\":\"The task was completed successfully.\"}\n";
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
        let input = "\n  {\"status\":\"accepted\",\"content\":\"The task was completed successfully.\"}  \n";
        let result = parse_role_response(input);
        assert!(
            matches!(result, RoleResult::Accepted { .. }),
            "leading and trailing whitespace must not prevent parsing, got {result:?}"
        );
    }

    // --- ProviderRoleRunner tests ---

    #[test]
    fn provider_error_still_maps_to_failed() {
        let runner = ProviderRoleRunner::new(FailingProvider {
            kind: ProviderErrorKind::Retryable,
            message: "rate limited".to_string(),
        });
        let result = runner
            .run_role(
                RoleRequest {
                    role: DeliberationRole::Producer,
                    objective: "write a poem".to_string(),
                    target_files: vec![],
                    producer_content: None,
                    critic_content: None,
                    feedback: vec![],
                    node_kind: NodeKind::Work,
                    tool_context: None,
                },
                &crate::telemetry::NoopTelemetry,
            )
            .result;
        assert!(
            matches!(result, RoleResult::Failed { .. }),
            "provider error must map to Failed, not Rejected, got {result:?}"
        );
    }

    #[test]
    fn provider_terminal_error_maps_to_failed() {
        let runner = ProviderRoleRunner::new(FailingProvider {
            kind: ProviderErrorKind::Terminal,
            message: "auth error".to_string(),
        });
        let result = runner
            .run_role(
                RoleRequest {
                    role: DeliberationRole::Producer,
                    objective: "write a poem".to_string(),
                    target_files: vec![],
                    producer_content: None,
                    critic_content: None,
                    feedback: vec![],
                    node_kind: NodeKind::Work,
                    tool_context: None,
                },
                &crate::telemetry::NoopTelemetry,
            )
            .result;
        assert!(
            matches!(result, RoleResult::Failed { .. }),
            "terminal provider error must map to Failed, not {result:?}"
        );
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
            None,
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
                RoleRequest {
                    role: DeliberationRole::Producer,
                    objective: "recover output".to_string(),
                    target_files: vec![],
                    producer_content: None,
                    critic_content: None,
                    feedback: vec![],
                    node_kind: NodeKind::Work,
                    tool_context: None,
                },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "recover output".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: None,
            },
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
        let provider =
            ScriptedProvider::from_strs(&["invalid one", "invalid two", "invalid three"]);
        let runner = ProviderRoleRunner::new(&provider);

        let result = runner
            .run_role(
                RoleRequest {
                    role: DeliberationRole::Producer,
                    objective: "never valid".to_string(),
                    target_files: vec![],
                    producer_content: None,
                    critic_content: None,
                    feedback: vec![],
                    node_kind: NodeKind::Work,
                    tool_context: None,
                },
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
                RoleRequest {
                    role: DeliberationRole::Referee,
                    objective: "review output".to_string(),
                    target_files: vec![],
                    producer_content: Some("draft".to_string()),
                    critic_content: Some("review".to_string()),
                    feedback: vec![],
                    node_kind: NodeKind::Work,
                    tool_context: None,
                },
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

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "recover output".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: None,
            },
            &telemetry,
        );

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

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "recover output".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: None,
            },
            &telemetry,
        );

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
        let provider =
            ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed safely"}"#]);
        let runner = ProviderRoleRunner::new(&provider);
        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: objective.to_string(),
                target_files,
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
        let large = vec![b'x'; MAX_TARGET_STATE_TEXT_BYTES + 1];
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "read hello.txt".to_string(),
                target_files: vec![],
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "write a file".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "do the thing".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: None,
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "loop forever".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "loop with distinct files".to_string(),
                target_files: vec![],
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "write a file".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Critic,
                objective: "review hello.txt".to_string(),
                target_files: vec![],
                producer_content: Some("draft".to_string()),
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(view),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "inspect hello.txt".to_string(),
                target_files: vec![],
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "loop on list_files".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "list then write".to_string(),
                target_files: vec![],
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "write a file".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "write and confirm".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "produce something".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: None,
            },
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
            RoleRequest {
                role: DeliberationRole::Critic,
                objective: "review the work".to_string(),
                target_files: vec![],
                producer_content: Some("draft".to_string()),
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
        let provider =
            ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
        let runner = ProviderRoleRunner::new_with_max_tokens(&provider, 256);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "test".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: None,
            },
            &crate::telemetry::NoopTelemetry,
        );

        let requests = provider.requests.borrow();
        assert_eq!(
            requests[0].max_tokens, 256,
            "configured max_tokens must be forwarded to the provider"
        );
    }

    #[test]
    fn role_prompt_includes_tool_request_as_valid_response_when_tools_available() {
        let provider =
            ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "test with tools".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
        let provider =
            ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "test".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: None,
            },
            &crate::telemetry::NoopTelemetry,
        );

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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "read the large file".to_string(),
                target_files: vec![],
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
        let provider =
            ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "anything".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: None,
            },
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

        let provider =
            ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "write something".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: None,
            },
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
        let provider =
            ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "produce something".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
        let provider =
            ScriptedProvider::from_strs(&[r#"{"status":"rejected","reason":"needs work"}"#]);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Critic,
                objective: "review the draft".to_string(),
                target_files: vec![],
                producer_content: Some("draft".to_string()),
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Referee,
                objective: "approve the result".to_string(),
                target_files: vec![],
                producer_content: Some("content".to_string()),
                critic_content: Some("looks good".to_string()),
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Critic,
                objective: "review".to_string(),
                target_files: vec![],
                producer_content: Some("draft".to_string()),
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
    fn echoed_replace_text_placeholder_triggers_parse_failure_not_tool_execution() {
        use crate::telemetry::{TelemetryEvent, VecTelemetry};

        let provider = ScriptedProvider::from_strs(&[
            // Exact payload echoed by the confused model in the failing run.
            r#"{"tool":"replace_text","path":"output.txt","old":"...","new":"..."}"#,
            r#"{"status":"accepted","content":"haiku written"}"#,
        ]);
        let runner = ProviderRoleRunner::new(&provider);
        let telemetry = VecTelemetry::new();

        let output = runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "write a haiku".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: None,
            },
            &telemetry,
        );

        assert!(
            matches!(output.result, RoleResult::Accepted { ref content } if content == "haiku written"),
            "placeholder tool request must not execute; got {:?}",
            output.result
        );
        let records = telemetry.records();
        assert!(
            records
                .iter()
                .all(|r| !matches!(r.event, TelemetryEvent::ToolRequested { .. })),
            "placeholder tool request must not emit ToolRequested"
        );
        assert!(
            records
                .iter()
                .any(|r| matches!(&r.event, TelemetryEvent::ParseFailed { .. })),
            "placeholder tool request must emit ParseFailed"
        );
    }

    #[test]
    fn echoed_write_file_placeholder_triggers_parse_failure_not_tool_execution() {
        use crate::telemetry::{TelemetryEvent, VecTelemetry};

        let provider = ScriptedProvider::from_strs(&[
            r#"{"tool":"write_file","path":"output.txt","content":"..."}"#,
            r#"{"status":"accepted","content":"completed"}"#,
        ]);
        let runner = ProviderRoleRunner::new(&provider);
        let telemetry = VecTelemetry::new();

        let output = runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "write something".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: None,
            },
            &telemetry,
        );

        assert!(
            matches!(output.result, RoleResult::Accepted { ref content } if content == "completed"),
            "placeholder write_file must not execute; got {:?}",
            output.result
        );
        let records = telemetry.records();
        assert!(
            records
                .iter()
                .all(|r| !matches!(r.event, TelemetryEvent::ToolRequested { .. })),
            "placeholder write_file must not emit ToolRequested"
        );
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
                render_role_prompt(system, &role, "write a haiku about Rust", pc, cc, &[], None);
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
            None,
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
            let prompt = render_role_prompt(system, &role, "test objective", pc, cc, &[], None);
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
            None,
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
            None,
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
        let provider =
            ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "Update the program.".to_string(),
                target_files: vec!["main.py".to_string()],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "Update the program.\n\nTarget files: prompt.txt".to_string(),
                target_files: vec!["main.py".to_string()],
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
        let provider =
            ScriptedProvider::from_strs(&[r#"{"status":"rejected","reason":"needs work"}"#]);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Critic,
                objective: "Review the update.".to_string(),
                target_files: vec!["main.py".to_string()],
                producer_content: Some("updated main.py".to_string()),
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
            None,
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
            None,
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
            None,
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
            None,
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
            None,
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
                None,
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
            None,
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
        let provider =
            ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
        let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "produce something".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "plan the work".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Plan,
                tool_context: None,
            },
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
        let provider =
            ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"work done"}"#]);
        let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "do the work".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: None,
            },
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
                role: DeliberationRole::Critic,
                objective: "review the plan".to_string(),
                target_files: vec![],
                producer_content: Some("plan graph".to_string()),
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Plan,
                tool_context: None,
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
            RoleRequest {
                role: DeliberationRole::Critic,
                objective: "review the draft".to_string(),
                target_files: vec![],
                producer_content: Some("draft".to_string()),
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: None,
            },
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
                role: DeliberationRole::Referee,
                objective: "approve the plan".to_string(),
                target_files: vec![],
                producer_content: Some("plan graph".to_string()),
                critic_content: Some("plan review".to_string()),
                feedback: vec![],
                node_kind: NodeKind::Plan,
                tool_context: None,
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
        let provider =
            ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"approved"}"#]);
        let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Referee,
                objective: "approve the result".to_string(),
                target_files: vec![],
                producer_content: Some("content".to_string()),
                critic_content: Some("review".to_string()),
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: None,
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "plan the work".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Plan,
                tool_context: None,
            },
            &crate::telemetry::NoopTelemetry,
        );
        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "do the work".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: None,
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "plan the work".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Plan,
                tool_context: None,
            },
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
        let provider = ScriptedProvider::from_strs(&[
            r#"{"tool":"read_file","path":"hello.txt"}"#,
            tasks_json,
        ]);
        let runner = ProviderRoleRunner::new(&provider);

        let output = runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "plan the work".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Plan,
                tool_context: None,
            },
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
        let provider =
            ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "implement the feature".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "plan the work".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Plan,
                tool_context: None,
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "plan the work".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Plan,
                tool_context: None,
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "plan the work".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Plan,
                tool_context: None,
            },
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

        let output = runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "test".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: None,
            },
            &crate::telemetry::NoopTelemetry,
        );

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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "produce output".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: None,
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "plan the work".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Plan,
                tool_context: None,
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "plan the work".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Plan,
                tool_context: None,
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "plan the work".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Plan,
                tool_context: None,
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "plan the work".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Plan,
                tool_context: None,
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "plan the work".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Plan,
                tool_context: None,
            },
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
        let provider =
            ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "write some code".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: None,
            },
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
            RoleRequest {
                role: DeliberationRole::Critic,
                objective: "review the draft".to_string(),
                target_files: vec![],
                producer_content: Some("some draft".to_string()),
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: None,
            },
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
        let provider =
            ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"approved"}"#]);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Referee,
                objective: "approve the result".to_string(),
                target_files: vec![],
                producer_content: Some("content".to_string()),
                critic_content: Some("review".to_string()),
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: None,
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "read hello.txt".to_string(),
                target_files: vec![],
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "replace hello with goodbye in hello.txt".to_string(),
                target_files: vec![],
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
    fn successful_write_file_observation_instructs_final_response() {
        let provider = ScriptedProvider::from_strs(&[
            r#"{"tool":"write_file","path":"result.txt","content":"some output"}"#,
            r#"{"status":"accepted","content":"wrote result.txt"}"#,
        ]);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "write result.txt".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
            &crate::telemetry::NoopTelemetry,
        );

        let requests = provider.requests.borrow();
        assert_eq!(requests.len(), 2, "must call provider twice");
        let second_prompt = &requests[1].prompt;
        assert!(
            second_prompt.contains("The requested change has already been recorded."),
            "successful write_file must include completion-pressure text; got:\n{second_prompt}"
        );
        assert!(
            second_prompt.contains("Do not call any more tools."),
            "successful write_file must prohibit further tool calls; got:\n{second_prompt}"
        );
        assert!(
            !second_prompt.contains("Available file tools:"),
            "completion-pressure prompt must not include the tool section; got:\n{second_prompt}"
        );
    }

    #[test]
    fn successful_delete_file_observation_instructs_final_response() {
        let provider = ScriptedProvider::from_strs(&[
            r#"{"tool":"delete_file","path":"old.txt"}"#,
            r#"{"status":"accepted","content":"deleted old.txt"}"#,
        ]);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "delete old.txt".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
            &crate::telemetry::NoopTelemetry,
        );

        let requests = provider.requests.borrow();
        assert_eq!(requests.len(), 2, "must call provider twice");
        let second_prompt = &requests[1].prompt;
        assert!(
            second_prompt.contains("The requested change has already been recorded."),
            "successful delete_file must include completion-pressure text; got:\n{second_prompt}"
        );
        assert!(
            second_prompt.contains("Do not call any more tools."),
            "successful delete_file must prohibit further tool calls; got:\n{second_prompt}"
        );
        assert!(
            !second_prompt.contains("Available file tools:"),
            "completion-pressure prompt must not include the tool section; got:\n{second_prompt}"
        );
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "write data.txt".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "write out.txt".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
    fn successful_write_file_enables_completion_pressure() {
        let provider = ScriptedProvider::from_strs(&[
            r#"{"tool":"write_file","path":"out.txt","content":"hello"}"#,
            r#"{"status":"accepted","content":"wrote out.txt"}"#,
        ]);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "write out.txt".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
            &crate::telemetry::NoopTelemetry,
        );

        let requests = provider.requests.borrow();
        assert_eq!(requests.len(), 2);
        let second_prompt = &requests[1].prompt;
        assert!(
            second_prompt.contains("The requested change has already been recorded."),
            "write_file must enable completion pressure; got:\n{second_prompt}"
        );
        assert!(
            second_prompt.contains("Do not call any more tools."),
            "completion-pressure prompt must prohibit further tools; got:\n{second_prompt}"
        );
    }

    #[test]
    fn successful_replace_text_enables_completion_pressure() {
        let (_temp, view) = make_view("cp-replace-text");
        let provider = ScriptedProvider::from_strs(&[
            r#"{"tool":"replace_text","path":"hello.txt","old":"hello world","new":"goodbye"}"#,
            r#"{"status":"accepted","content":"replaced text"}"#,
        ]);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "replace text in hello.txt".to_string(),
                target_files: vec![],
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

        let requests = provider.requests.borrow();
        assert_eq!(requests.len(), 2);
        let second_prompt = &requests[1].prompt;
        assert!(
            second_prompt.contains("The requested change has already been recorded."),
            "replace_text must enable completion pressure; got:\n{second_prompt}"
        );
        assert!(
            second_prompt.contains("Do not call any more tools."),
            "completion-pressure prompt must prohibit further tools; got:\n{second_prompt}"
        );
    }

    #[test]
    fn successful_delete_file_enables_completion_pressure() {
        let provider = ScriptedProvider::from_strs(&[
            r#"{"tool":"delete_file","path":"old.txt"}"#,
            r#"{"status":"accepted","content":"deleted old.txt"}"#,
        ]);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "delete old.txt".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
            &crate::telemetry::NoopTelemetry,
        );

        let requests = provider.requests.borrow();
        assert_eq!(requests.len(), 2);
        let second_prompt = &requests[1].prompt;
        assert!(
            second_prompt.contains("The requested change has already been recorded."),
            "delete_file must enable completion pressure; got:\n{second_prompt}"
        );
        assert!(
            second_prompt.contains("Do not call any more tools."),
            "completion-pressure prompt must prohibit further tools; got:\n{second_prompt}"
        );
    }

    #[test]
    fn completion_pressure_hides_tool_section() {
        // After a successful mutation the prompt must not contain the tool section.
        let provider = ScriptedProvider::from_strs(&[
            r#"{"tool":"write_file","path":"out.txt","content":"data"}"#,
            r#"{"status":"accepted","content":"completed"}"#,
        ]);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "write out.txt".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "write out.txt".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "write result.txt".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "plan the work".to_string(),
                target_files: vec![],
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Plan,
                tool_context: None,
            },
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
        let provider =
            ScriptedProvider::from_strs(&[r#"{"status":"rejected","reason":"needs work"}"#]);
        let runner = ProviderRoleRunner::new(&provider);

        let output = runner.run_role(
            RoleRequest {
                role: DeliberationRole::Critic,
                objective: "review the draft".to_string(),
                target_files: vec![],
                producer_content: Some("draft".to_string()),
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Referee,
                objective: "approve the result".to_string(),
                target_files: vec![],
                producer_content: Some("content".to_string()),
                critic_content: Some("review".to_string()),
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(view),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Critic,
                objective: "review the draft".to_string(),
                target_files: vec![],
                producer_content: Some("draft content".to_string()),
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Referee,
                objective: "approve the result".to_string(),
                target_files: vec![],
                producer_content: Some("content".to_string()),
                critic_content: Some("review".to_string()),
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(view),
                }),
            },
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
        responses.push(
            r#"{"status":"rejected","reason":"cannot determine quality without more context"}"#,
        );
        let provider = ScriptedProvider::from_strs(&responses);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Critic,
                objective: "review the draft".to_string(),
                target_files: vec![],
                producer_content: Some("draft".to_string()),
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Critic,
                objective: "review the draft".to_string(),
                target_files: vec![],
                producer_content: Some("draft".to_string()),
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(dummy_view()),
                }),
            },
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
        responses
            .push(r#"{"status":"accepted","content":"produced the required output"}"#.to_string());
        let response_strs: Vec<&str> = responses.iter().map(|s| s.as_str()).collect();
        let provider = ScriptedProvider::from_strs(&response_strs);
        let runner = ProviderRoleRunner::new(&provider);

        let output = runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "read files and produce output".to_string(),
                target_files: vec![],
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
                RoleRequest {
                    role: DeliberationRole::Critic,
                    objective: "review the draft".to_string(),
                    target_files: vec![],
                    producer_content: Some("draft".to_string()),
                    critic_content: None,
                    feedback: vec![],
                    node_kind: NodeKind::Work,
                    tool_context: Some(RoleToolContext {
                        artifact_view: Box::new(dummy_view()),
                    }),
                },
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
            RoleRequest {
                role: DeliberationRole::Critic,
                objective: "review the work".to_string(),
                target_files: vec![],
                producer_content: Some("some content".to_string()),
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(view),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Critic,
                objective: "review the work".to_string(),
                target_files: vec![],
                producer_content: Some("some content".to_string()),
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(view),
                }),
            },
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
                role: DeliberationRole::Critic,
                objective: "review the plan".to_string(),
                target_files: vec![],
                producer_content: Some("plan output".to_string()),
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Plan,
                tool_context: None,
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
        let provider =
            ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"approved"}"#]);
        let runner = ProviderRoleRunner::new(&provider);

        let output = runner.run_role(
            RoleRequest {
                role: DeliberationRole::Referee,
                objective: "approve the result".to_string(),
                target_files: vec![],
                producer_content: Some("content".to_string()),
                critic_content: Some("review".to_string()),
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: None,
            },
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
            RoleRequest {
                role: DeliberationRole::Critic,
                objective: "review the work".to_string(),
                target_files: vec![],
                producer_content: Some("some content".to_string()),
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(view),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Critic,
                objective: "review the work".to_string(),
                target_files: vec![],
                producer_content: Some("some content".to_string()),
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(view),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Critic,
                objective: "review the work".to_string(),
                target_files: vec![],
                producer_content: Some("some content".to_string()),
                critic_content: None,
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(view),
                }),
            },
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
            RoleRequest {
                role: DeliberationRole::Referee,
                objective: "approve the work".to_string(),
                target_files: vec![],
                producer_content: Some("content".to_string()),
                critic_content: Some("review".to_string()),
                feedback: vec![],
                node_kind: NodeKind::Work,
                tool_context: Some(RoleToolContext {
                    artifact_view: Box::new(view),
                }),
            },
            &crate::telemetry::NoopTelemetry,
        );

        assert!(
            matches!(output.result, RoleResult::Accepted { .. }),
            "Referee must accept after a successful read_file; got {:?}",
            output.result
        );
    }
}
