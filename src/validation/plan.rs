//! Node-owned validation plan executed before artifact integration.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::artifacts::Workspace;

use super::validator::{matches_name_glob, workspace_has_matching_file};
use super::{CommandSpec, ValidationResult, ValidationScope};

fn default_timeout_seconds() -> u64 {
    120
}

fn default_must_pass() -> bool {
    true
}

/// The lifecycle stage at which a validation step runs.
///
/// `PreIntegration` is the only stage today.  Future variants (e.g.
/// `PostIntegration`) can be added without breaking existing plans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidationStage {
    /// Run after the artifact update is applied but before the git commit.
    PreIntegration,
}

/// A single command step inside a [`ValidationPlan`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationStep {
    /// The command to run: `command[0]` is the program, the remainder are args.
    ///
    /// Executed directly via [`std::process::Command`]; no shell is involved.
    pub command: Vec<String>,
    /// When non-empty, the step is skipped unless at least one workspace file
    /// matches any pattern.  Supports the same simple glob syntax as
    /// [`CommandSpec::when_files_present`] (single `*` wildcard).
    #[serde(default)]
    pub when_artifacts_present: Vec<String>,
    /// Which file set, if any, should be appended to the command.
    #[serde(default = "default_validation_scope")]
    pub scope: ValidationScope,
    /// The lifecycle stage at which this step runs.
    pub stage: ValidationStage,
    /// When `true` (the default), a non-zero exit halts the plan and returns
    /// the failure immediately.  When `false`, the failure is skipped and
    /// subsequent steps still run.
    #[serde(default = "default_must_pass")]
    pub must_pass: bool,
}

/// A per-node validation contract executed before artifact integration.
///
/// The plan replaces the global [`super::Validator`] singleton so that
/// validation behaviour is captured at node-creation time and survives
/// checkpoint/resume unchanged, regardless of any later config changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationPlan {
    /// Ordered steps to execute.
    pub steps: Vec<ValidationStep>,
    /// Per-command wall-clock timeout in seconds.  Defaults to 120.
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
}

impl ValidationPlan {
    /// Execute all `PreIntegration` steps against `workspace` without node-local
    /// file context. Workspace-scoped commands run normally; file-scoped
    /// commands receive an empty path list.
    pub fn execute(&self, workspace: &Workspace) -> ValidationResult {
        self.execute_scoped(workspace, &[], &[])
    }

    /// Execute all `PreIntegration` steps against `workspace`.
    ///
    /// Steps are executed in order.  A step with a non-empty
    /// `when_artifacts_present` list is skipped when no workspace file matches
    /// any of its patterns.  On the first `must_pass` step that fails, the
    /// failure is returned immediately.  Non-`must_pass` failures are ignored
    /// and execution continues.
    pub fn execute_scoped(
        &self,
        workspace: &Workspace,
        target_files: &[String],
        changed_files: &[String],
    ) -> ValidationResult {
        let timeout = Duration::from_secs(self.timeout_seconds);
        let mut ran = 0usize;
        for step in &self.steps {
            if step.stage != ValidationStage::PreIntegration {
                continue;
            }
            if step.command.is_empty() {
                continue;
            }
            let scoped_paths = match step.scope {
                ValidationScope::TargetFiles => target_files,
                ValidationScope::ChangedFiles => changed_files,
                ValidationScope::Workspace => &[],
            };
            if should_skip_for_missing_artifacts(step, workspace, scoped_paths) {
                continue;
            }
            ran += 1;
            let mut args = step.command[1..].to_vec();
            if step.scope != ValidationScope::Workspace {
                args.extend(scoped_paths.iter().cloned());
            }
            let spec = CommandSpec {
                program: step.command[0].clone(),
                args,
                when_files_present: vec![],
                scope: ValidationScope::Workspace,
            };
            let result = spec.run_with_timeout(workspace.path(), timeout);
            if !result.passed && step.must_pass {
                return result;
            }
        }
        ValidationResult {
            passed: true,
            summary: format!("all {ran} step(s) passed"),
            failure: None,
        }
    }
}

