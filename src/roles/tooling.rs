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

pub(super) fn extract_artifact_changed(executor: &mut Option<FileToolExecutor>) -> bool {
    executor.take().is_some_and(|e| e.changed())
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

/// Processes a single tool request within the role loop.
///
/// Handles pressure-mode violations, tool execution, fingerprinting, and prompt
/// updates. Returns `Continue` to keep the loop running or `Fail` to exit early.
#[allow(clippy::too_many_arguments)]
pub(super) fn dispatch_tool_step(
    tool_req: FileToolRequest,
    raw_response: &str,
    executor: &mut Option<FileToolExecutor>,
    proto: &mut ProtocolState,
    telemetry: &dyn TelemetrySink,
    subsource: &str,
    core_prompt: &str,
    observation_suffix: &mut String,
    current_prompt: &mut String,
) -> ToolDispatchOutcome {
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
                raw_response: raw_response.to_string(),
                parse_error: parse_error.clone(),
                attempt_count: proto.current_attempt(),
            },
        ));
        if proto.is_repeated_observation_coercion_active() || !proto.allow_model_call() {
            return ToolDispatchOutcome::Fail(RoleResult::Failed {
                kind: FailureKind::ProtocolFailure,
                reason: parse_error,
            });
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
        *observation_suffix = format!("{observation_suffix}\n\n{violation_note}");
        *current_prompt = format!("{core_prompt}{observation_suffix}");
        return ToolDispatchOutcome::Continue;
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
        return ToolDispatchOutcome::Fail(RoleResult::Failed {
            kind: FailureKind::ToolFailure,
            reason: "tool loop limit reached".to_string(),
        });
    }

    let is_read_file_req = matches!(&tool_req, FileToolRequest::ReadFile { .. });
    let mut read_file_succeeded = false;
    let (observation, mutation_recorded) = match executor {
        Some(exec) => {
            if is_read_file_req {
                proto.record_read_file_attempt();
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

    *observation_suffix = format!("{observation_suffix}\n\n{obs_section}");
    *current_prompt = if !proto.allow_tool_call() {
        format!("{core_prompt}{observation_suffix}")
    } else {
        format!("{current_prompt}\n\n{obs_section}")
    };
    ToolDispatchOutcome::Continue
}
