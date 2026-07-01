//! Telemetry trace viewer.
//!
//! Reads the plain-text files written by
//! [`FileTelemetry`](crate::telemetry::FileTelemetry) under a run's telemetry
//! directory and prints them to stdout, either as a chronological one-line
//! summary or as full event content filtered by kind. Each printed record is
//! set off with a banner so consecutive prompts or failures are easy to tell
//! apart.
//!
//! Prompt bodies are always printed verbatim. Failure content is printed
//! as-is too, except that a `raw_response` payload — the model's raw JSON
//! reply — is rendered as YAML when it parses as JSON, since that's
//! considerably easier to read than a single-line JSON blob.

use std::error::Error;
use std::path::Path;

use super::run_info::latest_run_dir;

/// Which telemetry events [`run_trace`] should print.
#[derive(Clone, Copy)]
pub enum TraceFilter {
    /// Print a one-line summary of every event.
    All,
    /// Print only `RolePromptRendered` events, with the full prompt body.
    Prompts,
    /// Print only failure-related events, with their full content.
    Failures,
}

/// Print telemetry events for one run under `runs_root` according to `filter`.
///
/// `runs_root` is the directory configured as `telemetry.directory` in the
/// forge config — it holds one subdirectory per run plus a `latest` pointer
/// to the most recently created one. When `run` is `None` the latest run is
/// used; otherwise `run` must name a run directory directly under
/// `runs_root` (the same id printed as `Run ID` in the run summary).
///
/// Files within the resolved run's `telemetry/` directory are visited in
/// filename order, which matches emission order because `FileTelemetry`
/// prefixes every filename with a zero-padded, incrementing counter.
pub fn run_trace(
    runs_root: &str,
    run: Option<&str>,
    filter: TraceFilter,
) -> Result<(), Box<dyn Error>> {
    let runs_root = Path::new(runs_root);
    let run_dir = match run {
        Some(id) => runs_root.join(id),
        None => latest_run_dir(runs_root)?,
    };
    let telemetry_dir = run_dir.join("telemetry");

    let mut paths: Vec<_> = std::fs::read_dir(&telemetry_dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("txt"))
        .collect();
    paths.sort();

    for path in &paths {
        let content = std::fs::read_to_string(path)?;
        let Some(header) = EventHeader::parse(path, &content) else {
            continue;
        };

        match filter {
            TraceFilter::All => println!("{}", summary_line(&header)),
            TraceFilter::Prompts => {
                if header.kind == "RolePromptRendered" {
                    print_prompt(&header, &content);
                }
            }
            TraceFilter::Failures => {
                if is_failure_kind(&header.kind) {
                    print_full(&header, &content);
                }
            }
        }
    }

    Ok(())
}

/// Counter, source, optional subsource/node context, and kind parsed from one
/// telemetry file.
struct EventHeader {
    counter: String,
    source: String,
    subsource: Option<String>,
    /// Scheduler node id, present only on events that pertain to a single node.
    node_id: Option<String>,
    /// Zero-based node attempt number, present only alongside `node_id`.
    attempt: Option<String>,
    kind: String,
    /// The first field line after `kind:`, truncated for display in the
    /// default summary. `None` when the event has no further fields.
    preview: Option<String>,
}

