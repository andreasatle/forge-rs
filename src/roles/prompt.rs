//! Prompt rendering functions for role invocations.

use crate::machines::deliberation::{ArtifactContext, DeliberationContext};
use crate::machines::deliberation::{DeliberationRole, RevisionFeedback};
use crate::machines::scheduler::{NodeKind, TestPlanContext};
use crate::roles::{TargetView, TargetViewKind};
use crate::tools::{FileToolPolicy, FileToolResponse};

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
pub(super) fn format_observation_section(
    observation: &str,
    mutation_recorded: bool,
    is_work_producer: bool,
) -> String {
    let schema = status_schema_lines(is_work_producer);
    let base = format!(
        "Framework tool observation:\n{observation}\n\
         This is framework output, not a valid response format.\n\
         If the requested change is complete, return exactly one JSON object using the final response.\n\
         {schema}\n\
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
    let mut s = String::from("Available file tools:\n");
    if let Some(allowed) = &policy.allowed_paths {
        s.push_str(&format!("Allowed target files: {}\n", allowed.join(", ")));
    }
    s.push_str(
        "ToolRequest variants:\n\
         - list_files: `tool` must be \"list_files\".\n\
         - read_file: `tool` must be \"read_file\"; `path` must be a target file path string.\n",
    );
    if policy.allow_writes {
        s.push_str(
            "- write_file: `tool` must be \"write_file\"; `path` must be a target file path string; `content` must be the complete file content string.\n\
             - replace_text: `tool` must be \"replace_text\"; `path` must be a target file path string; `old` must be the exact existing text; `new` must be the replacement text.\n\
             - delete_file: `tool` must be \"delete_file\"; `path` must be a target file path string.\n",
        );
        s.push_str(
            "Tool selection guidance:\n\
             - Use write_file by default when creating a file or replacing most or all of an existing file.\n\
             - Use replace_text only for small, localized edits after you have read the file and can provide an exact old string that occurs once.\n\
             - replace_text matches bytes exactly; whitespace, indentation, or formatting differences will cause it to fail.\n\
             - Newlines in write_file content must be real newline characters encoded as \\n in the JSON string, not the literal two-character sequence backslash followed by n. Double-escaping produces a single-line file that fails to parse.\n",
        );
    }
    s.push_str(
        "You may return either:\n\
         1. a tool request JSON, or\n\
         2. a final role result JSON.\n\
         Return exactly one JSON object.",
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

pub(super) fn render_retry_prompt(
    original_prompt: &str,
    parse_error: &str,
    raw_response: &str,
    is_work_producer: bool,
) -> String {
    let schema_desc = schema_match_phrase(is_work_producer);
    let schema = status_schema_lines(is_work_producer);
    format!(
        "{original_prompt}\n\n\
         Your previous response could not be parsed: {parse_error}\n\n\
         Your response was:\n\
         {raw_response}\n\n\
         Return only one JSON object matching {schema_desc}:\n\
         {schema}"
    )
}

/// The Work-node Producer's job is to implement — it never rejects, so its
/// response is a single `summary` field with no `status` tag. Critic and
/// Referee use the status/content accept-or-reject wrapper.
fn status_schema_lines(is_work_producer: bool) -> &'static str {
    if is_work_producer {
        "`summary` must be a non-empty task-specific string describing what you did."
    } else {
        "Accepted: `status` must be \"accepted\"; `content` must be a non-empty task-specific string.\n\
         Rejected: `status` must be \"rejected\"; `reason` must be a non-empty task-specific string."
    }
}

fn schema_match_phrase(is_work_producer: bool) -> &'static str {
    if is_work_producer {
        "this schema"
    } else {
        "one of these schemas"
    }
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

pub(super) fn render_planner_retry_prompt(
    original_prompt: &str,
    parse_error: &str,
    raw_response: &str,
    schema: &str,
) -> String {
    format!(
        "{original_prompt}\n\n\
         Your previous response could not be parsed: {parse_error}\n\n\
         Your response was:\n\
         {raw_response}\n\n\
         Return only one JSON object matching this schema:\n\
         {schema}"
    )
}

/// Formats the observation section that signals completion-pressure mode.
///
/// Completion pressure only ever activates for the Work-node Producer (see
/// [`ProtocolState::record_tool_result`]), so this section shows only the
/// accepted schema.
///
/// [`ProtocolState::record_tool_result`]: super::protocol_state::ProtocolState::record_tool_result
pub(super) fn format_completion_pressure_section(observation: &str) -> String {
    format!(
        "Framework tool observation:\n{observation}\n\
         This is framework output, not a valid response format.\n\
         The requested change has already been recorded.\n\
         Do not call any more tools.\n\
         Return exactly one JSON object using the final response.\n\
         `summary` must be a non-empty task-specific string describing what you did."
    )
}

/// Formats the observation section that signals decision-pressure mode for read-only reviewer roles.
pub(super) fn format_decision_pressure_section(observation: &str) -> String {
    format!(
        "Framework tool observation:\n{observation}\n\
         This is framework output, not a valid response format.\n\
         You have gathered sufficient evidence to make a decision.\n\
         Do not call any more tools.\n\
         Return exactly one JSON object using one final response variant:\n\
         Accepted: `status` must be \"accepted\"; `content` must be a non-empty task-specific string.\n\
         Rejected: `status` must be \"rejected\"; `reason` must be a non-empty task-specific string."
    )
}

pub(super) fn render_completion_pressure_violation_note() -> String {
    "Tools are no longer available.\n\
     The requested change has already been recorded.\n\
     Return exactly one JSON object using this final role response:\n\
     `summary` must be a non-empty task-specific string describing what you did."
        .to_string()
}

pub(super) fn render_decision_pressure_violation_note() -> String {
    "Tools are no longer available.\n\
     You have gathered sufficient evidence to make a decision.\n\
     Return exactly one JSON object using one final role response:\n\
     Accepted: `status` must be \"accepted\"; `content` must be a non-empty task-specific string.\n\
     Rejected: `status` must be \"rejected\"; `reason` must be a non-empty task-specific string."
        .to_string()
}

/// Formats the observation section that signals repeated-observation coercion.
///
/// Reachable by any role, so the schema shown depends on `is_work_producer`.
pub(super) fn format_repeated_observation_coercion_section(
    observation: &str,
    is_work_producer: bool,
) -> String {
    let decision_verb = if is_work_producer {
        "Return the summary JSON now."
    } else {
        "Return accepted or rejected JSON now."
    };
    let schema_desc = schema_match_phrase(is_work_producer);
    let schema = status_schema_lines(is_work_producer);
    format!(
        "Framework tool observation:\n{observation}\n\
         You have already inspected this information. Do not call more tools.\n\
         {decision_verb}\n\
         Return exactly {schema_desc}:\n\
         {schema}"
    )
}

/// Builds a protocol-retry prompt for use in completion-pressure mode.
pub(super) fn render_completion_pressure_retry_prompt(
    core: &str,
    observation_suffix: &str,
    parse_error: &str,
    is_work_producer: bool,
) -> String {
    let schema_desc = schema_match_phrase(is_work_producer);
    let schema = status_schema_lines(is_work_producer);
    format!(
        "{core}{observation_suffix}\n\n\
         Your previous response could not be parsed: {parse_error}\n\
         Tools are no longer available.\n\
         Return exactly {schema_desc}:\n\
         {schema}"
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NodeReviewContract {
    pub(super) target_files: Vec<String>,
    pub(super) required_validation_targets: Vec<String>,
    pub(super) planned_follow_up_targets: Vec<String>,
    pub(super) covered_test_targets: Vec<String>,
    pub(super) missing_test_targets: Vec<String>,
    pub(super) requires_file_inspection: bool,
}

impl NodeReviewContract {
    pub(super) fn for_role(
        role: &DeliberationRole,
        node_kind: &NodeKind,
        target_files: &[String],
        test_plan_context: &TestPlanContext,
        has_tools: bool,
    ) -> Option<Self> {
        if !matches!(
            role,
            DeliberationRole::Producer | DeliberationRole::Critic | DeliberationRole::Referee
        ) {
            return None;
        }

        let planned_set = test_plan_context
            .planned_test_targets
            .iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>();
        let covered_test_targets = test_plan_context
            .required_validation_targets
            .iter()
            .filter(|target| planned_set.contains(target.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        let missing_test_targets = test_plan_context
            .required_validation_targets
            .iter()
            .filter(|target| !planned_set.contains(target.as_str()))
            .cloned()
            .collect::<Vec<_>>();

        Some(Self {
            target_files: target_files.to_vec(),
            required_validation_targets: test_plan_context.required_validation_targets.clone(),
            planned_follow_up_targets: test_plan_context.planned_test_targets.clone(),
            covered_test_targets,
            missing_test_targets,
            requires_file_inspection: *node_kind == NodeKind::Work && has_tools,
        })
    }
}

pub(super) struct RolePromptRender<'a> {
    pub(super) system: &'a str,
    pub(super) role: &'a DeliberationRole,
    pub(super) objective: &'a str,
    pub(super) context: &'a DeliberationContext,
    pub(super) producer_content: Option<&'a str>,
    pub(super) critic_content: Option<&'a str>,
    pub(super) feedback: &'a [RevisionFeedback],
    pub(super) target_views: &'a [TargetView],
    pub(super) test_plan_context: &'a TestPlanContext,
    pub(super) review_contract: Option<&'a NodeReviewContract>,
    /// Worker role name/description pairs to surface to the model. Callers
    /// pass an empty slice for every role except the Plan-node Producer.
    pub(super) worker_role_descriptions: &'a [(String, String)],
}

pub(super) fn render_role_prompt_with_test_plan_context(input: RolePromptRender<'_>) -> String {
    let mut parts = Vec::new();
    let renders_review_contract = input.review_contract.is_some()
        && matches!(
            input.role,
            DeliberationRole::Critic | DeliberationRole::Referee
        );
    if let Some(context) = render_deliberation_context(input.context, !renders_review_contract) {
        parts.push(format!("Context:\n{context}"));
    }
    parts.push(format!("Objective: {}", input.objective));
    parts.push(format!("Role: {:?}", input.role));
    if !input.target_views.is_empty() {
        parts.push(render_target_state_view(input.target_views));
    }
    if let Some(contract) = input.review_contract {
        if renders_review_contract {
            parts.push(render_node_review_contract(contract));
        }
    } else if !input
        .test_plan_context
        .required_validation_targets
        .is_empty()
    {
        parts.push(render_test_plan_context(input.test_plan_context));
    }
    if let Some(pc) = input.producer_content {
        parts.push(format!("Producer content: {pc}"));
    }
    if let Some(cc) = input.critic_content {
        parts.push(format!("Critic content: {cc}"));
    }
    if !input.feedback.is_empty() {
        let reasons: Vec<&str> = input.feedback.iter().map(|f| f.reason.as_str()).collect();
        parts.push(format!("Revision feedback: {}", reasons.join("; ")));
    }
    if !input.worker_role_descriptions.is_empty() {
        parts.push(render_worker_role_descriptions(
            input.worker_role_descriptions,
        ));
    }
    parts.push(input.system.to_string());
    parts.join("\n")
}

/// Renders the "Available worker roles" section shown to the Plan-node
/// Producer so it can assign roles explicitly to each task.
fn render_worker_role_descriptions(roles: &[(String, String)]) -> String {
    let mut lines = vec!["Available worker roles:".to_string()];
    for (role, description) in roles {
        lines.push(format!("- {role}: {description}"));
    }
    lines.join("\n")
}

fn render_deliberation_context(
    context: &DeliberationContext,
    include_target_files: bool,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(artifact) = &context.artifact {
        parts.push(render_artifact_context(artifact));
    }
    if let Some(requirement) = &context.testing_requirement {
        parts.push(requirement.clone());
    }
    if include_target_files && !context.target_files.is_empty() {
        parts.push(format!("Target files: {}", context.target_files.join(", ")));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

fn render_artifact_context(context: &ArtifactContext) -> String {
    let mut parts = Vec::new();
    let listing: Vec<String> = context
        .files
        .iter()
        .map(|path| format!("  {path}"))
        .collect();
    parts.push(format!(
        "Existing project files (already initialized — do not create tasks to recreate \
         or reinitialize these files unless the objective explicitly names them as targets):\n{}",
        listing.join("\n")
    ));
    if let Some(api_summary) = &context.api_summary {
        parts.push(format!("Current artifact state:\n{api_summary}"));
    }
    for file in &context.selected_files {
        parts.push(format!("{}:\n{}", file.path, file.content));
    }
    parts.join("\n\n")
}

pub(super) fn render_node_review_contract(contract: &NodeReviewContract) -> String {
    let mut lines = vec![
        "Node review contract (typed role-boundary metadata):".to_string(),
        "Evaluate the current node contract, not the entire project state.".to_string(),
        format!(
            "Current node target files: {}",
            render_list_or_none(&contract.target_files)
        ),
        format!(
            "Adapter-required test targets for current target files: {}",
            render_list_or_none(&contract.required_validation_targets)
        ),
        format!(
            "Declared follow-up/dependent target files: {}",
            render_list_or_none(&contract.planned_follow_up_targets)
        ),
    ];

    if !contract.covered_test_targets.is_empty() {
        lines.push(format!(
            "Required test targets covered by declared follow-up work: {}",
            contract.covered_test_targets.join(", ")
        ));
        lines.push(
            "Acceptance guidance: accept a correct source-only current node even when these covered test files do not exist yet."
                .to_string(),
        );
        lines.push(
            "Do not reject this current node solely because covered tests are planned separately."
                .to_string(),
        );
    }

    if !contract.missing_test_targets.is_empty() {
        lines.push(format!(
            "Required test targets not covered by declared follow-up work: {}",
            contract.missing_test_targets.join(", ")
        ));
        lines.push(
            "Acceptance guidance: if this current node changes code and no declared follow-up covers these tests, missing tests remain a valid rejection."
                .to_string(),
        );
    }

    if contract.requires_file_inspection {
        lines.push(
            "Inspection requirement: before accepting, inspect the current node target files with read_file."
                .to_string(),
        );
    }

    lines.push(
        "Overall project completion is checked separately by the scheduler and validation; do not turn planned follow-up work into a current-node rejection."
            .to_string(),
    );

    lines.join("\n")
}

fn render_list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}

pub(super) fn render_test_plan_context(context: &TestPlanContext) -> String {
    let required = context.required_validation_targets.join(", ");
    let planned_set = context
        .planned_test_targets
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let covered = context
        .required_validation_targets
        .iter()
        .filter(|target| planned_set.contains(target.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let missing = context
        .required_validation_targets
        .iter()
        .filter(|target| !planned_set.contains(target.as_str()))
        .cloned()
        .collect::<Vec<_>>();

    let mut lines = vec![
        "Test target plan context (built from structured target/dependency metadata):".to_string(),
        "Review scope: judge the current node's declared deliverables, while accounting for declared follow-up work.".to_string(),
        format!("Adapter-required test targets for the current node's target files: {required}"),
    ];
    if context.planned_test_targets.is_empty() {
        lines.push("Declared dependent/follow-up target files: none".to_string());
    } else {
        lines.push(format!(
            "Declared dependent/follow-up target files: {}",
            context.planned_test_targets.join(", ")
        ));
    }
    if missing.is_empty() {
        lines.push(format!(
            "Adapter-required test targets covered by declared follow-up work: {}",
            covered.join(", ")
        ));
        lines.push(
            "Acceptance is allowed for a correct source-only current node even though those planned test files do not exist yet."
                .to_string(),
        );
        lines.push(
            "Do not reject this current node solely because covered tests are planned separately."
                .to_string(),
        );
    } else {
        lines.push(format!(
            "Adapter-required test targets not covered by declared follow-up work: {}",
            missing.join(", ")
        ));
        lines.push(
            "If this current node changes code and no declared follow-up covers these tests, missing tests remain a valid rejection."
                .to_string(),
        );
    }
    lines.join("\n")
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
