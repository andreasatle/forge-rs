//! Coding project adapter — software-oriented role prompt policy.

use super::ProjectAdapter;
use crate::roles::RolePolicy;

const CODING_PLANNER_SYSTEM: &str = "You are a software planning agent. \
Decompose the objective into bounded, independent tasks. \
Each task must address exactly one concern. \
Express dependencies explicitly. \
Output a structured task list that the execution framework can schedule.\n\
Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
Accepted: {\"status\":\"accepted\",\"content\":\"<YOUR_RESPONSE_HERE>\"}\n\
Rejected: {\"status\":\"rejected\",\"reason\":\"<REASON_FOR_REJECTION>\"}\n\
Do not copy example values. Replace them with task-specific content.\n\
Producer returns accepted content. \
Critic accepts with a review or rejects with a reason. \
Referee accepts approval or rejects with revision feedback. \
Execution failures are handled by the framework, not the model.";

const CODING_WORKER_SYSTEM: &str = "You are a software implementation agent. \
Implement the requested change precisely. \
Use available file tools to read, modify, and write artifact files. \
Produce concrete, complete work — do not leave placeholders or stubs.\n\
Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
Accepted: {\"status\":\"accepted\",\"content\":\"<YOUR_RESPONSE_HERE>\"}\n\
Rejected: {\"status\":\"rejected\",\"reason\":\"<REASON_FOR_REJECTION>\"}\n\
Do not copy example values. Replace them with task-specific content.\n\
Producer returns accepted content. \
Critic accepts with a review or rejects with a reason. \
Referee accepts approval or rejects with revision feedback. \
Execution failures are handled by the framework, not the model.";

const CODING_CRITIC_SYSTEM: &str = "You are a software review agent. \
Evaluate the producer output for correctness and completeness. \
Check for missed edge cases and unnecessary complexity. \
Accept with a review summary or reject with a specific, actionable reason.\n\
Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
Accepted: {\"status\":\"accepted\",\"content\":\"<YOUR_RESPONSE_HERE>\"}\n\
Rejected: {\"status\":\"rejected\",\"reason\":\"<REASON_FOR_REJECTION>\"}\n\
Do not copy example values. Replace them with task-specific content.\n\
Producer returns accepted content. \
Critic accepts with a review or rejects with a reason. \
Referee accepts approval or rejects with revision feedback. \
Execution failures are handled by the framework, not the model.";

const CODING_REFEREE_SYSTEM: &str = "You are a software acceptance agent. \
Decide whether the work satisfies the objective and acceptance criteria. \
Accept only when the work is complete and correct. \
Reject with specific revision feedback otherwise.\n\
Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
Accepted: {\"status\":\"accepted\",\"content\":\"<YOUR_RESPONSE_HERE>\"}\n\
Rejected: {\"status\":\"rejected\",\"reason\":\"<REASON_FOR_REJECTION>\"}\n\
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
            planner_system: CODING_PLANNER_SYSTEM.to_string(),
            worker_system: CODING_WORKER_SYSTEM.to_string(),
            critic_system: CODING_CRITIC_SYSTEM.to_string(),
            referee_system: CODING_REFEREE_SYSTEM.to_string(),
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
            coding.planner_system, default.planner_system,
            "coding planner_system must differ from default"
        );
        assert_ne!(
            coding.worker_system, default.worker_system,
            "coding worker_system must differ from default"
        );
    }

    #[test]
    fn coding_adapter_preserves_json_protocol_invariants() {
        let policy = CodingProjectAdapter.role_policy();
        for (label, system) in [
            ("planner", policy.planner_system.as_str()),
            ("worker", policy.worker_system.as_str()),
            ("critic", policy.critic_system.as_str()),
            ("referee", policy.referee_system.as_str()),
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
                system.contains("<YOUR_RESPONSE_HERE>"),
                "{label} system must include accepted schema placeholder; got:\n{system}"
            );
            assert!(
                system.contains("<REASON_FOR_REJECTION>"),
                "{label} system must include rejected schema placeholder; got:\n{system}"
            );
        }
    }

    #[test]
    fn coding_planner_emphasizes_software_planning() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy.planner_system.contains("software planning"),
            "planner_system must mention software planning; got:\n{}",
            policy.planner_system
        );
        assert!(
            policy.planner_system.contains("bounded"),
            "planner_system must mention bounded tasks; got:\n{}",
            policy.planner_system
        );
    }

    #[test]
    fn coding_worker_emphasizes_implementation() {
        let policy = CodingProjectAdapter.role_policy();
        assert!(
            policy.worker_system.contains("software implementation"),
            "worker_system must mention software implementation; got:\n{}",
            policy.worker_system
        );
        assert!(
            policy.worker_system.contains("file tools"),
            "worker_system must mention file tools; got:\n{}",
            policy.worker_system
        );
    }

    #[test]
    fn coding_prompts_contain_same_protocol_footer_as_default() {
        let coding = CodingProjectAdapter.role_policy();
        let default = DefaultProjectAdapter.role_policy();
        // Every coding system string must contain the same key invariant
        // strings as the default policy to ensure equal protocol hardening.
        for system in [
            coding.planner_system.as_str(),
            coding.worker_system.as_str(),
            coding.critic_system.as_str(),
            coding.referee_system.as_str(),
        ] {
            assert_eq!(
                system.contains("Do not copy example values"),
                default.worker_system.contains("Do not copy example values"),
                "coding system must carry the same copy-guard as the default policy"
            );
            assert_eq!(
                system.contains("Return exactly one JSON object"),
                default
                    .worker_system
                    .contains("Return exactly one JSON object"),
                "coding system must carry the same JSON-only instruction as the default policy"
            );
        }
    }
}
