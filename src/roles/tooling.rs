//! Tool dispatch and policy helpers for role execution.

use crate::machines::deliberation::DeliberationRole;
use crate::machines::scheduler::{FailureKind, NodeKind};
use crate::roles::runner::RoleResult;
use crate::telemetry::{TelemetryEvent, TelemetryRecord, TelemetrySink};
use crate::tools::{FileToolExecutor, FileToolPolicy, FileToolRequest, FileToolResponse};

use super::prompt::{
    format_completion_pressure_section, format_decision_pressure_section,
    format_observation_section, format_repeated_observation_coercion_section,
    format_tool_observation, render_completion_pressure_violation_note,
    render_decision_pressure_violation_note,
};
use super::protocol_state::ProtocolState;

/// Outcome of a single tool-dispatch step within the role loop.
pub(super) enum ToolDispatchOutcome {
    /// The loop should continue to the next iteration.
    Continue,
    /// The role loop should exit with this result (caller wraps with `extract_update`).
    Fail(RoleResult),
}

pub(super) struct RoleToolDispatcher<'a> {
    executor: Option<FileToolExecutor>,
    proto: ProtocolState,
    telemetry: &'a dyn TelemetrySink,
    subsource: &'a str,
    core_prompt: String,
    base_prompt: String,
    observation_suffix: String,
    current_prompt: String,
}

impl<'a> RoleToolDispatcher<'a> {
    pub(super) fn new(
        executor: Option<FileToolExecutor>,
        proto: ProtocolState,
        telemetry: &'a dyn TelemetrySink,
        subsource: &'a str,
        core_prompt: String,
        base_prompt: String,
    ) -> Self {
        Self {
            executor,
            proto,
            telemetry,
            subsource,
            current_prompt: base_prompt.clone(),
            base_prompt,
            core_prompt,
            observation_suffix: String::new(),
        }
    }

    pub(super) fn artifact_changed(mut self) -> bool {
        self.executor.take().is_some_and(|e| e.changed())
    }

    pub(super) fn current_prompt(&self) -> &str {
        &self.current_prompt
    }

    pub(super) fn current_attempt(&self) -> usize {
        self.proto.current_attempt()
    }

    pub(super) fn allow_tool_call(&self) -> bool {
        self.proto.allow_tool_call()
    }

    pub(super) fn allow_model_call(&self) -> bool {
        self.proto.allow_model_call()
    }

    pub(super) fn record_protocol_failure(&mut self) {
        self.proto.record_protocol_failure();
    }

    pub(super) fn reviewer_accepted_without_reading(&self) -> bool {
        self.proto.reviewer_accepted_without_reading()
    }

    pub(super) fn read_file_attempted(&self) -> usize {
        self.proto.read_file_attempted()
    }

    pub(super) fn reviewer_accept_must_fail_immediately(&self) -> bool {
        self.proto.reviewer_accept_must_fail_immediately()
    }

    pub(super) fn render_planner_retry_prompt(
        &mut self,
        parse_error: &str,
        raw_response: &str,
        planner_protocol_schema: &str,
    ) {
        self.current_prompt = super::prompt::render_planner_retry_prompt(
            &self.base_prompt,
            parse_error,
            raw_response,
            planner_protocol_schema,
        );
    }

    pub(super) fn render_reviewer_must_read_prompt(&mut self, parse_error: &str) {
        self.current_prompt =
            super::prompt::render_reviewer_must_read_prompt(&self.base_prompt, parse_error);
    }

    pub(super) fn render_role_retry_prompt(
        &mut self,
        parse_error: &str,
        raw_response: &str,
        is_work_producer: bool,
    ) {
        self.current_prompt = if !self.proto.allow_tool_call() {
            super::prompt::render_completion_pressure_retry_prompt(
                &self.core_prompt,
                &self.observation_suffix,
                parse_error,
                is_work_producer,
            )
        } else {
            super::prompt::render_retry_prompt(
                &self.base_prompt,
                parse_error,
                raw_response,
                is_work_producer,
            )
        };
    }

