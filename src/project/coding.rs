//! Coding project adapter — software-oriented role prompt policy.

use super::{ProjectAdapter, build_file_text_target_views};
use crate::artifacts::ArtifactRead;
use crate::machines::deliberation::DeliberationRole;
use crate::roles::{RolePolicy, TargetView};

const CODING_PLANNER_SYSTEM: &str = "You are a software planning agent. \
Decompose the objective into bounded, independent tasks. \
Each task must address exactly one concern. \
Express dependencies explicitly. \
Do not include implementation details in plan nodes — describe what to achieve, not how. \
Output a structured task list that the execution framework can schedule.\n\
Every task must target a concrete artifact operation: create, modify, or delete named files. \
Every task must include `operation` as exactly one of \"create\", \"modify\", or \"delete\". \
Every task must include a non-empty `targets` array listing the exact files that task may create, modify, or delete. \
Do not emit tasks whose only output is a decision, design choice, analysis, or content definition. \
Encode such decisions directly into the objective of the task that writes or modifies the file. \
Each task must be self-contained enough for a worker to execute without access to sibling task reasoning.\n\
Files shown in the project context under 'Existing project files' already exist and are managed \
by the project infrastructure. \
Do not put those existing files in a task's `targets` unless the objective explicitly names them as targets. \
Only create tasks for files that the objective names as targets or that must be newly created \
to satisfy it. \
When project validation includes a test command, code-change plans must include at least one \
test-related task with an explicit test target.\n\
Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
{\"tasks\":[{\"id\":\"task-id\",\"objective\":\"Task objective.\",\"operation\":\"modify\",\"targets\":[\"path/to/file\"],\"depends_on\":[]}]}\n\
Do not copy example values. Replace them with actual task IDs and objectives.";

const CODING_WORKER_SYSTEM: &str = "You are a software implementation agent. \
Implement the requested change precisely. \
Use available file tools to read, modify, and write artifact files. \
Use tools before making assumptions about file contents — inspect files before editing them. \
Code changes require corresponding tests; create or update test files that verify the changed behavior. \
Produce concrete, complete artifact changes — do not leave placeholders or stubs.\n\
Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
Accepted: {\"status\":\"accepted\",\"content\":\"$RESPONSE_SUMMARY\"}\n\
Rejected: {\"status\":\"rejected\",\"reason\":\"$REASON_FOR_REJECTION\"}\n\
Do not copy example values. Replace them with task-specific content.\n\
Producer returns accepted content. \
Critic accepts with a review or rejects with a reason. \
Referee accepts approval or rejects with revision feedback. \
Execution failures are handled by the framework, not the model.";

macro_rules! reviewer_contract_guidance {
    () => {
        "Evaluate the current node contract, not the entire project state. \
Distinguish current node deliverables, planned follow-up deliverables, and overall project completion. \
Ground every rejection in the current node objective, declared target files, plan metadata, adapter policy, validation contract, or observable artifact correctness. \
Do not reject solely for unstated preferences about style, algorithm, architecture, or performance. \
For example, do not reject recursive code solely because an iterative version might be faster unless the contract requires iteration or a performance bound. \
If you have a style or performance concern outside the contract, mention it in accepted content as advisory only."
    };
}

const CODING_PLANNER_CRITIC_SYSTEM: &str = concat!(
    "You are a software planning review agent. \
Evaluate the proposed task graph, not the final implementation artifact. \
Judge whether the graph covers the objective, tasks are bounded, each task addresses one concern, dependencies are sensible, task objectives are actionable, and worker nodes have enough detail. \
Reject any task that does not identify a concrete file target or produce a verifiable artifact change. \
Reject pure-reasoning tasks such as \"define content\", \"decide design\", \"analyze approach\", or \"plan implementation\" unless they are embedded in an artifact-changing task. \
Do not judge whether files already changed, final code compiles, or the final artifact already exists. \
",
    reviewer_contract_guidance!(),
    " \
Accept with a plan review summary or reject with a specific, actionable plan revision reason.\n\
Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
Accepted: {\"status\":\"accepted\",\"content\":\"$RESPONSE_SUMMARY\"}\n\
Rejected: {\"status\":\"rejected\",\"reason\":\"$REASON_FOR_REJECTION\"}\n\
Do not copy example values. Replace them with task-specific content.\n\
Producer returns accepted content. \
Critic accepts with a review or rejects with a reason. \
Referee accepts approval or rejects with revision feedback. \
Execution failures are handled by the framework, not the model."
);

const CODING_WORKER_CRITIC_SYSTEM: &str = concat!(
    "You are a software review agent. \
Evaluate the producer output for correctness and completeness. \
Identify missing work, unsupported claims, and incomplete implementation. \
Check for missed edge cases and unnecessary complexity. \
Apply the rendered node review contract for current-node test and follow-up acceptance scope. \
Use read_file to inspect the specific files the producer was expected to modify. \
Do not accept based only on the producer summary or on file existence from list_files. \
Verify actual file contents satisfy the objective. \
",
    reviewer_contract_guidance!(),
    " \
Accept with a review summary or reject with a specific, actionable reason.\n\
Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
Accepted: {\"status\":\"accepted\",\"content\":\"$RESPONSE_SUMMARY\"}\n\
Rejected: {\"status\":\"rejected\",\"reason\":\"$REASON_FOR_REJECTION\"}\n\
Do not copy example values. Replace them with task-specific content.\n\
Producer returns accepted content. \
Critic accepts with a review or rejects with a reason. \
Referee accepts approval or rejects with revision feedback. \
Execution failures are handled by the framework, not the model."
);

const CODING_PLANNER_REFEREE_SYSTEM: &str = concat!(
    "You are a software planning acceptance agent. \
Decide whether the proposed task graph is a structurally valid, schedulable plan. \
Accept when tasks collectively cover the objective, dependencies make sense, and the graph is suitable for scheduling. \
A schedulable coding task must have an observable artifact outcome: it must create, modify, or delete named files. \
Reject plans containing tasks that cannot be verified through file changes or artifact inspection. \
Reject with plan revision feedback when a necessary task is omitted, task objectives are too vague, dependencies are wrong or missing, tasks are too large, or any task is a pure-reasoning step with no artifact target. \
Do not reject because final code has not been written, artifact files do not yet exist, or final output is not yet visible.\n\
",
    reviewer_contract_guidance!(),
    "\n\
Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
Accepted: {\"status\":\"accepted\",\"content\":\"$RESPONSE_SUMMARY\"}\n\
Rejected: {\"status\":\"rejected\",\"reason\":\"$REASON_FOR_REJECTION\"}\n\
Do not copy example values. Replace them with task-specific content.\n\
Producer returns accepted content. \
Critic accepts with a review or rejects with a reason. \
Referee accepts approval or rejects with revision feedback. \
Execution failures are handled by the framework, not the model."
);

const CODING_WORKER_REFEREE_SYSTEM: &str = concat!(
    "You are a software acceptance agent. \
Decide whether the work satisfies the objective and acceptance criteria. \
Perform a final completeness check: every requirement must be addressed, not just the last task. \
Before accepting, use read_file to inspect the specific files the producer was expected to modify. \
Apply the rendered node review contract for current-node test and follow-up acceptance scope. \
Do not rely on list_files to verify completion — file existence is not evidence of correct content. \
Reject if the artifact contents do not satisfy the objective, even if the producer or critic claims they do. \
Accept only when the work is complete and correct. \
",
    reviewer_contract_guidance!(),
    " \
Reject with specific revision feedback otherwise.\n\
Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
Accepted: {\"status\":\"accepted\",\"content\":\"$RESPONSE_SUMMARY\"}\n\
Rejected: {\"status\":\"rejected\",\"reason\":\"$REASON_FOR_REJECTION\"}\n\
Do not copy example values. Replace them with task-specific content.\n\
Producer returns accepted content. \
Critic accepts with a review or rejects with a reason. \
Referee accepts approval or rejects with revision feedback. \
Execution failures are handled by the framework, not the model."
);

fn is_code_file(target: &str) -> bool {
    let extension = target
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        extension.as_str(),
        "c" | "cc"
            | "cpp"
            | "cs"
            | "go"
            | "java"
            | "js"
            | "jsx"
            | "kt"
            | "m"
            | "mm"
            | "php"
            | "py"
            | "rb"
            | "rs"
            | "scala"
            | "swift"
            | "ts"
            | "tsx"
    )
}

fn is_test_file(target: &str) -> bool {
    let path = target.replace('\\', "/").to_ascii_lowercase();
    let filename = path.rsplit('/').next().unwrap_or(path.as_str());
    path.contains("/test/")
        || path.contains("/tests/")
        || path.starts_with("test/")
        || path.starts_with("tests/")
        || filename.starts_with("test_")
        || filename.starts_with("test-")
        || filename.ends_with("_test.rs")
        || filename.ends_with("_tests.rs")
        || filename.contains("_test.")
        || filename.contains("-test.")
        || filename.contains(".test.")
        || filename.contains("_tests.")
        || filename.contains("-tests.")
        || filename.contains(".spec.")
}

fn derive_test_path(source: &str) -> String {
    let path = source.replace('\\', "/");
    let (prefix, filename) = path
        .rsplit_once('/')
        .map(|(dir, f)| (format!("{dir}/"), f))
        .unwrap_or(("".into(), path.as_str()));
    let Some((stem, ext)) = filename.rsplit_once('.') else {
        return format!("{prefix}test_{filename}");
    };
    let lower = ext.to_ascii_lowercase();
    match lower.as_str() {
        "go" | "rs" => format!("{prefix}{stem}_test.{ext}"),
        "js" | "ts" | "jsx" | "tsx" => format!("{prefix}{stem}.test.{ext}"),
        _ => format!("{prefix}test_{stem}.{ext}"),
    }
}

/// A [`ProjectAdapter`] with software-oriented role prompt policy.
///
/// Each role receives a coding-specific preamble followed by the standard
/// JSON protocol instructions. All protocol hardening invariants are preserved.
pub struct CodingProjectAdapter;

impl ProjectAdapter for CodingProjectAdapter {
    fn required_test_targets(&self, targets: &[String]) -> Vec<String> {
        targets
            .iter()
            .filter(|t| is_code_file(t) && !is_test_file(t))
            .map(|t| derive_test_path(t))
            .collect()
    }

    fn role_policy(&self) -> RolePolicy {
        RolePolicy {
            planner_producer_system: CODING_PLANNER_SYSTEM.to_string(),
            worker_producer_system: CODING_WORKER_SYSTEM.to_string(),
            planner_critic_system: CODING_PLANNER_CRITIC_SYSTEM.to_string(),
            worker_critic_system: CODING_WORKER_CRITIC_SYSTEM.to_string(),
            planner_referee_system: CODING_PLANNER_REFEREE_SYSTEM.to_string(),
            worker_referee_system: CODING_WORKER_REFEREE_SYSTEM.to_string(),
        }
    }

    fn build_target_views(
        &self,
        artifact_view: &dyn ArtifactRead,
        targets: &[String],
        _role: &DeliberationRole,
        budget: usize,
    ) -> Vec<TargetView> {
        build_file_text_target_views(artifact_view, targets, budget)
    }

    fn context_file_names(&self) -> Vec<String> {
        vec!["README.md".to_string()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::DefaultProjectAdapter;

    // ── required_test_targets ────────────────────────────────────────────────

    #[test]
    fn required_test_targets_derives_python_test() {
        // Invariant: Python source files produce test_ prefixed counterparts.
        assert_eq!(
            CodingProjectAdapter.required_test_targets(&["main.py".to_string()]),
            vec!["test_main.py".to_string()],
        );
    }

    #[test]
    fn required_test_targets_derives_rust_test() {
        // Invariant: Rust source files produce _test.rs suffixed counterparts.
        assert_eq!(
            CodingProjectAdapter.required_test_targets(&["lib.rs".to_string()]),
            vec!["lib_test.rs".to_string()],
        );
    }

    #[test]
    fn required_test_targets_derives_go_test() {
        // Invariant: Go source files produce _test.go suffixed counterparts.
        assert_eq!(
            CodingProjectAdapter.required_test_targets(&["server.go".to_string()]),
            vec!["server_test.go".to_string()],
        );
    }

    #[test]
    fn required_test_targets_derives_js_test() {
        // Invariant: JS/TS source files produce .test.ext counterparts.
        let cases: &[(&str, &str)] = &[
            ("util.js", "util.test.js"),
            ("component.ts", "component.test.ts"),
            ("widget.tsx", "widget.test.tsx"),
            ("app.jsx", "app.test.jsx"),
        ];
        for (source, expected) in cases {
            assert_eq!(
                CodingProjectAdapter.required_test_targets(&[source.to_string()]),
                vec![expected.to_string()],
                "wrong test target for {source}"
            );
        }
    }

    #[test]
    fn required_test_targets_excludes_test_files() {
        // Invariant: test files are not themselves source files requiring tests.
        for test_file in &[
            "test_main.py",
            "lib_test.rs",
            "server_test.go",
            "util.test.js",
        ] {
            let result = CodingProjectAdapter.required_test_targets(&[test_file.to_string()]);
            assert!(
                result.is_empty(),
                "test file {test_file} must not produce additional test targets; got: {result:?}"
            );
        }
    }

    #[test]
    fn required_test_targets_excludes_non_code_files() {
        // Invariant: non-code files (docs, config) have no test targets.
        for non_code in &["README.md", "config.yaml", "pyproject.toml", "Cargo.lock"] {
            let result = CodingProjectAdapter.required_test_targets(&[non_code.to_string()]);
            assert!(
                result.is_empty(),
                "non-code file {non_code} must produce no test targets; got: {result:?}"
            );
        }
    }

    #[test]
    fn required_test_targets_preserves_directory_prefix() {
        // Invariant: directory prefix is preserved in derived test path.
        assert_eq!(
            CodingProjectAdapter.required_test_targets(&["src/main.py".to_string()]),
            vec!["src/test_main.py".to_string()],
        );
        assert_eq!(
            CodingProjectAdapter.required_test_targets(&["pkg/server.go".to_string()]),
            vec!["pkg/server_test.go".to_string()],
        );
        assert_eq!(
            CodingProjectAdapter.required_test_targets(&["lib/util.rs".to_string()]),
            vec!["lib/util_test.rs".to_string()],
        );
    }

    #[test]
    fn required_test_targets_handles_multiple_sources() {
        // Invariant: each source file independently produces its test target.
        let mut result = CodingProjectAdapter
            .required_test_targets(&["main.py".to_string(), "utils.rs".to_string()]);
        result.sort();
        let mut expected = vec!["test_main.py".to_string(), "utils_test.rs".to_string()];
        expected.sort();
        assert_eq!(result, expected);
    }

    #[test]
    fn required_test_targets_mixed_source_and_test_files() {
        // Invariant: test files in the input are excluded; only source files get targets.
        let targets = vec![
            "main.py".to_string(),
            "test_main.py".to_string(),
            "lib.rs".to_string(),
        ];
        let mut result = CodingProjectAdapter.required_test_targets(&targets);
        result.sort();
        let mut expected = vec!["test_main.py".to_string(), "lib_test.rs".to_string()];
        expected.sort();
        assert_eq!(result, expected);
    }

    #[test]
    fn required_test_targets_empty_input_returns_empty() {
        // Invariant: empty input always returns empty output.
        assert!(CodingProjectAdapter.required_test_targets(&[]).is_empty());
    }

    #[test]
    fn coding_adapter_role_policy_differs_from_default() {
        let coding = CodingProjectAdapter.role_policy();
        let default = DefaultProjectAdapter.role_policy();
        assert_ne!(
            coding.planner_producer_system, default.planner_producer_system,
            "coding planner_producer_system must differ from default"
        );
        assert_ne!(
            coding.worker_producer_system, default.worker_producer_system,
            "coding worker_producer_system must differ from default"
        );
    }

    #[test]
    fn coding_adapter_preserves_json_protocol_invariants() {
        let policy = CodingProjectAdapter.role_policy();
        // All non-planner-producer roles use the status/content wrapper schema.
        for (label, system) in [
            ("worker", policy.worker_producer_system.as_str()),
            ("planner critic", policy.planner_critic_system.as_str()),
            ("worker critic", policy.worker_critic_system.as_str()),
            ("planner referee", policy.planner_referee_system.as_str()),
            ("worker referee", policy.worker_referee_system.as_str()),
        ] {
            assert!(
                system.contains("\"status\""),
                "{label} system must contain JSON status field; got:\n{system}"
            );
            assert!(
                system.contains("Do not copy example values"),
                "{label} system must include copy-guard instruction; got:\n{system}"
            );
            assert!(
                !system.contains("\"...\""),
                "{label} system must not contain dot-placeholder JSON values; got:\n{system}"
            );
            assert!(
                system.contains("$RESPONSE_SUMMARY"),
                "{label} system must include accepted schema placeholder; got:\n{system}"
            );
            assert!(
                system.contains("$REASON_FOR_REJECTION"),
                "{label} system must include rejected schema placeholder; got:\n{system}"
            );
        }
        // Planner uses direct PlannerOutput schema — no status/content wrapper.
        assert!(
            policy.planner_producer_system.contains("\"tasks\""),
            "planner system must show direct tasks schema; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            !policy.planner_producer_system.contains("\"status\""),
            "planner system must not contain status/content wrapper; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            policy
                .planner_producer_system
                .contains("Do not copy example values"),
            "planner system must include copy-guard instruction; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            !policy.planner_producer_system.contains("\"...\""),
            "planner system must not contain dot-placeholder JSON values; got:\n{}",
            policy.planner_producer_system
        );
    }

    #[test]
    fn coding_planner_emphasizes_software_planning() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy.planner_producer_system.contains("software planning"),
            "planner_producer_system must mention software planning; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            policy.planner_producer_system.contains("bounded"),
            "planner_producer_system must mention bounded tasks; got:\n{}",
            policy.planner_producer_system
        );
    }

    #[test]
    fn coding_worker_emphasizes_implementation() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy
                .worker_producer_system
                .contains("software implementation"),
            "worker_producer_system must mention software implementation; got:\n{}",
            policy.worker_producer_system
        );
        assert!(
            policy.worker_producer_system.contains("file tools"),
            "worker_producer_system must mention file tools; got:\n{}",
            policy.worker_producer_system
        );
    }

    #[test]
    fn coding_planner_excludes_implementation_details() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy
                .planner_producer_system
                .contains("implementation details"),
            "planner_producer_system must instruct against implementation details; got:\n{}",
            policy.planner_producer_system
        );
    }

    #[test]
    fn coding_worker_inspects_before_editing() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy
                .worker_producer_system
                .contains("inspect files before editing"),
            "worker_producer_system must instruct to inspect files before editing; got:\n{}",
            policy.worker_producer_system
        );
        assert!(
            policy
                .worker_producer_system
                .contains("Use tools before making assumptions"),
            "worker_producer_system must instruct to use tools before making assumptions; got:\n{}",
            policy.worker_producer_system
        );
    }

    #[test]
    fn coding_worker_requires_tests_for_code_changes() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy
                .worker_producer_system
                .contains("Code changes require corresponding tests"),
            "worker_producer_system must require tests for code changes; got:\n{}",
            policy.worker_producer_system
        );
    }

    #[test]
    fn coding_critic_identifies_missing_work_and_unsupported_claims() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy.worker_critic_system.contains("missing work"),
            "worker_critic_system must mention missing work; got:\n{}",
            policy.worker_critic_system
        );
        assert!(
            policy.worker_critic_system.contains("unsupported claims"),
            "worker_critic_system must mention unsupported claims; got:\n{}",
            policy.worker_critic_system
        );
        assert!(
            policy
                .worker_critic_system
                .contains("incomplete implementation"),
            "worker_critic_system must mention incomplete implementation; got:\n{}",
            policy.worker_critic_system
        );
    }

    #[test]
    fn coding_planner_critic_does_not_require_final_artifact() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy.planner_critic_system.contains("proposed task graph"),
            "planner_critic_system must review the proposed task graph; got:\n{}",
            policy.planner_critic_system
        );
        assert!(
            policy
                .planner_critic_system
                .contains("not the final implementation artifact"),
            "planner_critic_system must not require final implementation; got:\n{}",
            policy.planner_critic_system
        );
        assert!(
            policy
                .planner_critic_system
                .contains("final artifact already exists"),
            "planner_critic_system must say artifact existence is out of scope; got:\n{}",
            policy.planner_critic_system
        );
    }

    #[test]
    fn coding_planner_referee_judges_plan_not_implementation() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy
                .planner_referee_system
                .contains("structurally valid, schedulable plan"),
            "planner_referee_system must judge schedulable plan structure; got:\n{}",
            policy.planner_referee_system
        );
        assert!(
            policy
                .planner_referee_system
                .contains("final code has not been written"),
            "planner_referee_system must not reject missing implementation; got:\n{}",
            policy.planner_referee_system
        );
        assert!(
            policy
                .planner_referee_system
                .contains("artifact files do not yet exist"),
            "planner_referee_system must not require artifact files; got:\n{}",
            policy.planner_referee_system
        );
    }

    #[test]
    fn worker_critic_still_judges_implementation() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy
                .worker_critic_system
                .contains("incomplete implementation"),
            "worker_critic_system must still judge implementation; got:\n{}",
            policy.worker_critic_system
        );
        assert!(
            !policy.worker_critic_system.contains("proposed task graph"),
            "worker_critic_system must not be the planner critic prompt; got:\n{}",
            policy.worker_critic_system
        );
    }

    #[test]
    fn worker_referee_still_judges_completion() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy
                .worker_referee_system
                .contains("work satisfies the objective"),
            "worker_referee_system must still judge completed work; got:\n{}",
            policy.worker_referee_system
        );
        assert!(
            policy
                .worker_referee_system
                .contains("work is complete and correct"),
            "worker_referee_system must still require completion; got:\n{}",
            policy.worker_referee_system
        );
    }

    #[test]
    fn coding_referee_performs_final_completeness_check() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy
                .worker_referee_system
                .contains("final completeness check"),
            "worker_referee_system must include a final completeness check instruction; got:\n{}",
            policy.worker_referee_system
        );
    }

    #[test]
    fn coding_worker_critic_requires_artifact_inspection() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy
                .worker_critic_system
                .contains("read_file to inspect the specific files"),
            "worker_critic_system must instruct to inspect specific files before accepting; got:\n{}",
            policy.worker_critic_system
        );
        assert!(
            policy
                .worker_critic_system
                .contains("Do not accept based only on the producer summary or on file existence from list_files"),
            "worker_critic_system must reject list_files-only acceptance; got:\n{}",
            policy.worker_critic_system
        );
        assert!(
            policy
                .worker_critic_system
                .contains("Verify actual file contents satisfy the objective"),
            "worker_critic_system must require verifying actual file contents; got:\n{}",
            policy.worker_critic_system
        );
    }

    #[test]
    fn coding_reviewers_must_ground_rejections_in_explicit_contract() {
        let policy = CodingProjectAdapter.role_policy();
        for (label, system) in [
            ("planner critic", policy.planner_critic_system.as_str()),
            ("worker critic", policy.worker_critic_system.as_str()),
            ("planner referee", policy.planner_referee_system.as_str()),
            ("worker referee", policy.worker_referee_system.as_str()),
        ] {
            assert!(
                system.contains("Evaluate the current node contract")
                    && system.contains("not the entire project state"),
                "{label} must scope review to the current node contract; got:\n{system}"
            );
            assert!(
                system.contains("current node deliverables")
                    && system.contains("planned follow-up deliverables")
                    && system.contains("overall project completion"),
                "{label} must distinguish current, follow-up, and project-completion scopes; got:\n{system}"
            );
            assert!(
                system.contains("Ground every rejection in the current node objective")
                    && system.contains("declared target files")
                    && system.contains("validation contract")
                    && system.contains("observable artifact correctness"),
                "{label} must name allowed rejection grounds; got:\n{system}"
            );
            assert!(
                system.contains(
                    "Do not reject solely for unstated preferences about style, algorithm, architecture, or performance"
                ),
                "{label} must forbid rejection on unstated preferences; got:\n{system}"
            );
            assert!(
                system.contains("mention it in accepted content as advisory only"),
                "{label} must allow non-contract style/performance concerns only as accepted-content advisory; got:\n{system}"
            );
        }
    }

    #[test]
    fn recursive_fibonacci_is_not_rejectable_for_unstated_iterative_preference() {
        let policy = CodingProjectAdapter.role_policy();
        for (label, system) in [
            ("worker critic", policy.worker_critic_system.as_str()),
            ("worker referee", policy.worker_referee_system.as_str()),
        ] {
            assert!(
                system.contains(
                    "do not reject recursive code solely because an iterative version might be faster"
                ),
                "{label} must not reject recursive Fibonacci solely for iterative-performance preference; got:\n{system}"
            );
            assert!(
                system.contains("unless the contract requires iteration or a performance bound"),
                "{label} must limit iterative/performance rejection to explicit requirements; got:\n{system}"
            );
        }
    }

    #[test]
    fn explicit_performance_or_iterative_requirement_remains_rejectable() {
        let policy = CodingProjectAdapter.role_policy();
        for (label, system) in [
            ("worker critic", policy.worker_critic_system.as_str()),
            ("worker referee", policy.worker_referee_system.as_str()),
        ] {
            assert!(
                system.contains("unless the contract requires iteration or a performance bound"),
                "{label} must permit rejection when iteration or performance is explicit; got:\n{system}"
            );
            assert!(
                system.contains("current node objective")
                    && system.contains("adapter policy")
                    && system.contains("validation contract"),
                "{label} must preserve explicit objective/policy/validation grounds; got:\n{system}"
            );
        }
    }

    #[test]
    fn coding_worker_referee_requires_artifact_inspection() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy
                .worker_referee_system
                .contains("read_file to inspect the specific files"),
            "worker_referee_system must instruct to inspect specific files before accepting; got:\n{}",
            policy.worker_referee_system
        );
        assert!(
            policy
                .worker_referee_system
                .contains("even if the producer or critic claims they do"),
            "worker_referee_system must reject when artifact does not satisfy objective regardless of claims; got:\n{}",
            policy.worker_referee_system
        );
        assert!(
            policy
                .worker_referee_system
                .contains("file existence is not evidence of correct content"),
            "worker_referee_system must warn against accepting based on file existence; got:\n{}",
            policy.worker_referee_system
        );
    }

    #[test]
    fn planner_prompts_not_affected_by_artifact_inspection() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            !policy
                .planner_critic_system
                .contains("read_file to inspect the specific files"),
            "planner_critic_system must not contain worker artifact inspection instruction; got:\n{}",
            policy.planner_critic_system
        );
        assert!(
            !policy
                .planner_referee_system
                .contains("file existence is not evidence of correct content"),
            "planner_referee_system must not contain worker artifact inspection instruction; got:\n{}",
            policy.planner_referee_system
        );
    }

    // ── artifact-operation invariant tests ───────────────────────────────────

    #[test]
    fn coding_planner_requires_concrete_artifact_operation() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy
                .planner_producer_system
                .contains("concrete artifact operation"),
            "planner_producer_system must require concrete artifact operations; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            policy.planner_producer_system.contains("`operation`"),
            "planner_producer_system must require structured operation; got:\n{}",
            policy.planner_producer_system
        );
    }

    #[test]
    fn coding_planner_names_files_as_artifact_targets() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy.planner_producer_system.contains("named files"),
            "planner_producer_system must mention named files as artifact targets; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            policy
                .planner_producer_system
                .contains("non-empty `targets` array"),
            "planner_producer_system must require non-empty targets; got:\n{}",
            policy.planner_producer_system
        );
    }

    #[test]
    fn coding_planner_prohibits_pure_reasoning_tasks() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy
                .planner_producer_system
                .contains("Do not emit tasks whose only output is a decision"),
            "planner_producer_system must prohibit pure-reasoning tasks; got:\n{}",
            policy.planner_producer_system
        );
    }

    #[test]
    fn coding_planner_does_not_recreate_existing_project_files() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy
                .planner_producer_system
                .contains("already exist and are managed by the project infrastructure"),
            "planner_producer_system must warn against recreating existing project files; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            policy
                .planner_producer_system
                .contains("explicitly names them as targets"),
            "planner_producer_system must say existing files are only targeted when objective names them; got:\n{}",
            policy.planner_producer_system
        );
    }

    #[test]
    fn coding_planner_mentions_test_targets_when_validation_tests() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy
                .planner_producer_system
                .contains("validation includes a test command"),
            "planner_producer_system must require test targets when validation runs tests; got:\n{}",
            policy.planner_producer_system
        );
    }

    #[test]
    fn coding_planner_critic_rejects_pure_reasoning_tasks() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy
                .planner_critic_system
                .contains("Reject pure-reasoning tasks"),
            "planner_critic_system must instruct to reject pure-reasoning tasks; got:\n{}",
            policy.planner_critic_system
        );
    }

    #[test]
    fn coding_planner_critic_requires_file_target() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy
                .planner_critic_system
                .contains("concrete file target"),
            "planner_critic_system must require a concrete file target; got:\n{}",
            policy.planner_critic_system
        );
    }

    #[test]
    fn coding_planner_referee_requires_observable_artifact_outcome() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy
                .planner_referee_system
                .contains("observable artifact outcome"),
            "planner_referee_system must require observable artifact outcome; got:\n{}",
            policy.planner_referee_system
        );
    }

    #[test]
    fn coding_planner_referee_rejects_unverifiable_tasks() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy
                .planner_referee_system
                .contains("cannot be verified through file changes"),
            "planner_referee_system must reject tasks not verifiable through file changes; got:\n{}",
            policy.planner_referee_system
        );
    }

    #[test]
    fn coding_worker_reviewers_defer_test_scope_to_node_review_contract() {
        let policy = CodingProjectAdapter.role_policy();
        for (label, system) in [
            ("worker critic", policy.worker_critic_system.as_str()),
            ("worker referee", policy.worker_referee_system.as_str()),
        ] {
            assert!(
                system.contains(
                    "Apply the rendered node review contract for current-node test and follow-up acceptance scope"
                ),
                "{label} must defer test/follow-up scope to the typed review contract; got:\n{system}"
            );
            assert!(
                !system.contains("Reject code changes that do not include corresponding tests")
                    && !system.contains("Reject code changes that omit corresponding tests")
                    && !system.contains("do not reject the current source-only node solely because those test files do not exist yet"),
                "{label} must not duplicate concrete test acceptance rules outside the review contract; got:\n{system}"
            );
        }
    }

    #[test]
    fn coding_planner_self_contained_task_requirement() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy.planner_producer_system.contains("self-contained"),
            "planner_producer_system must require self-contained task objectives; got:\n{}",
            policy.planner_producer_system
        );
    }

    #[test]
    fn coding_prompts_contain_same_protocol_footer_as_default() {
        let coding = CodingProjectAdapter.role_policy();
        let default = DefaultProjectAdapter.role_policy();
        // Every coding system string must contain the same key invariant
        // strings as the default policy to ensure equal protocol hardening.
        for system in [
            coding.planner_producer_system.as_str(),
            coding.worker_producer_system.as_str(),
            coding.planner_critic_system.as_str(),
            coding.worker_critic_system.as_str(),
            coding.planner_referee_system.as_str(),
            coding.worker_referee_system.as_str(),
        ] {
            assert_eq!(
                system.contains("Do not copy example values"),
                default
                    .worker_producer_system
                    .contains("Do not copy example values"),
                "coding system must carry the same copy-guard as the default policy"
            );
            assert_eq!(
                system.contains("Return exactly one JSON object"),
                default
                    .worker_producer_system
                    .contains("Return exactly one JSON object"),
                "coding system must carry the same JSON-only instruction as the default policy"
            );
        }
    }
}
