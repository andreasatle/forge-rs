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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::scheduler::FailureKind;

    fn parse_role_response(raw_response: &str) -> RoleResult {
        RoleResponseParser::new(raw_response)
            .parse_role_response()
            .unwrap_or_else(|reason| RoleResult::Failed {
                kind: FailureKind::ProtocolFailure,
                reason,
            })
    }

    fn role_json(status: &str, field: &str, value: &str) -> String {
        serde_json::json!({ "status": status, field: value }).to_string()
    }

    fn accepted_json(content: &str) -> String {
        role_json("accepted", "content", content)
    }

    fn rejected_json(reason: &str) -> String {
        role_json("rejected", "reason", reason)
    }

    fn assert_parse_failed(case: &str, input: &str) -> String {
        let result = parse_role_response(input);
        let RoleResult::Failed { reason, .. } = result else {
            panic!("[{case}] response must produce Failed, got {result:?}");
        };
        reason
    }

    fn assert_parse_failed_reason_contains(case: &str, input: &str, expected: &str) -> String {
        let reason = assert_parse_failed(case, input);
        assert!(
            reason.contains(expected),
            "[{case}] failure reason must mention '{expected}'; got: {reason}"
        );
        reason
    }

    fn assert_placeholder_rejected(case: &str, input: &str) -> String {
        assert_parse_failed_reason_contains(case, input, "placeholder")
    }

    fn assert_too_short(case: &str, input: &str) {
        assert_parse_failed_reason_contains(case, input, "too short");
    }

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
    fn empty_role_response_field_fails() {
        for (case, input) in [
            ("accepted/content", accepted_json("")),
            ("rejected/reason", rejected_json("")),
        ] {
            assert_parse_failed(case, &input);
        }
    }

    #[test]
    fn dot_placeholder_fails_without_including_raw() {
        for (case, input) in [
            ("accepted/content", accepted_json("...")),
            ("rejected/reason", rejected_json("...")),
        ] {
            let reason = assert_placeholder_rejected(case, &input);
            assert!(
                !reason.contains("raw:"),
                "[{case}] failure reason must not include 'raw:'; got: {reason}"
            );
        }
    }

    #[test]
    fn framework_placeholders_are_rejected() {
        for (case, input) in [
            (
                "accepted/response-summary",
                accepted_json("$RESPONSE_SUMMARY"),
            ),
            (
                "rejected/reason-for-rejection",
                rejected_json("$REASON_FOR_REJECTION"),
            ),
            (
                "rejected/min-length-dollar-reason",
                rejected_json("$REASON"),
            ),
        ] {
            assert_parse_failed_reason_contains(case, &input, "framework placeholder");
        }
    }

    #[test]
    fn too_short_role_response_field_fails() {
        for (case, input) in [
            ("accepted/1-char", accepted_json("{")),
            ("accepted/2-char", accepted_json("ok")),
            ("rejected/1-char", rejected_json("{")),
            ("rejected/2-char", rejected_json("ok")),
        ] {
            assert_too_short(case, &input);
        }
    }

    #[test]
    fn meaningful_accepted_content_passes() {
        for (case, content) in [
            (
                "prose summary",
                "Created src/main.rs with a Rust program that prints a haiku.",
            ),
            ("angle brackets", "<p>hello world</p>"),
            ("html-like", "<html><body>ok</body></html>"),
            ("xml-like", "<root><item>data</item></root>"),
            ("normal summary", "Summary of changes made to the file."),
        ] {
            let result = parse_role_response(&accepted_json(content));
            assert!(
                matches!(result, RoleResult::Accepted { .. }),
                "[{case}] sufficiently long, non-placeholder content must be accepted, got {result:?}"
            );
        }
    }

    #[test]
    fn min_length_boundary_fields_pass() {
        let value = "a".repeat(MIN_CONTENT_LENGTH);
        for (case, input, expected_status) in [
            ("accepted/content", accepted_json(&value), "accepted"),
            ("rejected/reason", rejected_json(&value), "rejected"),
        ] {
            let result = parse_role_response(&input);
            let matches_expected = match expected_status {
                "accepted" => matches!(&result, RoleResult::Accepted { .. }),
                "rejected" => matches!(&result, RoleResult::Rejected { .. }),
                _ => unreachable!("unknown expected status"),
            };
            assert!(
                matches_expected,
                "[{case}] field at exactly MIN_CONTENT_LENGTH must be accepted, got {result:?}"
            );
        }
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

    #[test]
    fn accepted_role_response_with_trailing_whitespace_parses() {
        for (name, input, is_accepted) in [
            (
                "trailing newline",
                "{\"status\":\"accepted\",\"content\":\"The task was completed successfully.\"}\n",
                true,
            ),
            (
                "trailing spaces and tabs",
                "{\"status\":\"accepted\",\"content\":\"The task was completed successfully.\"}  \t  ",
                true,
            ),
            (
                "trailing whitespace, rejected status",
                "{\"status\":\"rejected\",\"reason\":\"The output does not meet requirements.\"}\n\n",
                false,
            ),
            (
                "leading and trailing whitespace",
                "\n  {\"status\":\"accepted\",\"content\":\"The task was completed successfully.\"}  \n",
                true,
            ),
        ] {
            let result = parse_role_response(input);
            let matches_expected = if is_accepted {
                matches!(result, RoleResult::Accepted { .. })
            } else {
                matches!(result, RoleResult::Rejected { .. })
            };
            assert!(
                matches_expected,
                "{name}: surrounding whitespace must not cause role response parse failure, got {result:?}"
            );
        }
    }
}
