//! Telemetry trace viewer.
//!
//! Reads the plain-text files written by
//! [`FileTelemetry`](crate::telemetry::FileTelemetry) from a run's telemetry
//! directory and prints them to stdout, either as a chronological one-line
//! summary or as full event content filtered by kind.

use std::error::Error;
use std::path::Path;

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

/// Print telemetry events found in `telemetry_dir` according to `filter`.
///
/// Files are visited in filename order, which matches emission order because
/// `FileTelemetry` prefixes every filename with a zero-padded, incrementing
/// counter.
pub fn run_trace(telemetry_dir: &str, filter: TraceFilter) -> Result<(), Box<dyn Error>> {
    let dir = Path::new(telemetry_dir);
    let mut paths: Vec<_> = std::fs::read_dir(dir)?
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
    matches!(kind, "Failure" | "FailureClassified" | "ValidationFailed")
}

const PROMPT_MARKER: &str = "\nprompt:\n";

fn print_prompt(header: &EventHeader, content: &str) {
    println!("=== {header} ===");
    match content.find(PROMPT_MARKER) {
        Some(idx) => println!(
            "{}",
            content[idx + PROMPT_MARKER.len()..].trim_end_matches('\n')
        ),
        None => println!("{}", content.trim_end_matches('\n')),
    }
    println!();
}

fn print_full(header: &EventHeader, content: &str) {
    println!("=== {header} ===");
    println!("{}", content.trim_end_matches('\n'));
    println!();
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

    #[test]
    fn missing_directory_returns_error() {
        let dir = temp_dir("missing");
        let result = run_trace(dir.to_str().unwrap(), TraceFilter::All);
        assert!(
            result.is_err(),
            "trace over a nonexistent directory must return an error"
        );
    }

    #[test]
    fn all_filter_prints_one_line_per_event() {
        let dir = temp_dir("all-filter");
        let sink = FileTelemetry::new(dir.clone());
        sink.record(TelemetryRecord::new(
            "SchedulerMachine",
            TelemetryEvent::MachineStarted {
                machine: "SchedulerHandler".into(),
            },
        ));
        sink.record(TelemetryRecord::new_with_subsource(
            "RoleMachine",
            "Producer",
            TelemetryEvent::ParseSucceeded { attempt_count: 1 },
        ));

        let result = run_trace(dir.to_str().unwrap(), TraceFilter::All);
        assert!(result.is_ok(), "trace over a valid directory must succeed");

        let _ = std::fs::remove_dir_all(&dir);
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
        assert!(!is_failure_kind("RolePromptRendered"));
        assert!(!is_failure_kind("MachineStarted"));
    }

    #[test]
    fn non_txt_files_in_telemetry_directory_are_ignored() {
        let dir = temp_dir("ignore-non-txt");
        let sink = FileTelemetry::new(dir.clone());
        sink.record(TelemetryRecord::new(
            "SchedulerMachine",
            TelemetryEvent::MachineStarted {
                machine: "SchedulerHandler".into(),
            },
        ));
        std::fs::write(dir.join("notes.md"), "not telemetry").unwrap();

        // Must not error even though a non-.txt file with unrelated content exists.
        let result = run_trace(dir.to_str().unwrap(), TraceFilter::All);
        assert!(result.is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
