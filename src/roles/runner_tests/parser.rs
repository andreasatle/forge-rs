use super::*;

fn parse_role_response(raw_response: &str) -> RoleResult {
    crate::roles::parser::try_parse_role_response(raw_response).unwrap_or_else(|reason| {
        RoleResult::Failed {
            kind: FailureKind::ProtocolFailure,
            reason,
        }
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

// --- fake providers ---

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

// --- minimum-length guard tests ---

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
    let result = parse_role_response(
        r#"{"status":"accepted","content":"Created src/main.rs with a Rust program that prints a haiku."}"#,
    );
    assert!(
        matches!(result, RoleResult::Accepted { .. }),
        "long meaningful content must be accepted, got {result:?}"
    );
}

#[test]
fn min_length_boundary_fields_pass() {
    // Exactly MIN_CONTENT_LENGTH characters must be accepted.
    let value = "a".repeat(super::MIN_CONTENT_LENGTH);
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
fn arbitrary_angle_bracket_text_is_allowed() {
    let result = parse_role_response(r#"{"status":"accepted","content":"<p>hello world</p>"}"#);
    assert!(
        matches!(result, RoleResult::Accepted { .. }),
        "arbitrary angle-bracket content must be accepted, got {result:?}"
    );
}

#[test]
fn html_like_content_is_allowed() {
    let result =
        parse_role_response(r#"{"status":"accepted","content":"<html><body>ok</body></html>"}"#);
    assert!(
        matches!(result, RoleResult::Accepted { .. }),
        "HTML-like content must be accepted, got {result:?}"
    );
}

#[test]
fn xml_like_content_is_allowed() {
    let result =
        parse_role_response(r#"{"status":"accepted","content":"<root><item>data</item></root>"}"#);
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
    let input = "{\"status\":\"accepted\",\"content\":\"The task was completed successfully.\"}\n";
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
    let input =
        "\n  {\"status\":\"accepted\",\"content\":\"The task was completed successfully.\"}  \n";
    let result = parse_role_response(input);
    assert!(
        matches!(result, RoleResult::Accepted { .. }),
        "leading and trailing whitespace must not prevent parsing, got {result:?}"
    );
}

// --- ProviderRoleRunner tests ---

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