impl EventHeader {
    /// Parse the counter from `path`'s filename and the
    /// source/subsource/node_id/attempt/kind from the leading header lines of
    /// `content`.
    fn parse(path: &Path, content: &str) -> Option<Self> {
        let counter = path.file_stem()?.to_str()?.split("--").next()?.to_string();

        let mut lines = content.lines();
        let source = lines.next()?.strip_prefix("source: ")?.to_string();

        let mut line = lines.next()?;
        let mut subsource = None;
        if let Some(sub) = line.strip_prefix("subsource: ") {
            subsource = Some(sub.to_string());
            line = lines.next()?;
        }
        let mut node_id = None;
        if let Some(id) = line.strip_prefix("node_id: ") {
            node_id = Some(id.to_string());
            line = lines.next()?;
        }
        let mut attempt = None;
        if let Some(a) = line.strip_prefix("attempt: ") {
            attempt = Some(a.to_string());
            line = lines.next()?;
        }
        let kind = line.strip_prefix("kind: ")?.to_string();

        // Skip fields that add nothing beyond what's already shown: a
        // `machine: <name>` field always repeats `source` verbatim (see
        // `run_machine_with_telemetry`), and a bare `field:` marker (e.g.
        // `state:`) introduces a multi-line value with no inline text of its
        // own, so its first content line is used instead.
        let mut preview_line = lines.next();
        loop {
            match preview_line {
                Some(line) if line.starts_with("machine: ") => preview_line = lines.next(),
                Some(line) if line.trim_end().ends_with(':') => preview_line = lines.next(),
                _ => break,
            }
        }
        let preview = preview_line.and_then(|line| {
            let line = strip_trailing_open_bracket(line.trim());
            if line.is_empty() {
                None
            } else {
                Some(truncate(line, PREVIEW_MAX_CHARS))
            }
        });

        Some(Self {
            counter,
            source,
            subsource,
            node_id,
            attempt,
            kind,
            preview,
        })
    }
}

impl std::fmt::Display for EventHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.subsource {
            Some(sub) => write!(
                f,
                "{}  {}/{}  {}",
                self.counter, self.source, sub, self.kind
            ),
            None => write!(f, "{}  {}  {}", self.counter, self.source, self.kind),
        }
    }
}

/// Maximum length, in characters, of the preview snippet shown in the
/// default trace summary.
const PREVIEW_MAX_CHARS: usize = 80;

/// One line of the default trace summary: the header, `node=`/`attempt=`
/// context when present, and a short preview of the event's first field.
fn summary_line(header: &EventHeader) -> String {
    let mut line = header.to_string();
    if let Some(node_id) = &header.node_id {
        line.push_str(&format!("  node={node_id}"));
    }
    if let Some(attempt) = &header.attempt {
        line.push_str(&format!("  attempt={attempt}"));
    }
    if let Some(preview) = &header.preview {
        line.push_str(&format!("  {preview}"));
    }
    line
}

/// Drop a trailing, unmatched opening bracket left over from the first line
/// of a pretty-printed (`{:#?}`) struct or tuple variant, e.g. `Active {`
/// becomes `Active` and `WorkAccepted(` becomes `WorkAccepted`. The bracket
/// never closes on the same line, so it adds nothing but visual noise.
fn strip_trailing_open_bracket(line: &str) -> &str {
    let stripped = line
        .strip_suffix('{')
        .or_else(|| line.strip_suffix('('))
        .or_else(|| line.strip_suffix('['))
        .unwrap_or(line);
    stripped.trim_end()
}

/// Truncate `s` to at most `max_chars` characters, appending an ellipsis
/// when truncated.
fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut truncated: String = s.chars().take(max_chars).collect();
    truncated.push('…');
    truncated
}

