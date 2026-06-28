//! Prompt rendering functions for role invocations.

use crate::machines::deliberation::state::{DeliberationRole, RevisionFeedback};
use crate::roles::{TargetView, TargetViewKind};
use crate::services::extract_json_object;
use crate::tools::{FileToolPolicy, FileToolResponse, parse_tool_request};

pub(super) fn render_objective_for_prompt(objective: &str, target_files: &[String]) -> String {
    if target_files.is_empty() {
        objective.to_string()
    } else {
        format!("{objective}\n\nTarget files: {}", target_files.join(", "))
    }
}

pub(super) fn format_tool_observation(
    response: &FileToolResponse,
    max_observation_bytes: usize,
) -> String {
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
pub(super) fn cap_observation(s: String, max_bytes: usize) -> String {
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
pub(super) fn format_observation_section(observation: &str, mutation_recorded: bool) -> String {
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
pub(super) fn render_tool_section(policy: &FileToolPolicy) -> String {
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

pub(super) fn role_subsource(role: &DeliberationRole) -> &'static str {
    match role {
        DeliberationRole::Producer => "Producer",
        DeliberationRole::Critic => "Critic",
        DeliberationRole::Referee => "Referee",
    }
}

pub(super) fn render_retry_prompt(original_prompt: &str, parse_error: &str) -> String {
    format!(
        "{original_prompt}\n\n\
         Your previous response could not be parsed: {parse_error}\n\
         Return only one JSON object matching one of these schemas:\n\
         {{\"status\":\"accepted\",\"content\":\"$RESPONSE_SUMMARY\"}}\n\
         {{\"status\":\"rejected\",\"reason\":\"$REASON_FOR_REJECTION\"}}\n\
         Do not copy example values. Replace them with task-specific content."
    )
}

pub(super) fn render_reviewer_must_read_prompt(original_prompt: &str, parse_error: &str) -> String {
    format!(
        "{original_prompt}\n\n\
         Your previous response tried to accept without reading any file: {parse_error}\n\
         You must use read_file to inspect the relevant file contents before deciding.\n\
         Read the specific file(s) the producer was expected to modify, then return your decision.\n\
         Return a tool request to read the relevant file(s)."
    )
}

pub(super) fn render_planner_retry_prompt(original_prompt: &str, parse_error: &str) -> String {
    format!(
        "{original_prompt}\n\n\
         Your previous response could not be parsed: {parse_error}\n\
         Return only one JSON object matching this schema:\n\
         {{\"tasks\":[{{\"id\":\"task-id\",\"objective\":\"Task objective.\",\"depends_on\":[]}}]}}\n\
         Do not copy example values. Replace them with actual task IDs and objectives."
    )
}

/// Formats the observation section that signals completion-pressure mode.
pub(super) fn format_completion_pressure_section(observation: &str) -> String {
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

/// Formats the observation section that signals decision-pressure mode for read-only reviewer roles.
pub(super) fn format_decision_pressure_section(observation: &str) -> String {
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

pub(super) fn render_completion_pressure_violation_note() -> String {
    "Tools are no longer available.\n\
     The requested change has already been recorded.\n\
     Return a final role response:\n\
     {\"status\":\"accepted\",\"content\":\"$RESPONSE_SUMMARY\"}\n\
     {\"status\":\"rejected\",\"reason\":\"$REASON_FOR_REJECTION\"}\n\
     Do not copy example values. Replace them with task-specific content."
        .to_string()
}

pub(super) fn render_decision_pressure_violation_note() -> String {
    "Tools are no longer available.\n\
     You have gathered sufficient evidence to make a decision.\n\
     Return a final role response:\n\
     {\"status\":\"accepted\",\"content\":\"$RESPONSE_SUMMARY\"}\n\
     {\"status\":\"rejected\",\"reason\":\"$REASON_FOR_REJECTION\"}\n\
     Do not copy example values. Replace them with task-specific content."
        .to_string()
}

/// Formats the observation section that signals repeated-observation coercion.
pub(super) fn format_repeated_observation_coercion_section(observation: &str) -> String {
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

/// Returns `Some(err_msg)` when `s` contains a placeholder tool request.
pub(super) fn detect_placeholder_tool_echo(s: &str) -> Option<String> {
    let json = extract_json_object(s)?;
    match parse_tool_request(json) {
        Err(e) if e.contains("placeholder") => Some(e),
        _ => None,
    }
}

/// Builds a protocol-retry prompt for use in completion-pressure mode.
pub(super) fn render_completion_pressure_retry_prompt(
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
pub(super) fn render_role_prompt(
    system: &str,
    role: &DeliberationRole,
    objective: &str,
    producer_content: Option<&str>,
    critic_content: Option<&str>,
    feedback: &[RevisionFeedback],
    target_views: &[TargetView],
) -> String {
    let mut parts = Vec::new();
    parts.push(format!("Objective: {objective}"));
    parts.push(format!("Role: {role:?}"));
    if !target_views.is_empty() {
        parts.push(render_target_state_view(target_views));
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

pub(super) fn render_target_state_view(views: &[TargetView]) -> String {
    let mut lines = vec![
        "Target state view (built from structured target_files):".to_string(),
        "This view is prompt context only; file tools remain the source of operational access."
            .to_string(),
    ];
    for view in views {
        lines.push(format!("- target: {}", view.id));
        lines.push(format!("  exists: {}", view.exists));
        match &view.kind {
            TargetViewKind::FullText => {
                lines.push(format!(
                    "  representation: file text ({} bytes)",
                    view.representation.len()
                ));
                lines.push("  content:".to_string());
                lines.push(indent_target_state_content(&view.representation));
            }
            TargetViewKind::Absent => {
                lines.push("  representation: absent".to_string());
            }
            TargetViewKind::TooLarge => {
                lines.push(format!("  representation: {}", view.representation));
            }
            TargetViewKind::Error => {
                lines.push(format!("  representation: error: {}", view.representation));
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
