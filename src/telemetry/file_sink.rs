//! File-backed telemetry sink.

use std::path::PathBuf;
use std::sync::Mutex;

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
///
/// # Failure policy
///
/// Telemetry is observability, not artifact truth. All I/O failures are
/// handled gracefully:
///
/// - Directory creation failure → sink is disabled; all subsequent `record`
///   calls are silently dropped.
/// - File write failure → event is skipped; run continues unaffected.
pub struct FileTelemetry {
    /// `None` when the root directory could not be created (sink disabled).
    root: Option<PathBuf>,
    counter: Mutex<u64>,
    /// Count of events that could not be written due to I/O errors.
    telemetry_failures: Mutex<usize>,
}

impl FileTelemetry {
    /// Creates a new `FileTelemetry` that writes into `root`.
    ///
    /// The directory (and any missing ancestors) is created immediately. If
    /// that fails the sink is silently disabled: `record` becomes a no-op so
    /// telemetry failure never aborts a run.
    pub fn new(root: PathBuf) -> Self {
        let enabled_root = match std::fs::create_dir_all(&root) {
            Ok(()) => Some(root),
            Err(_) => None,
        };
        Self {
            root: enabled_root,
            counter: Mutex::new(0),
            telemetry_failures: Mutex::new(0),
        }
    }
}

impl TelemetrySink for FileTelemetry {
    fn record(&self, record: TelemetryRecord) {
        let Some(root) = &self.root else {
            return;
        };

        let mut n = self
            .counter
            .lock()
            .expect("telemetry counter mutex poisoned");
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
        let path = root.join(filename);
        if let Err(err) = std::fs::write(path, record.file_content()) {
            *self
                .telemetry_failures
                .lock()
                .expect("telemetry failures mutex poisoned") += 1;
            eprintln!("telemetry write failed: {err}");
        }
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
        let _sink = FileTelemetry::new(dir.clone());
        assert!(dir.exists(), "telemetry root directory must be created");
    }

    #[test]
    fn file_telemetry_writes_incrementing_files() {
        let dir = fresh_dir("increments");
        let sink = FileTelemetry::new(dir.clone());
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
        let sink = FileTelemetry::new(dir.clone());
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

    struct FileShapeCase {
        record: TelemetryRecord,
        expected_filename: &'static str,
        expected_content: &'static [&'static str],
    }

    #[test]
    fn file_telemetry_writes_expected_file_names_and_body_headers() {
        let dir = fresh_dir("file-shapes");
        let sink = FileTelemetry::new(dir.clone());

        let cases = [
            FileShapeCase {
                record: TelemetryRecord::new(
                    "SchedulerMachine",
                    TelemetryEvent::StateEntered {
                        machine: "SchedulerMachine".into(),
                        state: "Ready".into(),
                    },
                ),
                expected_filename: "000001--scheduler-machine--state-entered.txt",
                expected_content: &["source: SchedulerMachine", "kind: StateEntered"],
            },
            FileShapeCase {
                record: TelemetryRecord::new_with_subsource(
                    "RoleMachine",
                    "Producer",
                    TelemetryEvent::RolePromptRendered {
                        prompt: "hello".into(),
                        attempt_count: 1,
                    },
                ),
                expected_filename: "000002--role-machine--producer--role-prompt-rendered.txt",
                expected_content: &[
                    "source: RoleMachine",
                    "subsource: Producer",
                    "kind: RolePromptRendered",
                ],
            },
            FileShapeCase {
                record: TelemetryRecord::new_with_subsource(
                    "RoleMachine",
                    "Critic",
                    TelemetryEvent::ParseFailed {
                        raw_response: "bad".into(),
                        parse_error: "err".into(),
                        attempt_count: 1,
                    },
                ),
                expected_filename: "000003--role-machine--critic--parse-failed.txt",
                expected_content: &[
                    "source: RoleMachine",
                    "subsource: Critic",
                    "kind: ParseFailed",
                ],
            },
            FileShapeCase {
                record: TelemetryRecord::new_with_subsource(
                    "RoleMachine",
                    "Referee",
                    TelemetryEvent::ParseSucceeded { attempt_count: 1 },
                ),
                expected_filename: "000004--role-machine--referee--parse-succeeded.txt",
                expected_content: &[
                    "source: RoleMachine",
                    "subsource: Referee",
                    "kind: ParseSucceeded",
                ],
            },
        ];

        for case in cases {
            sink.record(case.record);
            let path = dir.join(case.expected_filename);
            assert!(path.exists(), "expected telemetry file {}", path.display());
            let content = std::fs::read_to_string(&path).unwrap();
            for expected in case.expected_content {
                assert!(
                    content.contains(expected),
                    "expected {expected:?} in {}; got: {content}",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn file_body_includes_node_id_and_attempt_when_present() {
        let dir = fresh_dir("node-context");
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

        let content =
            std::fs::read_to_string(dir.join("000001--deliberation-machine--state-entered.txt"))
                .unwrap();
        assert!(
            content.contains("node_id: root-child-0"),
            "file body must include node_id as a top-level field; got: {content}"
        );
        assert!(
            content.contains("attempt: 1"),
            "file body must include attempt as a top-level field; got: {content}"
        );
    }

    #[test]
    fn file_body_omits_node_id_and_attempt_when_absent() {
        let dir = fresh_dir("no-node-context");
        let sink = FileTelemetry::new(dir.clone());
        sink.record(TelemetryRecord::new(
            "SchedulerMachine",
            TelemetryEvent::StateEntered {
                machine: "SchedulerMachine".into(),
                state: "Ready".into(),
            },
        ));

        let content =
            std::fs::read_to_string(dir.join("000001--scheduler-machine--state-entered.txt"))
                .unwrap();
        assert!(
            !content.contains("node_id:"),
            "file body must omit node_id when the record carries none; got: {content}"
        );
        assert!(
            !content.contains("attempt:"),
            "file body must omit attempt when the record carries none; got: {content}"
        );
    }

    #[test]
    fn telemetry_write_failure_does_not_panic() {
        let dir = fresh_dir("write-fail");
        let sink = FileTelemetry::new(dir.clone());
        // Place a directory at the exact path where the first file would land.
        // fs::write on a directory path fails on all platforms.
        std::fs::create_dir(dir.join("000001--test--machine-started.txt")).unwrap();
        // Must not panic.
        sink.record(TelemetryRecord::new(
            "Test",
            TelemetryEvent::MachineStarted {
                machine: "Test".into(),
            },
        ));
    }

    #[test]
    fn telemetry_directory_creation_failure_disables_sink() {
        // Build a path whose parent is a regular file, so create_dir_all must fail.
        let base = fresh_dir("dir-fail-base");
        std::fs::create_dir_all(&base).unwrap();
        let file_path = base.join("not-a-dir.txt");
        std::fs::write(&file_path, "content").unwrap();
        let sink = FileTelemetry::new(file_path.join("telemetry"));
        // Sink is disabled; record must not panic.
        sink.record(TelemetryRecord::new(
            "Test",
            TelemetryEvent::MachineStarted {
                machine: "Test".into(),
            },
        ));
    }
}