fn default_validation_scope() -> ValidationScope {
    ValidationScope::Workspace
}

fn should_skip_for_missing_artifacts(
    step: &ValidationStep,
    workspace: &Workspace,
    scoped_paths: &[String],
) -> bool {
    if step.when_artifacts_present.is_empty() {
        return false;
    }

    match step.scope {
        ValidationScope::Workspace => {
            !workspace_has_matching_file(workspace.path(), &step.when_artifacts_present)
        }
        ValidationScope::TargetFiles | ValidationScope::ChangedFiles => {
            !paths_have_matching_file(scoped_paths, &step.when_artifacts_present)
        }
    }
}

fn paths_have_matching_file(paths: &[String], patterns: &[String]) -> bool {
    paths.iter().any(|path| {
        let name = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(path);
        patterns.iter().any(|p| matches_name_glob(p, name))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::Workspace;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_workspace() -> (PathBuf, Workspace) {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("forge-plan-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        let ws = Workspace::at_path(path.clone(), "abc".to_string());
        (path, ws)
    }

    fn step(cmd: &[&str]) -> ValidationStep {
        ValidationStep {
            command: cmd.iter().map(|s| s.to_string()).collect(),
            when_artifacts_present: vec![],
            scope: ValidationScope::Workspace,
            stage: ValidationStage::PreIntegration,
            must_pass: true,
        }
    }

    fn plan(steps: Vec<ValidationStep>) -> ValidationPlan {
        ValidationPlan {
            steps,
            timeout_seconds: 30,
        }
    }

    #[test]
    fn empty_plan_passes() {
        // Invariant: a plan with no steps always passes.
        let (_path, ws) = temp_workspace();
        let result = plan(vec![]).execute(&ws);
        assert!(
            result.passed,
            "empty plan must pass; got: {}",
            result.summary
        );
    }

    #[test]
    fn passing_step_succeeds() {
        // Invariant: a plan whose single step exits 0 passes.
        let (_path, ws) = temp_workspace();
        let result = plan(vec![step(&["true"])]).execute(&ws);
        assert!(
            result.passed,
            "true step must pass; got: {}",
            result.summary
        );
    }

    #[test]
    fn failing_must_pass_step_fails_plan() {
        // Invariant: a must_pass step that exits non-zero fails the plan.
        let (_path, ws) = temp_workspace();
        let result = plan(vec![step(&["false"])]).execute(&ws);
        assert!(!result.passed, "false must_pass step must fail the plan");
    }

    #[test]
    fn failing_non_must_pass_step_does_not_fail_plan() {
        // Invariant: a step with must_pass=false that exits non-zero is ignored;
        // subsequent steps still run.
        let (_path, ws) = temp_workspace();
        let s = ValidationStep {
            command: vec!["false".to_string()],
            when_artifacts_present: vec![],
            scope: ValidationScope::Workspace,
            stage: ValidationStage::PreIntegration,
            must_pass: false,
        };
        let result = plan(vec![s]).execute(&ws);
        assert!(
            result.passed,
            "non-must_pass failure must be ignored; got: {}",
            result.summary
        );
    }

    #[test]
    fn when_artifacts_present_skips_step_when_no_match() {
        // Invariant: a step with when_artifacts_present set is skipped — not
        // failed — when no workspace file matches any pattern.
        let (path, ws) = temp_workspace();
        let s = ValidationStep {
            command: vec!["false".to_string()],
            when_artifacts_present: vec!["test_*.py".to_string()],
            scope: ValidationScope::Workspace,
            stage: ValidationStage::PreIntegration,
            must_pass: true,
        };
        let result = plan(vec![s]).execute(&ws);
        assert!(
            result.passed,
            "step guarded by when_artifacts_present must be skipped when no matching file exists"
        );
        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn when_artifacts_present_runs_step_when_file_matches() {
        // Invariant: a step with when_artifacts_present IS run when a matching
        // file exists in the workspace.
        let (path, ws) = temp_workspace();
        std::fs::write(path.join("test_foo.py"), "# test\n").unwrap();
        let s = ValidationStep {
            command: vec!["true".to_string()],
            when_artifacts_present: vec!["test_*.py".to_string()],
            scope: ValidationScope::Workspace,
            stage: ValidationStage::PreIntegration,
            must_pass: true,
        };
        let result = plan(vec![s]).execute(&ws);
        assert!(
            result.passed,
            "step guarded by when_artifacts_present must run when matching file exists"
        );
        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn target_files_scope_appends_node_targets_to_command() {
        // Invariant: target-file scoped commands receive the node's declared
        // target files as trailing command args.
        let (path, ws) = temp_workspace();
        let s = ValidationStep {
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf '%s\n' \"$@\" > captured.txt".to_string(),
                "capture".to_string(),
            ],
            when_artifacts_present: vec![],
            scope: ValidationScope::TargetFiles,
            stage: ValidationStage::PreIntegration,
            must_pass: true,
        };
        let targets = vec!["src/lib.rs".to_string(), "README.md".to_string()];

        let result = plan(vec![s]).execute_scoped(&ws, &targets, &[]);

        assert!(result.passed, "scoped capture command must pass");
        let captured = std::fs::read_to_string(path.join("captured.txt")).unwrap();
        assert_eq!(captured, "src/lib.rs\nREADME.md\n");
        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn workspace_scope_does_not_append_node_targets() {
        // Invariant: workspace-scoped commands run exactly as declared.
        let (path, ws) = temp_workspace();
        let s = ValidationStep {
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf '%s\n' \"$#\" > count.txt".to_string(),
                "capture".to_string(),
            ],
            when_artifacts_present: vec![],
            scope: ValidationScope::Workspace,
            stage: ValidationStage::PreIntegration,
            must_pass: true,
        };
        let targets = vec!["src/lib.rs".to_string()];

        let result = plan(vec![s]).execute_scoped(&ws, &targets, &[]);

        assert!(result.passed, "workspace capture command must pass");
        let captured = std::fs::read_to_string(path.join("count.txt")).unwrap();
        assert_eq!(captured, "0\n");
        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn target_file_guard_runs_pytest_like_step_for_matching_test_target() {
        // Invariant: a target-scoped test command guarded by test-file
        // patterns runs when the node target itself is a matching test file.
        let (path, ws) = temp_workspace();
        let s = ValidationStep {
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "touch pytest-ran".to_string(),
                "pytest".to_string(),
            ],
            when_artifacts_present: vec!["test_*.py".to_string(), "*_test.py".to_string()],
            scope: ValidationScope::TargetFiles,
            stage: ValidationStage::PreIntegration,
            must_pass: true,
        };
        let targets = vec!["test_main.py".to_string()];

        let result = plan(vec![s]).execute_scoped(&ws, &targets, &[]);

        assert!(result.passed, "matching test target should run pytest step");
        assert!(path.join("pytest-ran").exists());
        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn target_file_guard_skips_glob_gated_step_for_non_matching_target() {
        // Invariant: a target-scoped step declared with `when_artifacts_present`
        // is skipped when none of the node's target files match the glob.
        //
        // This is a generic `ValidationPlan` mechanism test only — it does not
        // describe how `pass_tests` decides whether to run pytest in
        // production. `pass_tests` gets its own `key` in
        // `plugins/python.yaml` whose pytest step is workspace-scoped and
        // carries no `when_artifacts_present` gate at all, so its pytest step
        // always runs regardless of the node's target files. This gate exists
        // for steps that genuinely should be skipped based on which files a
        // node targets (e.g. a workspace bootstrap step keyed to a specific
        // target), not for deciding whether a test suite has already been
        // written by a sibling team.
        let (path, ws) = temp_workspace();
        let s = ValidationStep {
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "touch pytest-ran".to_string(),
                "pytest".to_string(),
            ],
            when_artifacts_present: vec!["test_*.py".to_string(), "*_test.py".to_string()],
            scope: ValidationScope::TargetFiles,
            stage: ValidationStage::PreIntegration,
            must_pass: true,
        };
        let targets = vec!["main.py".to_string()];

        let result = plan(vec![s]).execute_scoped(&ws, &targets, &[]);

        assert!(result.passed, "non-matching source target should skip step");
        assert!(!path.join("pytest-ran").exists());
        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn pass_tests_shaped_workspace_scoped_step_runs_and_reports_real_failure() {
        // Invariant / regression: reconstructs the bug where a pass_tests-shaped
        // node (target_files = ["main.py"], a source file, not a test file)
        // never actually ran its pytest-like step — the old shared
        // `implementer` role gated that step on `when_artifacts_present`
        // matched against `target_files`, which a source-only node's targets
        // never satisfy, so the step was always skipped and the plan
        // trivially "passed" no matter what a real test run would have found.
        //
        // `pass_tests` now gets its own `key` (see plugins/python.yaml)
        // whose pytest step is workspace-scoped with no `when_artifacts_present`
        // gate at all. This test builds a step of that exact shape and proves
        // it actually executes — regardless of target_files being source-only,
        // and regardless of which files exist in the workspace — and that a
        // real failing exit code is reflected in the returned ValidationResult,
        // not just that a gate would have allowed it to run.
        let (path, ws) = temp_workspace();
        std::fs::create_dir_all(path.join("tests")).unwrap();
        std::fs::write(
            path.join("tests").join("test_main.py"),
            "# a real test file\n",
        )
        .unwrap();
        let s = ValidationStep {
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "touch pytest-ran; exit 1".to_string(),
                "pytest".to_string(),
            ],
            when_artifacts_present: vec![],
            scope: ValidationScope::Workspace,
            stage: ValidationStage::PreIntegration,
            must_pass: true,
        };
        let targets = vec!["main.py".to_string()];

        let result = plan(vec![s]).execute_scoped(&ws, &targets, &[]);

        assert!(
            path.join("pytest-ran").exists(),
            "pass_tests-shaped step must actually run for a source-only target, not be skipped"
        );
        assert!(
            !result.passed,
            "a real failing exit code must be reflected in the plan result, not swallowed"
        );
        assert!(
            result.failure.is_some(),
            "a failing must_pass step must populate ValidationResult::failure"
        );
        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn checkpoint_roundtrip_preserves_validation_plan() {
        // Invariant: a ValidationPlan serialized to JSON and deserialized is
        // identical to the original — checkpoint/resume preserves the plan.
        let original = ValidationPlan {
            steps: vec![
                ValidationStep {
                    command: vec!["cargo".to_string(), "test".to_string()],
                    when_artifacts_present: vec!["*_test.rs".to_string()],
                    scope: ValidationScope::TargetFiles,
                    stage: ValidationStage::PreIntegration,
                    must_pass: true,
                },
                ValidationStep {
                    command: vec![
                        "cargo".to_string(),
                        "fmt".to_string(),
                        "--check".to_string(),
                    ],
                    when_artifacts_present: vec![],
                    scope: ValidationScope::Workspace,
                    stage: ValidationStage::PreIntegration,
                    must_pass: false,
                },
            ],
            timeout_seconds: 60,
        };
        let json = serde_json::to_string(&original).expect("must serialize");
        let restored: ValidationPlan = serde_json::from_str(&json).expect("must deserialize");
        assert_eq!(
            original, restored,
            "checkpoint roundtrip must preserve ValidationPlan exactly"
        );
    }
}
