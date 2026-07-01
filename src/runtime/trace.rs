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
            TraceFilter::All => println!("{header}"),
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

/// Counter, source, optional subsource, and kind parsed from one telemetry file.
struct EventHeader {
    counter: String,
    source: String,
    subsource: Option<String>,
    kind: String,
}

impl EventHeader {
    /// Parse the counter from `path`'s filename and the source/subsource/kind
    /// from the leading `source:`/`subsource:`/`kind:` lines of `content`.
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
        let kind = line.strip_prefix("kind: ")?.to_string();

        Some(Self {
            counter,
            source,
            subsource,
            kind,
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
    fn event_header_display_includes_subsource_slash_separated() {
        let header = EventHeader {
            counter: "000001".to_string(),
            source: "RoleMachine".to_string(),
            subsource: Some("Producer".to_string()),
            kind: "RolePromptRendered".to_string(),
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
            kind: "MachineStarted".to_string(),
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
