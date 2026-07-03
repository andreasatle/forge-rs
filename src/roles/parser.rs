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

pub(super) struct RoleResponseParser<'a> {
    raw_response: &'a str,
    text: &'a str,
}

impl<'a> RoleResponseParser<'a> {
    pub(super) fn new(raw_response: &'a str) -> Self {
        let raw_response = raw_response.trim();
        Self {
            raw_response,
            text: Self::strip_code_fence(raw_response),
        }
    }

    pub(super) fn text(&self) -> &'a str {
        self.text
    }

    pub(super) fn parse_role_response(&self) -> Result<RoleResult, String> {
        let json_str = self.json_object()?;
        let result = match serde_json::from_str::<JsonRoleResponse>(json_str) {
            Ok(JsonRoleResponse::Accepted { content }) => Self::accepted_content_result(content)?,
            Ok(JsonRoleResponse::Rejected { reason }) => Self::rejected_reason_result(reason)?,
            Err(err) => return Err(format!("JSON parse error: {err}")),
        };
        Ok(result)
    }

    /// Parses the Work-node Producer's `{"summary": "..."}` response.
    ///
    /// The Producer never rejects: a valid, non-placeholder `summary` always maps
    /// to [`RoleResult::Accepted`]. Critic and Referee responses go through
    /// [`RoleResponseParser::parse_role_response`] instead — see [`WORK_PRODUCER_SYSTEM`].
    ///
    /// [`WORK_PRODUCER_SYSTEM`]: crate::roles::policy::WORK_PRODUCER_SYSTEM
    pub(super) fn parse_producer_summary_response(&self) -> Result<RoleResult, String> {
        let json_str = self.json_object()?;
        match serde_json::from_str::<ProducerSummaryResponse>(json_str) {
            Ok(ProducerSummaryResponse { summary }) => Self::producer_summary_result(summary),
            Err(err) => Err(format!("JSON parse error: {err}")),
        }
    }

    fn json_object(&self) -> Result<&'a str, String> {
        if !self.text.starts_with('{') {
            return Err(
                "role response must start with a JSON object; preamble text is not permitted"
                    .to_string(),
            );
        }
        extract_json_object(self.text).ok_or_else(|| {
            let _ = self.raw_response;
            "no JSON object found in role response".to_string()
        })
    }

    fn accepted_content_result(content: String) -> Result<RoleResult, String> {
        match Self::validate_meaningful_field(&content)? {
            MeaningfulField::Empty => Err("accepted response has empty content".to_string()),
            MeaningfulField::DotPlaceholder => {
                Err("role response has placeholder accepted content".to_string())
            }
            MeaningfulField::TooShort(len) => Err(format!(
                "accepted content is too short to be a meaningful summary ({len} chars)"
            )),
            MeaningfulField::Valid => Ok(RoleResult::Accepted { content }),
        }
    }

    fn rejected_reason_result(reason: String) -> Result<RoleResult, String> {
        match Self::validate_meaningful_field(&reason)? {
            MeaningfulField::Empty | MeaningfulField::DotPlaceholder => {
                Err("role response has placeholder reason".to_string())
            }
            MeaningfulField::TooShort(len) => Err(format!(
                "rejection reason is too short to be meaningful ({len} chars)"
            )),
            MeaningfulField::Valid => Ok(RoleResult::Rejected { reason }),
        }
    }

    fn producer_summary_result(summary: String) -> Result<RoleResult, String> {
        match Self::validate_meaningful_field(&summary)? {
            MeaningfulField::Empty => Err("summary response has empty content".to_string()),
            MeaningfulField::DotPlaceholder => {
                Err("role response has placeholder summary".to_string())
            }
            MeaningfulField::TooShort(len) => Err(format!(
                "summary is too short to be a meaningful summary ({len} chars)"
            )),
            MeaningfulField::Valid => Ok(RoleResult::Accepted { content: summary }),
        }
    }

    fn validate_meaningful_field(value: &str) -> Result<MeaningfulField, String> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Ok(MeaningfulField::Empty);
        } else if trimmed == "..." {
            return Ok(MeaningfulField::DotPlaceholder);
        } else if Self::is_framework_placeholder(value) {
            return Err(format!(
                "role response returned framework placeholder: {value}"
            ));
        } else if trimmed.len() < MIN_CONTENT_LENGTH {
            return Ok(MeaningfulField::TooShort(trimmed.len()));
        }
        Ok(MeaningfulField::Valid)
    }

    fn is_framework_placeholder(value: &str) -> bool {
        let s = value.trim();
        s.starts_with('$')
            && s.len() > 1
            && s[1..].bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
    }

    fn strip_code_fence(s: &'a str) -> &'a str {
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
}

enum MeaningfulField {
    Empty,
    DotPlaceholder,
    TooShort(usize),
    Valid,
}
