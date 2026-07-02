//! Role response parsing.

use serde::Deserialize;

use crate::roles::runner::RoleResult;
use crate::services::extract_json_object;

pub(super) const MIN_CONTENT_LENGTH: usize = 8;

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum JsonRoleResponse {
    Accepted { content: String },
    Rejected { reason: String },
}

/// The Work-node Producer's response schema: a single `summary` field. The
/// Producer cannot reject, so there is no `status` tag to discriminate.
#[derive(Deserialize)]
struct ProducerSummaryResponse {
    summary: String,
}

pub(super) fn is_framework_placeholder(value: &str) -> bool {
    let s = value.trim();
    s.starts_with('$') && s.len() > 1 && s[1..].bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
}

pub(super) fn try_parse_role_response(raw_response: &str) -> Result<RoleResult, String> {
    let text = strip_code_fence(raw_response.trim());
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

/// Parses the Work-node Producer's `{"summary": "..."}` response.
///
/// The Producer never rejects: a valid, non-placeholder `summary` always maps
/// to [`RoleResult::Accepted`]. Critic and Referee responses go through
/// [`try_parse_role_response`] instead — see [`WORK_PRODUCER_SYSTEM`].
///
/// [`WORK_PRODUCER_SYSTEM`]: crate::roles::policy::WORK_PRODUCER_SYSTEM
pub(super) fn try_parse_producer_summary_response(
    raw_response: &str,
) -> Result<RoleResult, String> {
    let text = strip_code_fence(raw_response.trim());
    if !text.starts_with('{') {
        return Err(
            "role response must start with a JSON object; preamble text is not permitted"
                .to_string(),
        );
    }
    let json_str = match extract_json_object(text) {
        Some(s) => s,
        None => return Err("no JSON object found in role response".to_string()),
    };
    match serde_json::from_str::<ProducerSummaryResponse>(json_str) {
        Ok(ProducerSummaryResponse { summary }) => {
            let trimmed = summary.trim();
            if trimmed.is_empty() {
                Err("summary response has empty content".to_string())
            } else if trimmed == "..." {
                Err("role response has placeholder summary".to_string())
            } else if is_framework_placeholder(&summary) {
                Err(format!(
                    "role response returned framework placeholder: {summary}"
                ))
            } else if trimmed.len() < MIN_CONTENT_LENGTH {
                Err(format!(
                    "summary is too short to be a meaningful summary ({} chars)",
                    trimmed.len()
                ))
            } else {
                Ok(RoleResult::Accepted { content: summary })
            }
        }
        Err(err) => Err(format!("JSON parse error: {err}")),
    }
}

pub(super) fn strip_code_fence(s: &str) -> &str {
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
