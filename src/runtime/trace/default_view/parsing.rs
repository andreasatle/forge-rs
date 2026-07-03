//! Turns telemetry file contents into `(node_id, attempt, RawRecord)`
//! triples.
//!
//! Only `EffectEmitted` for `RunNode`/`IntegrateWork` and every
//! `DeliberationMachine` `StateEntered`/`EventReceived`/`EffectEmitted`
//! record carry an explicit `node_id`/`attempt` on the telemetry header
//! (see `EffectContextTelemetry` and `NodeContextTelemetry` in the
//! scheduler/node_runner modules). Everything else — `RoleMachine`
//! role-protocol records and `Integration`-sourced validation records —
//! carries none. Execution is strictly sequential (one node/attempt in
//! flight at a time), so a single linear pass that remembers the last
//! explicit `(node_id, attempt)` and applies it to context-less records
//! reconstructs grouping correctly.

use super::super::reader::split_header;
use std::path::PathBuf;

/// One telemetry record with its header fields and full body text, before
/// node/attempt context has been resolved.
pub(super) struct RawRecord {
    pub(super) source: String,
    pub(super) subsource: Option<String>,
    pub(super) kind: String,
    pub(super) body: String,
    node_id: Option<String>,
    attempt: Option<u32>,
}

/// A [`RawRecord`] with its node/attempt context resolved.
pub(super) struct ContextRecord {
    pub(super) node_id: String,
    pub(super) attempt: u32,
    pub(super) record: RawRecord,
}

pub(super) struct DefaultTraceParser<'a> {
    paths: &'a [PathBuf],
    current_context: Option<(String, u32)>,
}

impl<'a> DefaultTraceParser<'a> {
    pub(super) fn new(paths: &'a [PathBuf]) -> Self {
        Self {
            paths,
            current_context: None,
        }
    }

    /// Read and parse every telemetry file in `paths`, in order.
    pub(super) fn read_records(&self) -> std::io::Result<Vec<RawRecord>> {
        let mut records = Vec::with_capacity(self.paths.len());
        for path in self.paths {
            let content = std::fs::read_to_string(path)?;
            if let Some(record) = Self::parse_record(&content) {
                records.push(record);
            }
        }
        Ok(records)
    }

    pub(super) fn parse_record(content: &str) -> Option<RawRecord> {
        let header = split_header(content)?;
        Some(RawRecord {
            source: header.source,
            subsource: header.subsource,
            kind: header.kind,
            body: header.body,
            node_id: header.node_id,
            attempt: header.attempt.and_then(|a| a.parse().ok()),
        })
    }

    /// Assign every record to the node/attempt it belongs to, inheriting the
    /// last explicit context for records that carry none of their own.
    ///
    /// Records observed before any node context has been established (e.g. the
    /// scheduler's own startup records) are dropped — they don't belong to any
    /// node and the default view has nothing to attach them to.
    pub(super) fn assign_node_context(&mut self, records: Vec<RawRecord>) -> Vec<ContextRecord> {
        let mut out = Vec::with_capacity(records.len());

        for record in records {
            if let Some(node_id) = &record.node_id {
                self.current_context = Some((node_id.clone(), record.attempt.unwrap_or(0)));
            }
            if let Some((node_id, attempt)) = &self.current_context {
                out.push(ContextRecord {
                    node_id: node_id.clone(),
                    attempt: *attempt,
                    record,
                });
            }
        }

        out
    }
}

/// Extract a `field: value` line from a pretty-printed (`{:#?}`) Debug dump.
///
/// Handles both quoted string values (unescaping `\"`/`\\`/`\n`) and bare
/// values (enum unit variants, numbers, nested-struct openers like
/// `Terminal {`), stripping the trailing comma/brace/paren that Rust's
/// pretty-printer always appends. Each telemetry body describes exactly one
/// event, so field names never collide within it — a plain line scan is
/// sufficient regardless of nesting depth.
pub(super) fn debug_field(body: &str, field: &str) -> Option<String> {
    let prefix = format!("{field}: ");
    let line = body.lines().find_map(|line| {
        let trimmed = line.trim();
        trimmed.strip_prefix(&prefix)
    })?;

    let value = line.strip_suffix(',').unwrap_or(line);
    let value = value
        .strip_suffix('{')
        .or_else(|| value.strip_suffix('('))
        .map(str::trim_end)
        .unwrap_or(value);

    if let Some(unquoted) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        Some(unescape(unquoted))
    } else {
        Some(value.to_string())
    }
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The variant name of a pretty-printed enum, e.g. `CriticAccepted` from a
/// body whose first content line (after skipping a `machine:` field and a
/// bare `event:`/`effect:`/`state:` marker) is `CriticAccepted {`.
pub(super) fn event_variant_name(body: &str) -> Option<&str> {
    let mut lines = body.lines();
    let mut line = lines.next()?;
    loop {
        if line.starts_with("machine: ") || line.trim_end().ends_with(':') {
            line = lines.next()?;
            continue;
        }
        break;
    }
    let trimmed = line.trim();
    let name = trimmed
        .strip_suffix('{')
        .or_else(|| trimmed.strip_suffix('('))
        .map(str::trim_end)
        .unwrap_or(trimmed);
    if name.is_empty() { None } else { Some(name) }
}
