//! Provider-backed role execution.
//!
//! `RoleRunner` owns one complete role round-trip: render prompt, call provider,
//! parse JSON, retry on protocol failure. The deliberation layer above sees only
//! `RoleRequest` in and `RoleResult` out.

use std::cell::RefCell;
use std::rc::Rc;

use crate::artifacts::{ArtifactRead, Workspace};
use crate::machines::deliberation::event::RoleResult;
use crate::machines::deliberation::state::{DeliberationRole, RevisionFeedback};
use crate::machines::scheduler::{FailureKind, NodeKind, TestPlanContext};
use crate::node_runner::planner::{try_parse_planner_response, validate_planner_output};
use crate::providers::{ProviderClient, ProviderErrorKind, ProviderRequest, StructuredOutput};
use crate::roles::TargetView;
use crate::roles::policy::RolePolicy;
use crate::services::extract_json_object;
use crate::telemetry::{TelemetryEvent, TelemetryRecord, TelemetrySink};
use crate::tools::{FileToolExecutor, parse_tool_request};

use super::parser::{strip_code_fence, try_parse_role_response};
#[cfg(test)]
use super::prompt::render_role_prompt;
use super::prompt::{
    RolePromptRender, detect_placeholder_tool_echo, render_completion_pressure_retry_prompt,
    render_objective_for_prompt, render_planner_retry_prompt, render_retry_prompt,
    render_reviewer_must_read_prompt, render_role_prompt_with_test_plan_context,
    render_tool_section, role_subsource,
};
use super::protocol_state::ProtocolState;
use super::tooling::{
    ToolDispatchOutcome, dispatch_tool_step, extract_artifact_changed, file_tool_policy_for_request,
};

#[cfg(test)]
use super::parser::MIN_CONTENT_LENGTH;
#[cfg(test)]
use super::prompt::format_tool_observation;
#[cfg(test)]
use super::protocol_state::{MAX_PROTOCOL_RETRIES, MAX_READ_ONLY_TOOL_STEPS, MAX_TOOL_STEPS};
#[cfg(test)]
use crate::tools::{FileToolPolicy, FileToolResponse};

/// A read-only view of the artifact made available to role tool loops.
pub struct RoleToolContext {
    /// The artifact state the role may read from.
    ///
    /// In artifact Work this is the shared WorkAttempt workspace, so reviewer
    /// roles see producer writes directly.
    pub artifact_view: Box<dyn ArtifactRead>,
    /// Optional live workspace for artifact-producing Work attempts.
    pub writable_workspace: Option<Rc<RefCell<Workspace>>>,
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
    /// Structured test-target planning context for this node.
    pub test_plan_context: TestPlanContext,
    /// Adapter-produced views of the target files for prompt context.
    ///
    /// Built by [`ProjectAdapter::build_target_views`] before the request is
    /// dispatched. The runner renders these verbatim and never inspects the
    /// artifact directly for target-state content.
    ///
    /// [`ProjectAdapter::build_target_views`]: crate::project::ProjectAdapter::build_target_views
    pub target_views: Vec<TargetView>,
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
    /// Whether any file tool changed artifact state.
    pub artifact_changed: bool,
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

impl<P: ProviderClient> RoleRunner for ProviderRoleRunner<P> {
    fn run_role(&self, request: RoleRequest, telemetry: &dyn TelemetrySink) -> RoleRunOutput {
        let subsource = role_subsource(&request.role);
        let has_tools = request.tool_context.is_some();

        let policy =
            file_tool_policy_for_request(&request.role, &request.node_kind, &request.target_files);

        let system = match (&request.node_kind, &request.role) {
            (NodeKind::Plan, DeliberationRole::Producer) => &self.policy.planner_producer_system,
            (NodeKind::Plan, DeliberationRole::Critic) => &self.policy.planner_critic_system,
            (NodeKind::Plan, DeliberationRole::Referee) => &self.policy.planner_referee_system,
            (NodeKind::Work, DeliberationRole::Producer) => &self.policy.worker_producer_system,
            (NodeKind::Work, DeliberationRole::Critic) => &self.policy.worker_critic_system,
            (NodeKind::Work, DeliberationRole::Referee) => &self.policy.worker_referee_system,
        };

        let rendered_objective =
            render_objective_for_prompt(&request.objective, &request.target_files);
        let core_prompt = render_role_prompt_with_test_plan_context(RolePromptRender {
            system,
            role: &request.role,
            objective: &rendered_objective,
            producer_content: request.producer_content.as_deref(),
            critic_content: request.critic_content.as_deref(),
            feedback: &request.feedback,
            target_views: &request.target_views,
            test_plan_context: &request.test_plan_context,
        });
        let base_prompt = if has_tools {
            format!("{core_prompt}\n\n{}", render_tool_section(&policy))
        } else {
            core_prompt.clone()
        };

        let mut executor: Option<FileToolExecutor> = request.tool_context.map(|ctx| {
            if let Some(workspace) = ctx.writable_workspace {
                FileToolExecutor::with_workspace(ctx.artifact_view, workspace, policy)
            } else {
                FileToolExecutor::with_policy(ctx.artifact_view, policy)
            }
        });

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
                    let artifact_changed = extract_artifact_changed(&mut executor);
                    return RoleRunOutput {
                        result: RoleResult::Failed {
                            kind,
                            reason: format!("provider error ({:?}): {}", err.kind, err.message),
                        },
                        artifact_changed,
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
                match dispatch_tool_step(
                    tool_req,
                    &response.content,
                    &mut executor,
                    &mut proto,
                    telemetry,
                    subsource,
                    &core_prompt,
                    &mut observation_suffix,
                    &mut current_prompt,
                ) {
                    ToolDispatchOutcome::Continue => continue,
                    ToolDispatchOutcome::Fail(result) => {
                        let artifact_changed = extract_artifact_changed(&mut executor);
                        return RoleRunOutput {
                            result,
                            artifact_changed,
                        };
                    }
                }
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
                            let artifact_changed = extract_artifact_changed(&mut executor);
                            return RoleRunOutput {
                                result: RoleResult::Accepted { content: canonical },
                                artifact_changed,
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
                                let artifact_changed = extract_artifact_changed(&mut executor);
                                return RoleRunOutput {
                                    result: RoleResult::Failed {
                                        kind: FailureKind::PlannerValidationFailure,
                                        reason: err,
                                    },
                                    artifact_changed,
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
                            let artifact_changed = extract_artifact_changed(&mut executor);
                            return RoleRunOutput {
                                result: RoleResult::Failed {
                                    kind: FailureKind::ProtocolFailure,
                                    reason: parse_error,
                                },
                                artifact_changed,
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
                                let artifact_changed = extract_artifact_changed(&mut executor);
                                return RoleRunOutput {
                                    result: RoleResult::Failed {
                                        kind: FailureKind::ProtocolFailure,
                                        reason: parse_error,
                                    },
                                    artifact_changed,
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
                        let artifact_changed = extract_artifact_changed(&mut executor);
                        return RoleRunOutput {
                            result,
                            artifact_changed,
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
                            let artifact_changed = extract_artifact_changed(&mut executor);
                            return RoleRunOutput {
                                result: RoleResult::Failed {
                                    kind: FailureKind::ProtocolFailure,
                                    reason: terminal_reason,
                                },
                                artifact_changed,
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

#[cfg(test)]
#[path = "runner_tests/mod.rs"]
mod tests;
