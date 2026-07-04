//! Flat chronological trace view (`--summary`) and the `--prompts`/
//! `--failures` full-content views.

use super::reader::{EventHeader, short_id};

/// Print a one-line summary of every telemetry file in `paths`, in order.
pub(super) fn print_summary(paths: &[std::path::PathBuf]) -> std::io::Result<()> {
    for path in paths {
        let content = std::fs::read_to_string(path)?;
        let Some(header) = EventHeader::parse(path, &content) else {
            continue;
        };
        println!("{}", summary_line(&header));
    }
    Ok(())
}

/// Print full prompt bodies for every `RolePromptRendered` event in `paths`.
pub(super) fn print_prompts(paths: &[std::path::PathBuf]) -> std::io::Result<()> {
    for path in paths {
        let content = std::fs::read_to_string(path)?;
        let Some(header) = EventHeader::parse(path, &content) else {
            continue;
        };
        if header.kind == "RolePromptRendered" {
            print_prompt(&header, &content);
        }
    }
    Ok(())
}

/// Print full content for every failure-related event in `paths`.
pub(super) fn print_failures(paths: &[std::path::PathBuf]) -> std::io::Result<()> {
    for path in paths {
        let content = std::fs::read_to_string(path)?;
        let Some(header) = EventHeader::parse(path, &content) else {
            continue;
        };
        if is_failure_kind(&header.kind) {
            print_full(&header, &content);
        }
    }
    Ok(())
}

/// One line of the flat summary: the header, `node=`/`attempt=` context when
/// present, and a short preview of the event's first field.
pub(super) fn summary_line(header: &EventHeader) -> String {
    let mut line = header.to_string();
    if let Some(node_id) = &header.node_id {
        line.push_str(&format!("  node={}", short_id(node_id)));
    }
    if let Some(attempt) = &header.attempt {
        line.push_str(&format!("  attempt={attempt}"));
    }
    if let Some(preview) = &header.preview {
        line.push_str(&format!("  {preview}"));
    }
    line
}

pub(super) fn is_failure_kind(kind: &str) -> bool {
    matches!(
        kind,
        "Failure" | "FailureClassified" | "ValidationFailed" | "ParseFailed"
    )
}

/// Width of the `=`-rule printed above and below each record's header line.
const BANNER_WIDTH: usize = 64;

fn print_banner(header: &EventHeader) {
    let rule = "=".repeat(BANNER_WIDTH);
    println!("{rule}");
    println!("{header}");
    println!("{rule}");
}

const PROMPT_MARKER: &str = "\nprompt:\n";

fn print_prompt(header: &EventHeader, content: &str) {
    print_banner(header);
    println!("{}", prompt_body(content));
    println!();
}

/// Extract the prompt text verbatim, dropping only the preceding
/// `source:`/`kind:`/`attempt_count:`/`prompt:` field lines.
pub(super) fn prompt_body(content: &str) -> &str {
    match content.find(PROMPT_MARKER) {
        Some(idx) => content[idx + PROMPT_MARKER.len()..].trim_end_matches('\n'),
        None => content.trim_end_matches('\n'),
    }
}

const RAW_RESPONSE_MARKER: &str = "\nraw_response:\n";

fn print_full(header: &EventHeader, content: &str) {
    print_banner(header);
    println!("{}", failure_body(content));
    println!();
}

/// Render a failure record's content, converting a `raw_response` payload to
/// YAML when it parses as JSON. All other fields are left untouched.
pub(super) fn failure_body(content: &str) -> String {
    match content.find(RAW_RESPONSE_MARKER) {
        Some(idx) => {
            let split_at = idx + RAW_RESPONSE_MARKER.len();
            let before = &content[..split_at];
            let raw_response = content[split_at..].trim_end_matches('\n');
            match json_to_yaml(raw_response) {
                Some(yaml) => format!("{before}{yaml}"),
                None => format!("{before}{raw_response}"),
            }
        }
        None => content.trim_end_matches('\n').to_string(),
    }
}

/// Parse `text` as JSON and re-render it as YAML, or `None` if it isn't
/// valid JSON.
pub(super) fn json_to_yaml(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    let yaml = serde_yaml::to_string(&value).ok()?;
    Some(yaml.trim_end().to_string())
}