    /// Processes a single tool request within the role loop.
    ///
    /// Handles pressure-mode violations, tool execution, fingerprinting, and prompt
    /// updates. Returns `Continue` to keep the loop running or `Fail` to exit early.
    pub(super) fn dispatch_tool_step(
        &mut self,
        tool_req: FileToolRequest,
        raw_response: &str,
    ) -> ToolDispatchOutcome {
        if !self.proto.allow_tool_call() {
            let parse_error = if self.proto.is_repeated_observation_coercion_active() {
                "protocol error: repeated identical tool observations; model continued calling tools after coercion".to_string()
            } else {
                "tool request received while no tools are available".to_string()
            };
            self.telemetry.record(TelemetryRecord::new_with_subsource(
                "RoleMachine",
                self.subsource,
                TelemetryEvent::ParseFailed {
                    raw_response: raw_response.to_string(),
                    parse_error: parse_error.clone(),
                    attempt_count: self.proto.current_attempt(),
                },
            ));
            if self.proto.is_repeated_observation_coercion_active()
                || !self.proto.allow_model_call()
            {
                return ToolDispatchOutcome::Fail(RoleResult::Failed {
                    kind: FailureKind::ProtocolFailure,
                    reason: parse_error,
                });
            }
            self.proto.record_protocol_failure();
            self.telemetry.record(TelemetryRecord::new_with_subsource(
                "RoleMachine",
                self.subsource,
                TelemetryEvent::ProtocolRetry {
                    parse_error: parse_error.clone(),
                    attempt_count: self.proto.current_attempt(),
                },
            ));
            let violation_note = if self.proto.is_decision_pressure_active() {
                render_decision_pressure_violation_note()
            } else {
                render_completion_pressure_violation_note()
            };
            self.observation_suffix = format!("{}\n\n{violation_note}", self.observation_suffix);
            self.current_prompt = format!("{}{}", self.core_prompt, self.observation_suffix);
            return ToolDispatchOutcome::Continue;
        }

        self.proto.record_tool_call();
        let tool_name = tool_name_of(&tool_req);
        self.telemetry.record(TelemetryRecord::new_with_subsource(
            "RoleMachine",
            self.subsource,
            TelemetryEvent::ToolRequested {
                tool: tool_name.clone(),
            },
        ));

        if self.proto.tool_loop_limit_reached() {
            self.telemetry.record(TelemetryRecord::new_with_subsource(
                "RoleMachine",
                self.subsource,
                TelemetryEvent::ToolLoopLimitReached,
            ));
            return ToolDispatchOutcome::Fail(RoleResult::Failed {
                kind: FailureKind::ToolFailure,
                reason: "tool loop limit reached".to_string(),
            });
        }

        let is_read_file_req = matches!(&tool_req, FileToolRequest::ReadFile { .. });
        let mut read_file_succeeded = false;
        let (observation, mutation_recorded) = match &mut self.executor {
            Some(exec) => {
                if is_read_file_req {
                    self.proto.record_read_file_attempt();
                }
                let max_obs = exec.policy().max_observation_bytes;
                let response = exec.execute(tool_req);
                if is_read_file_req && matches!(response, FileToolResponse::FileContents { .. }) {
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

        let fingerprint = format!("{tool_name}\n{observation}");

        self.telemetry.record(TelemetryRecord::new_with_subsource(
            "RoleMachine",
            self.subsource,
            TelemetryEvent::ToolReturned {
                tool: tool_name,
                result: observation.clone(),
            },
        ));

        self.proto
            .record_tool_result(fingerprint, mutation_recorded, read_file_succeeded);

        let obs_section = if !self.proto.allow_tool_call() {
            if self.proto.is_repeated_observation_coercion_active() {
                format_repeated_observation_coercion_section(
                    &observation,
                    self.proto.is_work_producer(),
                )
            } else if self.proto.is_decision_pressure_active() {
                format_decision_pressure_section(&observation)
            } else {
                format_completion_pressure_section(&observation)
            }
        } else {
            format_observation_section(
                &observation,
                mutation_recorded,
                self.proto.is_work_producer(),
            )
        };

        self.observation_suffix = format!("{}\n\n{obs_section}", self.observation_suffix);
        self.current_prompt = if !self.proto.allow_tool_call() {
            format!("{}{}", self.core_prompt, self.observation_suffix)
        } else {
            format!("{}\n\n{obs_section}", self.current_prompt)
        };
        ToolDispatchOutcome::Continue
    }
}

pub(super) fn tool_name_of(req: &FileToolRequest) -> String {
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
pub(super) fn file_tool_policy_for_role(role: &DeliberationRole) -> FileToolPolicy {
    match role {
        DeliberationRole::Producer => FileToolPolicy::default(),
        DeliberationRole::Critic | DeliberationRole::Referee => FileToolPolicy {
            allow_writes: false,
            ..FileToolPolicy::default()
        },
    }
}

pub(super) fn file_tool_policy_for_request(
    role: &DeliberationRole,
    node_kind: &NodeKind,
    target_files: &[String],
) -> FileToolPolicy {
    let mut policy = file_tool_policy_for_role(role);
    if node_kind == &NodeKind::Work && !target_files.is_empty() {
        policy.allowed_paths = Some(target_files.to_vec());
    }
    policy
}
