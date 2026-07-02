//! Source-to-validation-target derivation rules.
//!
//! Shared between project adapters and language specs so both can declare
//! "a source file matching this pattern needs a validation target matching
//! that pattern" without duplicating the matching engine.

use serde::Deserialize;

/// One source-to-validation-target derivation rule.
///
/// `pattern` and `target` each contain exactly one `{stem}` placeholder and
/// are matched against a file's basename (its directory prefix is preserved
/// separately). A basename matches when it starts with the text before
/// `{stem}` and ends with the text after it; the captured middle section is
/// substituted into `target` to derive the validation target basename.
///
/// Example: `pattern: "{stem}.py"`, `target: "test_{stem}.py"` derives
/// `test_main.py` from `main.py`. `pattern: "{stem}.rs"`,
/// `target: "{stem}_test.rs"` derives `lib_test.rs` from `lib.rs`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationTargetRule {
    /// Source basename pattern, e.g. `"{stem}.py"`.
    pub pattern: String,
    /// Validation target basename pattern, e.g. `"test_{stem}.py"`.
    pub target: String,
}

/// Apply `rules` to each of `targets`, returning the derived validation
/// targets for the sources that match a rule.
///
/// Targets that already look like a derived validation file (their basename
/// matches some rule's *target* pattern), or that match no rule's *source*
/// pattern, are skipped.
pub fn derive_validation_targets(
    rules: &[ValidationTargetRule],
    targets: &[String],
) -> Vec<String> {
    targets
        .iter()
        .filter_map(|target| derive_validation_target(rules, target))
        .collect()
}

/// Apply `rules` to a single `target` path, returning the derived validation
/// target if one applies.
///
/// A target is skipped (returns `None`) when its basename already matches
/// the *target* pattern of any rule — this marks it as a derived validation
/// file rather than a source that needs one. Otherwise the first rule whose
/// *source* pattern matches wins.
fn derive_validation_target(rules: &[ValidationTargetRule], target: &str) -> Option<String> {
    let path = target.replace('\\', "/");
    let (prefix, basename) = path
        .rsplit_once('/')
        .map(|(dir, name)| (format!("{dir}/"), name))
        .unwrap_or((String::new(), path.as_str()));

    let already_derived = rules
        .iter()
        .any(|rule| match_stem(basename, &rule.target).is_some());
    if already_derived {
        return None;
    }

    rules.iter().find_map(|rule| {
        let stem = match_stem(basename, &rule.pattern)?;
        Some(format!("{prefix}{}", rule.target.replace("{stem}", &stem)))
    })
}

/// Match `basename` against `pattern`, which must contain exactly one
/// `{stem}` placeholder. Returns the captured stem on success.
fn match_stem(basename: &str, pattern: &str) -> Option<String> {
    let (before, after) = pattern.split_once("{stem}")?;
    if basename.len() < before.len() + after.len() {
        return None;
    }
    if !basename.starts_with(before) || !basename.ends_with(after) {
        return None;
    }
    let stem = &basename[before.len()..basename.len() - after.len()];
    if stem.is_empty() {
        return None;
    }
    Some(stem.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_stem_rejects_pattern_without_placeholder() {
        assert_eq!(match_stem("main.py", "main.py"), None);
    }

    #[test]
    fn match_stem_rejects_empty_capture() {
        // Invariant: a pattern that would capture an empty stem does not match.
        assert_eq!(match_stem(".py", "{stem}.py"), None);
    }

    #[test]
    fn match_stem_rejects_non_matching_basename() {
        assert_eq!(match_stem("main.rs", "{stem}.py"), None);
    }
}
