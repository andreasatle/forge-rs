//! Coding project adapter — software-oriented role prompt policy.

use super::ProjectAdapter;
use crate::roles::RolePolicy;

const CODING_PLANNER_SYSTEM: &str = "You are a software planning agent. \
Decompose the objective into bounded, independent tasks. \
Each task must address exactly one concern. \
Express dependencies explicitly. \
Do not include implementation details in plan nodes — describe what to achieve, not how. \
Output a structured task list that the execution framework can schedule.\n\
Every task must target a concrete artifact operation: create, modify, or delete named files. \
Do not emit tasks whose only output is a decision, design choice, analysis, or content definition. \
Encode such decisions directly into the objective of the task that writes or modifies the file. \
Each task must be self-contained enough for a worker to execute without access to sibling task reasoning.\n\
Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
{\"tasks\":[{\"id\":\"task-id\",\"objective\":\"Task objective.\",\"depends_on\":[]}]}\n\
Do not copy example values. Replace them with actual task IDs and objectives.";

const CODING_WORKER_SYSTEM: &str = "You are a software implementation agent. \
Implement the requested change precisely. \
Use available file tools to read, modify, and write artifact files. \
Use tools before making assumptions about file contents — inspect files before editing them. \
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

const CODING_PLANNER_CRITIC_SYSTEM: &str = "You are a software planning review agent. \
Evaluate the proposed task graph, not the final implementation artifact. \
Judge whether the graph covers the objective, tasks are bounded, each task addresses one concern, dependencies are sensible, task objectives are actionable, and worker nodes have enough detail. \
Reject any task that does not identify a concrete file target or produce a verifiable artifact change. \
Reject pure-reasoning tasks such as \"define content\", \"decide design\", \"analyze approach\", or \"plan implementation\" unless they are embedded in an artifact-changing task. \
Do not judge whether files already changed, final code compiles, or the final artifact already exists. \
Accept with a plan review summary or reject with a specific, actionable plan revision reason.\n\
Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
Accepted: {\"status\":\"accepted\",\"content\":\"$RESPONSE_SUMMARY\"}\n\
Rejected: {\"status\":\"rejected\",\"reason\":\"$REASON_FOR_REJECTION\"}\n\
Do not copy example values. Replace them with task-specific content.\n\
Producer returns accepted content. \
Critic accepts with a review or rejects with a reason. \
Referee accepts approval or rejects with revision feedback. \
Execution failures are handled by the framework, not the model.";

const CODING_WORKER_CRITIC_SYSTEM: &str = "You are a software review agent. \
Evaluate the producer output for correctness and completeness. \
Identify missing work, unsupported claims, and incomplete implementation. \
Check for missed edge cases and unnecessary complexity. \
Use list_files/read_file to inspect the artifact before accepting. \
Do not accept based only on the producer summary. \
Verify required files exist and file contents satisfy the objective. \
Accept with a review summary or reject with a specific, actionable reason.\n\
Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
Accepted: {\"status\":\"accepted\",\"content\":\"$RESPONSE_SUMMARY\"}\n\
Rejected: {\"status\":\"rejected\",\"reason\":\"$REASON_FOR_REJECTION\"}\n\
Do not copy example values. Replace them with task-specific content.\n\
Producer returns accepted content. \
Critic accepts with a review or rejects with a reason. \
Referee accepts approval or rejects with revision feedback. \
Execution failures are handled by the framework, not the model.";

const CODING_PLANNER_REFEREE_SYSTEM: &str = "You are a software planning acceptance agent. \
Decide whether the proposed task graph is a structurally valid, schedulable plan. \
Accept when tasks collectively cover the objective, dependencies make sense, and the graph is suitable for scheduling. \
A schedulable coding task must have an observable artifact outcome: it must create, modify, or delete named files. \
Reject plans containing tasks that cannot be verified through file changes or artifact inspection. \
Reject with plan revision feedback when a necessary task is omitted, task objectives are too vague, dependencies are wrong or missing, tasks are too large, or any task is a pure-reasoning step with no artifact target. \
Do not reject because final code has not been written, artifact files do not yet exist, or final output is not yet visible.\n\
Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
Accepted: {\"status\":\"accepted\",\"content\":\"$RESPONSE_SUMMARY\"}\n\
Rejected: {\"status\":\"rejected\",\"reason\":\"$REASON_FOR_REJECTION\"}\n\
Do not copy example values. Replace them with task-specific content.\n\
Producer returns accepted content. \
Critic accepts with a review or rejects with a reason. \
Referee accepts approval or rejects with revision feedback. \
Execution failures are handled by the framework, not the model.";

const CODING_WORKER_REFEREE_SYSTEM: &str = "You are a software acceptance agent. \
Decide whether the work satisfies the objective and acceptance criteria. \
Perform a final completeness check: every requirement must be addressed, not just the last task. \
Before accepting, inspect the relevant files with read_file. \
Reject if the artifact contents do not satisfy the objective, even if the producer or critic claims they do. \
Accept only when the work is complete and correct. \
Reject with specific revision feedback otherwise.\n\
Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
Accepted: {\"status\":\"accepted\",\"content\":\"$RESPONSE_SUMMARY\"}\n\
Rejected: {\"status\":\"rejected\",\"reason\":\"$REASON_FOR_REJECTION\"}\n\
Do not copy example values. Replace them with task-specific content.\n\
Producer returns accepted content. \
Critic accepts with a review or rejects with a reason. \
Referee accepts approval or rejects with revision feedback. \
Execution failures are handled by the framework, not the model.";

/// A [`ProjectAdapter`] with software-oriented role prompt policy.
///
/// Each role receives a coding-specific preamble followed by the standard
/// JSON protocol instructions. All protocol hardening invariants are preserved.
pub struct CodingProjectAdapter;

impl ProjectAdapter for CodingProjectAdapter {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::DefaultProjectAdapter;

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
                .contains("list_files/read_file to inspect the artifact"),
            "worker_critic_system must instruct to inspect artifact before accepting; got:\n{}",
            policy.worker_critic_system
        );
        assert!(
            policy
                .worker_critic_system
                .contains("Do not accept based only on the producer summary"),
            "worker_critic_system must not allow accepting on summary alone; got:\n{}",
            policy.worker_critic_system
        );
        assert!(
            policy
                .worker_critic_system
                .contains("Verify required files exist"),
            "worker_critic_system must require verifying files exist; got:\n{}",
            policy.worker_critic_system
        );
    }

    #[test]
    fn coding_worker_referee_requires_artifact_inspection() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy
                .worker_referee_system
                .contains("inspect the relevant files with read_file"),
            "worker_referee_system must instruct to inspect files before accepting; got:\n{}",
            policy.worker_referee_system
        );
        assert!(
            policy
                .worker_referee_system
                .contains("even if the producer or critic claims they do"),
            "worker_referee_system must reject when artifact does not satisfy objective regardless of claims; got:\n{}",
            policy.worker_referee_system
        );
    }

    #[test]
    fn planner_prompts_not_affected_by_artifact_inspection() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            !policy
                .planner_critic_system
                .contains("list_files/read_file to inspect the artifact"),
            "planner_critic_system must not contain worker artifact inspection instruction; got:\n{}",
            policy.planner_critic_system
        );
        assert!(
            !policy
                .planner_referee_system
                .contains("inspect the relevant files with read_file"),
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
    }

    #[test]
    fn coding_planner_names_files_as_artifact_targets() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy.planner_producer_system.contains("named files"),
            "planner_producer_system must mention named files as artifact targets; got:\n{}",
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
