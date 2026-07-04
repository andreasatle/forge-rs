//! Low-level telemetry file reading: directory listing and per-file header
//! parsing shared by every trace view.

use std::error::Error;
use std::path::{Path, PathBuf};

/// List the telemetry files under `run_dir/telemetry`, sorted in emission
/// order.
///
/// `FileTelemetry` prefixes every filename with a zero-padded, incrementing
/// counter, so lexicographic sort matches emission order.
pub(super) fn list_telemetry_files(run_dir: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let telemetry_dir = run_dir.join("telemetry");

    let mut paths: Vec<_> = std::fs::read_dir(&telemetry_dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("txt"))
        .collect();
    paths.sort();
    Ok(paths)
}

/// The routing header (source/subsource/node_id/attempt/kind) shared by
/// every telemetry file, plus the lines that follow it. Both [`EventHeader`]
/// (flat-summary preview) and the default view's field extraction build on
/// this shared split so the header-line-skipping logic exists in exactly one
/// place.
pub(super) struct RecordHeader {
    pub(super) source: String,
    pub(super) subsource: Option<String>,
    pub(super) node_id: Option<String>,
    pub(super) attempt: Option<String>,
    pub(super) kind: String,
    /// Every line after the `kind: <Kind>` line, verbatim and rejoined with `\n`.
    pub(super) body: String,
}

/// Split `content` into its routing header and body.
pub(super) fn split_header(content: &str) -> Option<RecordHeader> {
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

    Some(RecordHeader {
        source,
        subsource,
        node_id,
        attempt,
        kind,
        body: lines.collect::<Vec<_>>().join("\n"),
    })
}

/// Counter, source, optional subsource/node context, and kind parsed from one
/// telemetry file.
pub(super) struct EventHeader {
    pub(super) counter: String,
    pub(super) source: String,
    pub(super) subsource: Option<String>,
    /// Scheduler node id, present only on events that pertain to a single node.
    pub(super) node_id: Option<String>,
    /// Zero-based node attempt number, present only alongside `node_id`.
    pub(super) attempt: Option<String>,
    pub(super) kind: String,
    /// The first field line after `kind:`, truncated for display in the
    /// default summary. `None` when the event has no further fields.
    pub(super) preview: Option<String>,
}

impl EventHeader {
    /// Parse the counter from `path`'s filename and the
    /// source/subsource/node_id/attempt/kind from the leading header lines of
    /// `content`.
    pub(super) fn parse(path: &Path, content: &str) -> Option<Self> {
        let counter = path.file_stem()?.to_str()?.split("--").next()?.to_string();
        let header = split_header(content)?;

        // Skip fields that add nothing beyond what's already shown: a
        // `machine: <name>` field always repeats `source` verbatim (see
        // `run_machine_with_telemetry`), and a bare `field:` marker (e.g.
        // `state:`) introduces a multi-line value with no inline text of its
        // own, so its first content line is used instead.
        let mut lines = header.body.lines();
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
            source: header.source,
            subsource: header.subsource,
            node_id: header.node_id,
            attempt: header.attempt,
            kind: header.kind,
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
pub(super) const PREVIEW_MAX_CHARS: usize = 80;

/// Drop a trailing, unmatched opening bracket left over from the first line
/// of a pretty-printed (`{:#?}`) struct or tuple variant, e.g. `Active {`
/// becomes `Active` and `WorkAccepted(` becomes `WorkAccepted`. The bracket
/// never closes on the same line, so it adds nothing but visual noise.
pub(super) fn strip_trailing_open_bracket(line: &str) -> &str {
    let stripped = line
        .strip_suffix('{')
        .or_else(|| line.strip_suffix('('))
        .or_else(|| line.strip_suffix('['))
        .unwrap_or(line);
    stripped.trim_end()
}

/// Truncate `s` to at most `max_chars` characters, appending an ellipsis
/// when truncated.
pub(super) fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut truncated: String = s.chars().take(max_chars).collect();
    truncated.push('…');
    truncated
}

/// The first 8 characters of a node id, for compact display in trace output.
/// Node ids are full UUIDs; the trace viewer shows only this short prefix,
/// matching the `[worker <short-id>]` form used in log/progress output.
pub(super) fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}
