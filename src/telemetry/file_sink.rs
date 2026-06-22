//! File-backed telemetry sink.

use std::cell::RefCell;
use std::path::PathBuf;

use super::event::TelemetryRecord;
use super::sink::TelemetrySink;

/// A sink that writes one plain-text file per event into a directory.
///
/// Files are named with a six-digit counter, source slug, optional subsource
/// slug, and event kind slug separated by `--`, e.g.
/// `000001--scheduler-machine--machine-started.txt` or
/// `000020--role-machine--producer--role-prompt-rendered.txt`. This produces a
/// deterministic, alphabetically-ordered trace of a machine run.
///
/// # File format
///
/// ```text
/// source: SchedulerMachine
/// kind: StateEntered
/// machine: SchedulerHandler
/// state:
/// Running {
///     graph: …
/// }
/// ```
///
/// All values come from the `TelemetryEvent` payload; no external serialiser
/// is required.
pub struct FileTelemetry {
    root: PathBuf,
    counter: RefCell<u64>,
}

impl FileTelemetry {
    /// Creates a new `FileTelemetry` that writes into `root`.
    ///
    /// The directory (and any missing ancestors) is created immediately.
    /// Returns an error if the directory cannot be created.
    pub fn new(root: PathBuf) -> Result<Self, std::io::Error> {
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            counter: RefCell::new(0),
        })
    }
}

impl TelemetrySink for FileTelemetry {
    fn record(&self, record: TelemetryRecord) {
        let mut n = self.counter.borrow_mut();
        *n += 1;
        let filename = match &record.subsource {
            Some(sub) => format!(
                "{:06}--{}--{}--{}.txt",
                *n,
                kebab_case(&record.source),
                kebab_case(sub),
                record.event.kind_slug()
            ),
            None => format!(
                "{:06}--{}--{}.txt",
                *n,
                kebab_case(&record.source),
                record.event.kind_slug()
            ),
        };
        let path = self.root.join(filename);
        std::fs::write(path, record.file_content()).expect("telemetry write failed");
    }
}

fn kebab_case(value: &str) -> String {
    let mut slug = String::new();
    for (index, character) in value.chars().enumerate() {
        if character.is_ascii_uppercase() {
            if index > 0 && !slug.ends_with('-') {
                slug.push('-');
            }
            slug.push(character.to_ascii_lowercase());
        } else if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::TelemetryEvent;

    fn fresh_dir(suffix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("forge-telemetry-test-{suffix}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn file_telemetry_creates_directory() {
        let dir = fresh_dir("creates");
        let _sink = FileTelemetry::new(dir.clone()).unwrap();
        assert!(dir.exists(), "telemetry root directory must be created");
    }

    #[test]
    fn file_telemetry_writes_incrementing_files() {
        let dir = fresh_dir("increments");
        let sink = FileTelemetry::new(dir.clone()).unwrap();
        sink.record(TelemetryRecord::new(
            "A",
            TelemetryEvent::MachineStarted {
                machine: "A".into(),
            },
        ));
        assert!(dir.join("000001--a--machine-started.txt").exists());
    }

    #[test]
    fn file_telemetry_file_content_contains_kind_and_fields() {
        let dir = fresh_dir("content");
        let sink = FileTelemetry::new(dir.clone()).unwrap();
        sink.record(TelemetryRecord::new(
            "MyMachine",
            TelemetryEvent::StateEntered {
                machine: "MyMachine".into(),
                state: "Idle {}".into(),
            },
        ));
        let content =
            std::fs::read_to_string(dir.join("000001--my-machine--state-entered.txt")).unwrap();
        assert!(content.contains("source: MyMachine"));
        assert!(content.contains("kind: StateEntered"));
        assert!(content.contains("machine: MyMachine"));
        assert!(content.contains("Idle {}"));
    }

    #[test]
    fn file_name_contains_source_and_kind() {
        let dir = fresh_dir("source-and-kind");
        let sink = FileTelemetry::new(dir.clone()).unwrap();
        sink.record(TelemetryRecord::new(
            "SchedulerMachine",
            TelemetryEvent::StateEntered {
                machine: "SchedulerMachine".into(),
                state: "Ready".into(),
            },
        ));
        assert!(
            dir.join("000001--scheduler-machine--state-entered.txt")
                .exists()
        );
    }

    #[test]
    fn file_contents_include_same_source() {
        let dir = fresh_dir("matching-source");
        let sink = FileTelemetry::new(dir.clone()).unwrap();
        sink.record(TelemetryRecord::new(
            "RoleMachine",
            TelemetryEvent::ParseFailed {
                raw_response: "bad".into(),
                parse_error: "invalid".into(),
                attempt_count: 1,
            },
        ));
        let content =
            std::fs::read_to_string(dir.join("000001--role-machine--parse-failed.txt")).unwrap();
        assert!(content.contains("source: RoleMachine"));
    }

    #[test]
    fn file_name_uses_double_separator() {
        let dir = fresh_dir("double-sep");
        let sink = FileTelemetry::new(dir.clone()).unwrap();
        sink.record(TelemetryRecord::new(
            "SchedulerMachine",
            TelemetryEvent::StateEntered {
                machine: "SchedulerMachine".into(),
                state: "Ready".into(),
            },
        ));
        assert!(
            dir.join("000001--scheduler-machine--state-entered.txt")
                .exists()
        );
    }

    #[test]
    fn role_event_file_name_contains_role() {
        let dir = fresh_dir("role-subsource");
        let sink = FileTelemetry::new(dir.clone()).unwrap();

        sink.record(TelemetryRecord::new_with_subsource(
            "RoleMachine",
            "Producer",
            TelemetryEvent::RolePromptRendered {
                prompt: "p".into(),
                attempt_count: 1,
            },
        ));
        sink.record(TelemetryRecord::new_with_subsource(
            "RoleMachine",
            "Critic",
            TelemetryEvent::ParseFailed {
                raw_response: "bad".into(),
                parse_error: "err".into(),
                attempt_count: 1,
            },
        ));
        sink.record(TelemetryRecord::new_with_subsource(
            "RoleMachine",
            "Referee",
            TelemetryEvent::ParseSucceeded { attempt_count: 1 },
        ));

        assert!(
            dir.join("000001--role-machine--producer--role-prompt-rendered.txt")
                .exists()
        );
        assert!(
            dir.join("000002--role-machine--critic--parse-failed.txt")
                .exists()
        );
        assert!(
            dir.join("000003--role-machine--referee--parse-succeeded.txt")
                .exists()
        );
    }

    #[test]
    fn file_body_contains_matching_subsource() {
        let dir = fresh_dir("body-subsource");
        let sink = FileTelemetry::new(dir.clone()).unwrap();
        sink.record(TelemetryRecord::new_with_subsource(
            "RoleMachine",
            "Producer",
            TelemetryEvent::RolePromptRendered {
                prompt: "hello".into(),
                attempt_count: 1,
            },
        ));
        let content = std::fs::read_to_string(
            dir.join("000001--role-machine--producer--role-prompt-rendered.txt"),
        )
        .unwrap();
        assert!(content.contains("source: RoleMachine"));
        assert!(content.contains("subsource: Producer"));
    }
}