fn is_failure_kind(kind: &str) -> bool {
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
fn prompt_body(content: &str) -> &str {
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
fn failure_body(content: &str) -> String {
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
fn json_to_yaml(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    let yaml = serde_yaml::to_string(&value).ok()?;
    Some(yaml.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::{FileTelemetry, TelemetryEvent, TelemetryRecord, TelemetrySink};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "forge-trace-test-{label}-{}-{seq}",
            std::process::id()
        ))
    }

    /// Create `<runs_root>/<run_id>/telemetry/` and return the telemetry dir.
    fn make_run_telemetry_dir(runs_root: &Path, run_id: &str) -> PathBuf {
        let telemetry_dir = runs_root.join(run_id).join("telemetry");
        std::fs::create_dir_all(&telemetry_dir).unwrap();
        telemetry_dir
    }

    /// Point `<runs_root>/latest` at `run_id`, mirroring `run_info::update_latest`.
    fn set_latest(runs_root: &Path, run_id: &str) {
        let latest = runs_root.join("latest");
        let _ = std::fs::remove_file(&latest);
        #[cfg(unix)]
        std::os::unix::fs::symlink(run_id, &latest).unwrap();
        #[cfg(not(unix))]
        std::fs::write(&latest, run_id).unwrap();
    }

    #[test]
    fn missing_runs_root_returns_error() {
        let dir = temp_dir("missing-root");
        let result = run_trace(dir.to_str().unwrap(), None, TraceFilter::All);
        assert!(
            result.is_err(),
            "trace over a nonexistent runs root must return an error"
        );
    }

    #[test]
    fn missing_latest_pointer_returns_error() {
        let dir = temp_dir("missing-latest");
        std::fs::create_dir_all(&dir).unwrap();
        let result = run_trace(dir.to_str().unwrap(), None, TraceFilter::All);
        assert!(
            result.is_err(),
            "trace with no `latest` pointer and no runs must return an error"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_run_resolves_to_latest() {
        let root = temp_dir("default-latest");
        let telemetry_dir = make_run_telemetry_dir(&root, "run-a");
        let sink = FileTelemetry::new(telemetry_dir);
        sink.record(TelemetryRecord::new(
            "SchedulerMachine",
            TelemetryEvent::MachineStarted {
                machine: "SchedulerHandler".into(),
            },
        ));
        set_latest(&root, "run-a");

        let result = run_trace(root.to_str().unwrap(), None, TraceFilter::All);
        assert!(
            result.is_ok(),
            "trace with no --run must resolve the `latest` pointer and succeed"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn explicit_run_bypasses_latest() {
        let root = temp_dir("explicit-run");
        let telemetry_dir = make_run_telemetry_dir(&root, "run-b");
        let sink = FileTelemetry::new(telemetry_dir);
        sink.record(TelemetryRecord::new(
            "SchedulerMachine",
            TelemetryEvent::MachineStarted {
                machine: "SchedulerHandler".into(),
            },
        ));
        // No `latest` pointer is set: an explicit --run must not need it.

        let result = run_trace(root.to_str().unwrap(), Some("run-b"), TraceFilter::All);
        assert!(
            result.is_ok(),
            "trace with an explicit --run must not require a `latest` pointer"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unknown_explicit_run_returns_error() {
        let root = temp_dir("unknown-run");
        std::fs::create_dir_all(&root).unwrap();

        let result = run_trace(
            root.to_str().unwrap(),
            Some("no-such-run"),
            TraceFilter::All,
        );
        assert!(
            result.is_err(),
            "trace with a --run that does not exist under runs_root must fail"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn event_header_parses_source_and_kind_without_subsource() {
        let dir = temp_dir("header-no-sub");
        let sink = FileTelemetry::new(dir.clone());
        sink.record(TelemetryRecord::new(
            "SchedulerMachine",
            TelemetryEvent::MachineStarted {
                machine: "SchedulerHandler".into(),
            },
        ));
        let path = dir.join("000001--scheduler-machine--machine-started.txt");
        let content = std::fs::read_to_string(&path).unwrap();

        let header = EventHeader::parse(&path, &content).expect("header must parse");
        assert_eq!(header.counter, "000001");
        assert_eq!(header.source, "SchedulerMachine");
        assert_eq!(header.subsource, None);
        assert_eq!(header.kind, "MachineStarted");
        // MachineStarted's only field is `machine:`, which always repeats
        // `source`; skipping it leaves no further field to preview.
        assert_eq!(header.preview, None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn event_header_parses_subsource_when_present() {
        let dir = temp_dir("header-with-sub");
        let sink = FileTelemetry::new(dir.clone());
        sink.record(TelemetryRecord::new_with_subsource(
            "RoleMachine",
            "Producer",
            TelemetryEvent::ParseSucceeded { attempt_count: 1 },
        ));
        let path = dir.join("000001--role-machine--producer--parse-succeeded.txt");
        let content = std::fs::read_to_string(&path).unwrap();

        let header = EventHeader::parse(&path, &content).expect("header must parse");
        assert_eq!(header.source, "RoleMachine");
        assert_eq!(header.subsource, Some("Producer".to_string()));
        assert_eq!(header.kind, "ParseSucceeded");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn event_header_parses_node_id_and_attempt_when_present() {
        let dir = temp_dir("header-with-node-context");
        let sink = FileTelemetry::new(dir.clone());
        let mut record = TelemetryRecord::new(
            "DeliberationMachine",
            TelemetryEvent::StateEntered {
                machine: "DeliberationMachine".into(),
                state: "Ready".into(),
            },
        );
        record.node_id = Some("root-child-0".into());
        record.attempt = Some(1);
        sink.record(record);
        let path = dir.join("000001--deliberation-machine--state-entered.txt");
        let content = std::fs::read_to_string(&path).unwrap();

        let header = EventHeader::parse(&path, &content).expect("header must parse");
        assert_eq!(header.node_id.as_deref(), Some("root-child-0"));
        assert_eq!(header.attempt.as_deref(), Some("1"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn event_header_leaves_node_id_and_attempt_none_when_absent() {
        let dir = temp_dir("header-without-node-context");
        let sink = FileTelemetry::new(dir.clone());
        sink.record(TelemetryRecord::new(
            "SchedulerMachine",
            TelemetryEvent::MachineStarted {
                machine: "SchedulerHandler".into(),
            },
        ));
        let path = dir.join("000001--scheduler-machine--machine-started.txt");
        let content = std::fs::read_to_string(&path).unwrap();

        let header = EventHeader::parse(&path, &content).expect("header must parse");
        assert_eq!(header.node_id, None);
        assert_eq!(header.attempt, None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summary_line_includes_node_and_attempt_when_present() {
        let header = EventHeader {
            counter: "000012".to_string(),
            source: "DeliberationMachine".to_string(),
            subsource: None,
            node_id: Some("root-child-0".to_string()),
            attempt: Some("1".to_string()),
            kind: "StateEntered".to_string(),
            preview: None,
        };
        assert_eq!(
            summary_line(&header),
            "000012  DeliberationMachine  StateEntered  node=root-child-0  attempt=1"
        );
    }

    #[test]
    fn event_header_preview_skips_redundant_machine_field_and_bare_marker() {
        let dir = temp_dir("header-multiline-marker");
        let sink = FileTelemetry::new(dir.clone());
        sink.record(TelemetryRecord::new(
            "SchedulerMachine",
            TelemetryEvent::StateEntered {
                machine: "SchedulerMachine".into(),
                state: "Active { graph: .. }".into(),
            },
        ));
        let path = dir.join("000001--scheduler-machine--state-entered.txt");
        let content = std::fs::read_to_string(&path).unwrap();

        let header = EventHeader::parse(&path, &content).expect("header must parse");
        assert_eq!(
            header.preview.as_deref(),
            Some("Active { graph: .. }"),
            "preview must skip the redundant `machine:` field (it always repeats `source`) \
             and the bare `state:` marker, landing on the actual state text"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn event_header_preview_uses_first_non_redundant_single_line_field() {
        // Failure has no `machine:` field, so its first field (`component:`)
        // is used directly.
        let content = "source: SchedulerMachine\nkind: Failure\ncomponent: Worker\nreason: boom\n";
        let path = Path::new("000001--scheduler-machine--failure.txt");

        let header = EventHeader::parse(path, content).expect("header must parse");
        assert_eq!(header.preview.as_deref(), Some("component: Worker"));
    }

    #[test]
    fn event_header_preview_skips_bare_multiline_marker() {
        // Synthetic content where a bare `state:` marker (no inline value,
        // and no preceding `machine:` field) is the first line after `kind:`.
        let content =
            "source: SchedulerMachine\nkind: StateEntered\nstate:\nActive { graph: .. }\n";
        let path = Path::new("000001--scheduler-machine--state-entered.txt");

        let header = EventHeader::parse(path, content).expect("header must parse");
        assert_eq!(
            header.preview.as_deref(),
            Some("Active { graph: .. }"),
            "a bare multi-line marker must be skipped in favor of its first content line"
        );
    }

    #[test]
    fn event_header_preview_is_none_when_no_further_fields() {
        let dir = temp_dir("header-no-preview");
        let sink = FileTelemetry::new(dir.clone());
        sink.record(TelemetryRecord::new(
            "RoleMachine",
            TelemetryEvent::ToolLoopLimitReached,
        ));
        let path = dir.join("000001--role-machine--tool-loop-limit-reached.txt");
        let content = std::fs::read_to_string(&path).unwrap();

        let header = EventHeader::parse(&path, &content).expect("header must parse");
        assert_eq!(
            header.preview, None,
            "an event with no fields after `kind:` must have no preview"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preview_strips_dangling_brace_from_pretty_printed_struct_variant() {
        // A real `{:#?}` dump of a struct-like enum variant puts only the
        // variant name and opening brace on the first line.
        let content = "source: SchedulerMachine\nkind: StateEntered\nstate:\n\
            Active {\n    graph: RunGraph {\n        nodes: [],\n    },\n}\n";
        let path = Path::new("000001--scheduler-machine--state-entered.txt");

        let header = EventHeader::parse(path, content).expect("header must parse");
        assert_eq!(
            header.preview.as_deref(),
            Some("Active"),
            "the dangling `{{` left by pretty-printed Debug output must be stripped"
        );
    }

    #[test]
    fn preview_strips_dangling_paren_from_pretty_printed_tuple_variant() {
        let content = "source: SchedulerMachine\nkind: EventReceived\nevent:\n\
            WorkAccepted(\n    NodeId(\n        \"root\",\n    ),\n)\n";
        let path = Path::new("000001--scheduler-machine--event-received.txt");

        let header = EventHeader::parse(path, content).expect("header must parse");
        assert_eq!(header.preview.as_deref(), Some("WorkAccepted"));
    }

    #[test]
    fn strip_trailing_open_bracket_removes_brace_and_trailing_space() {
        assert_eq!(strip_trailing_open_bracket("Active {"), "Active");
    }

    #[test]
    fn strip_trailing_open_bracket_removes_paren() {
        assert_eq!(strip_trailing_open_bracket("WorkAccepted("), "WorkAccepted");
    }

    #[test]
    fn strip_trailing_open_bracket_leaves_complete_line_unchanged() {
        assert_eq!(
            strip_trailing_open_bracket("component: Worker"),
            "component: Worker"
        );
    }

    #[test]
    fn strip_trailing_open_bracket_returns_empty_for_bare_bracket() {
        assert_eq!(strip_trailing_open_bracket("{"), "");
    }

    #[test]
    fn truncate_leaves_short_strings_unchanged() {
        assert_eq!(truncate("short", 80), "short");
    }

    #[test]
    fn truncate_shortens_long_strings_with_ellipsis() {
        let long = "a".repeat(100);
        let truncated = truncate(&long, 10);
        assert_eq!(truncated, format!("{}…", "a".repeat(10)));
    }

    #[test]
    fn summary_line_appends_preview_when_present() {
        let header = EventHeader {
            counter: "000001".to_string(),
            source: "SchedulerMachine".to_string(),
            subsource: None,
            node_id: None,
            attempt: None,
            kind: "MachineStarted".to_string(),
            preview: Some("machine: SchedulerHandler".to_string()),
        };
        assert_eq!(
            summary_line(&header),
            "000001  SchedulerMachine  MachineStarted  machine: SchedulerHandler"
        );
    }

    #[test]
    fn summary_line_omits_trailing_separator_without_preview() {
        let header = EventHeader {
            counter: "000001".to_string(),
            source: "SchedulerMachine".to_string(),
            subsource: None,
            node_id: None,
            attempt: None,
            kind: "ValidationStarted".to_string(),
            preview: None,
        };
        assert_eq!(
            summary_line(&header),
            "000001  SchedulerMachine  ValidationStarted"
        );
    }

    #[test]
    fn event_header_display_includes_subsource_slash_separated() {
        let header = EventHeader {
            counter: "000001".to_string(),
            source: "RoleMachine".to_string(),
            subsource: Some("Producer".to_string()),
            node_id: None,
            attempt: None,
            kind: "RolePromptRendered".to_string(),
            preview: None,
        };
        assert_eq!(
            header.to_string(),
            "000001  RoleMachine/Producer  RolePromptRendered"
        );
    }

    #[test]
    fn event_header_display_omits_slash_without_subsource() {
        let header = EventHeader {
            counter: "000002".to_string(),
            source: "SchedulerMachine".to_string(),
            subsource: None,
            node_id: None,
            attempt: None,
            kind: "MachineStarted".to_string(),
            preview: None,
        };
        assert_eq!(
            header.to_string(),
            "000002  SchedulerMachine  MachineStarted"
        );
    }

    #[test]
    fn failure_kinds_are_recognized() {
        assert!(is_failure_kind("Failure"));
        assert!(is_failure_kind("FailureClassified"));
        assert!(is_failure_kind("ValidationFailed"));
        assert!(is_failure_kind("ParseFailed"));
        assert!(!is_failure_kind("RolePromptRendered"));
        assert!(!is_failure_kind("MachineStarted"));
    }

    #[test]
    fn prompt_body_extracts_text_after_marker_verbatim() {
        let content = "source: RoleMachine\nsubsource: Producer\nkind: RolePromptRendered\n\
            attempt_count: 1\nprompt:\nWrite a function.\n{\"example\":\"json in prompt\"}\n";
        assert_eq!(
            prompt_body(content),
            "Write a function.\n{\"example\":\"json in prompt\"}",
            "prompt body must be returned byte-for-byte, including any embedded JSON snippets"
        );
    }

    #[test]
    fn prompt_body_falls_back_to_full_content_without_marker() {
        let content = "source: RoleMachine\nkind: SomethingElse\nfield: value\n";
        assert_eq!(
            prompt_body(content),
            "source: RoleMachine\nkind: SomethingElse\nfield: value"
        );
    }

    #[test]
    fn json_to_yaml_converts_valid_json() {
        // serde_json orders object keys alphabetically without the
        // "preserve_order" feature, so `content` sorts before `status`.
        let yaml = json_to_yaml(r#"{"status":"accepted","content":"done"}"#)
            .expect("valid JSON must convert");
        assert_eq!(yaml, "content: done\nstatus: accepted");
    }

    #[test]
    fn json_to_yaml_returns_none_for_non_json() {
        assert!(
            json_to_yaml("not json at all").is_none(),
            "plain text must not be reported as convertible"
        );
    }

    #[test]
    fn failure_body_renders_raw_response_json_as_yaml() {
        let content = "source: RoleMachine\nsubsource: Producer\nkind: ParseFailed\n\
            attempt_count: 1\nparse_error: missing field `status`\n\
            raw_response:\n{\"content\":\"done\"}\n";
        let body = failure_body(content);
        assert!(
            body.contains("parse_error: missing field `status`"),
            "fields before raw_response must be preserved verbatim; got: {body}"
        );
        assert!(
            body.ends_with("content: done"),
            "raw_response JSON must be rendered as YAML; got: {body}"
        );
        assert!(
            !body.contains("{\"content\":\"done\"}"),
            "raw JSON form must not remain once converted; got: {body}"
        );
    }

    #[test]
    fn failure_body_leaves_non_json_raw_response_untouched() {
        let content = "source: RoleMachine\nkind: ParseFailed\nattempt_count: 1\n\
            parse_error: empty response\nraw_response:\n(empty)\n";
        let body = failure_body(content);
        assert!(
            body.ends_with("(empty)"),
            "non-JSON raw_response must be printed verbatim; got: {body}"
        );
    }

    #[test]
    fn failure_body_without_raw_response_marker_is_unchanged() {
        let content = "source: SchedulerMachine\nkind: Failure\ncomponent: Worker\nreason: boom\n";
        assert_eq!(
            failure_body(content),
            "source: SchedulerMachine\nkind: Failure\ncomponent: Worker\nreason: boom"
        );
    }

    #[test]
    fn non_txt_files_in_telemetry_directory_are_ignored() {
        let root = temp_dir("ignore-non-txt");
        let telemetry_dir = make_run_telemetry_dir(&root, "run-a");
        let sink = FileTelemetry::new(telemetry_dir.clone());
        sink.record(TelemetryRecord::new(
            "SchedulerMachine",
            TelemetryEvent::MachineStarted {
                machine: "SchedulerHandler".into(),
            },
        ));
        std::fs::write(telemetry_dir.join("notes.md"), "not telemetry").unwrap();
        set_latest(&root, "run-a");

        // Must not error even though a non-.txt file with unrelated content exists.
        let result = run_trace(root.to_str().unwrap(), None, TraceFilter::All);
        assert!(result.is_ok());

        let _ = std::fs::remove_dir_all(&root);
    }
}
