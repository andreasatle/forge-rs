//! Source-to-validation-target derivation rules.
//!
//! Shared between project adapters and language specs so both can declare
//! "a source file matching this pattern needs a validation target matching
//! that pattern" without duplicating the matching engine.

use serde::Deserialize;

/// One source-to-validation-target derivation rule.
///
/// `pattern` is matched against a file's basename (its directory prefix is
/// preserved separately); it contains exactly one `{stem}` placeholder and
/// matches when the basename starts with the text before `{stem}` and ends
/// with the text after it. The captured middle section is substituted into
/// `target` to derive the validation target path, which is appended after
/// the source file's directory prefix.
///
/// `target` may itself contain a directory component, in which case it is
/// treated as a fixed subdirectory relative to the source file's directory
/// rather than part of the matched basename.
///
/// Example: `pattern: "{stem}.py"`, `target: "test_{stem}.py"` derives
/// `test_main.py` from `main.py`. `pattern: "{stem}.rs"`,
/// `target: "{stem}_test.rs"` derives `lib_test.rs` from `lib.rs`.
/// `pattern: "{stem}.py"`, `target: "tests/test_{stem}.py"` derives
/// `tests/test_main.py` from `main.py`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationTargetRule {
    /// Source basename pattern, e.g. `"{stem}.py"`.
    pub pattern: String,
    /// Validation target pattern, e.g. `"test_{stem}.py"` or
    /// `"tests/test_{stem}.py"`.
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
/// A target is skipped (returns `None`) when it already looks like a
/// derived validation file for any rule — this marks it as a derived
/// validation file rather than a source that needs one. Otherwise the
/// first rule whose *source* pattern matches wins.
fn derive_validation_target(rules: &[ValidationTargetRule], target: &str) -> Option<String> {
    let path = target.replace('\\', "/");
    let (prefix, basename) = path
        .rsplit_once('/')
        .map(|(dir, name)| (format!("{dir}/"), name))
        .unwrap_or((String::new(), path.as_str()));

    let already_derived = rules.iter().any(|rule| is_derived_target(&path, rule));
    if already_derived {
        return None;
    }

    rules.iter().find_map(|rule| {
        let stem = match_stem(basename, &rule.pattern)?;
        Some(format!("{prefix}{}", rule.target.replace("{stem}", &stem)))
    })
}

/// Whether `path` already looks like a validation target produced by `rule`.
///
/// `rule.target` may carry a directory component (e.g. `"tests/test_{stem}.py"`).
/// When it does, `path`'s own directory must end with that component; when
/// it doesn't, only `path`'s basename is checked, matching the historical
/// (directory-agnostic) behavior.
fn is_derived_target(path: &str, rule: &ValidationTargetRule) -> bool {
    let (target_dir, target_basename) = rule
        .target
        .rsplit_once('/')
        .map(|(dir, name)| (Some(dir), name))
        .unwrap_or((None, rule.target.as_str()));
    let (path_dir, path_basename) = path
        .rsplit_once('/')
        .map(|(dir, name)| (Some(dir), name))
        .unwrap_or((None, path));

    if match_stem(path_basename, target_basename).is_none() {
        return false;
    }

    match target_dir {
        None => true,
        Some(dir) => match path_dir {
            Some(path_dir) => path_dir == dir || path_dir.ends_with(&format!("/{dir}")),
            None => false,
        },
    }
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

    fn python_rules() -> Vec<ValidationTargetRule> {
        vec![ValidationTargetRule {
            pattern: "{stem}.py".to_string(),
            target: "tests/test_{stem}.py".to_string(),
        }]
    }

    #[test]
    fn derive_validation_targets_places_target_in_subdirectory() {
        // Invariant: a directory-qualified `target` pattern nests the derived
        // validation target under that subdirectory, not next to the source.
        let derived = derive_validation_targets(&python_rules(), &["main.py".to_string()]);
        assert_eq!(derived, vec!["tests/test_main.py".to_string()]);
    }

    #[test]
    fn derive_validation_targets_preserves_source_directory_prefix() {
        // Invariant: a source file nested in its own directory gets its test
        // file placed under a sibling `tests/` directory, not the repo root.
        let derived = derive_validation_targets(&python_rules(), &["pkg/main.py".to_string()]);
        assert_eq!(derived, vec!["pkg/tests/test_main.py".to_string()]);
    }

    #[test]
    fn derive_validation_targets_skips_file_already_in_target_subdirectory() {
        // Invariant: a file that already lives under the target rule's
        // subdirectory and matches its basename pattern is recognized as an
        // already-derived validation file, not treated as a new source that
        // itself needs a test (regression: used to produce
        // "tests/tests/test_test_main.py").
        let derived =
            derive_validation_targets(&python_rules(), &["tests/test_main.py".to_string()]);
        assert!(derived.is_empty());
    }
}
