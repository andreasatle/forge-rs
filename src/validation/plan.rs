//! Node-owned validation plan executed before artifact integration.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::artifacts::Workspace;

use super::validator::{run_command_with_timeout, workspace_has_matching_file};
use super::{CommandSpec, ValidationResult};

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
    /// Execute all `PreIntegration` steps against `workspace`.
    ///
    /// Steps are executed in order.  A step with a non-empty
    /// `when_artifacts_present` list is skipped when no workspace file matches
    /// any of its patterns.  On the first `must_pass` step that fails, the
    /// failure is returned immediately.  Non-`must_pass` failures are ignored
    /// and execution continues.
    pub fn execute(&self, workspace: &Workspace) -> ValidationResult {
        let timeout = Duration::from_secs(self.timeout_seconds);
        let mut ran = 0usize;
        for step in &self.steps {
            if step.stage != ValidationStage::PreIntegration {
                continue;
            }
            if step.command.is_empty() {
                continue;
            }
            if !step.when_artifacts_present.is_empty()
                && !workspace_has_matching_file(workspace.path(), &step.when_artifacts_present)
            {
                continue;
            }
            ran += 1;
            let spec = CommandSpec {
                program: step.command[0].clone(),
                args: step.command[1..].to_vec(),
                when_files_present: vec![],
            };
            let result = run_command_with_timeout(&spec, workspace.path(), timeout);
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
    fn non_pre_integration_stage_is_skipped() {
        // Invariant: steps whose stage is not PreIntegration are not executed.
        // (Forward compat: future stages don't break existing plans.)
        let (_path, ws) = temp_workspace();
        // We can't construct a non-PreIntegration stage yet since it's the only
        // variant, but we can verify the filter passes for PreIntegration.
        let s = ValidationStep {
            command: vec!["true".to_string()],
            when_artifacts_present: vec![],
            stage: ValidationStage::PreIntegration,
            must_pass: true,
        };
        let result = plan(vec![s]).execute(&ws);
        assert!(result.passed, "PreIntegration step must run");
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
