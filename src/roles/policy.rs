//! Role-specific system prompt policy.
//!
//! [`RolePolicy`] holds the system instruction injected into each role's rendered
//! prompt. The default reproduces the current hardcoded behaviour exactly.
//!
//! Policy selection is specific to both [`NodeKind`] and [`DeliberationRole`]:
//! plan and work nodes can use different Producer, Critic, and Referee
//! instructions.
//!
//! [`DeliberationRole`]: crate::machines::deliberation::DeliberationRole
//! [`DeliberationRole::Producer`]: crate::machines::deliberation::DeliberationRole
//! [`NodeKind`]: crate::machines::scheduler::NodeKind

/// JSON protocol instructions for Worker, Critic, and Referee roles.
const DEFAULT_SYSTEM: &str = "Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
Accepted: {\"status\":\"accepted\",\"content\":\"<YOUR_RESPONSE_HERE>\"}\n\
Rejected: {\"status\":\"rejected\",\"reason\":\"<REASON_FOR_REJECTION>\"}\n\
Do not copy example values. Replace them with task-specific content.\n\
Producer returns accepted content. \
Critic accepts with a review or rejects with a reason. \
Referee accepts approval or rejects with revision feedback. \
Execution failures are handled by the framework, not the model.";

/// JSON protocol instructions for the Planner (Plan-node Producer) role.
///
/// The planner returns a [`PlannerOutput`] directly — no `status`/`content`
/// wrapper. This avoids double-encoding and works correctly under JSON grammar.
const PLANNER_SYSTEM: &str = "Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
{\"tasks\":[{\"id\":\"task-id\",\"objective\":\"Task objective.\",\"depends_on\":[]}]}\n\
Do not copy example values. Replace them with actual task IDs and objectives.";

/// Per-role system prompt policy.
///
/// Each field is injected verbatim as the final paragraph of the rendered
/// prompt for that role. Override individual fields to change what a specific
/// role is told to do without touching the surrounding prompt structure.
#[derive(Clone)]
pub struct RolePolicy {
    /// System instruction for Plan-node Producer role.
    pub planner_producer_system: String,
    /// System instruction for Work-node Producer role.
    pub worker_producer_system: String,
    /// System instruction for Plan-node Critic role.
    pub planner_critic_system: String,
    /// System instruction for Work-node Critic role.
    pub worker_critic_system: String,
    /// System instruction for Plan-node Referee role.
    pub planner_referee_system: String,
    /// System instruction for Work-node Referee role.
    pub worker_referee_system: String,
}

impl Default for RolePolicy {
    fn default() -> Self {
        Self {
            planner_producer_system: PLANNER_SYSTEM.to_string(),
            worker_producer_system: DEFAULT_SYSTEM.to_string(),
            planner_critic_system: DEFAULT_SYSTEM.to_string(),
            worker_critic_system: DEFAULT_SYSTEM.to_string(),
            planner_referee_system: DEFAULT_SYSTEM.to_string(),
            worker_referee_system: DEFAULT_SYSTEM.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_role_policy_contains_json_protocol_instructions() {
        let policy = RolePolicy::default();
        // All non-producer planner/work review roles use the status/content wrapper schema.
        for system in [
            &policy.worker_producer_system,
            &policy.planner_critic_system,
            &policy.worker_critic_system,
            &policy.planner_referee_system,
            &policy.worker_referee_system,
        ] {
            assert!(
                system.contains("\"status\""),
                "default system must contain JSON status field; got:\n{system}"
            );
            assert!(
                system.contains("Do not copy example values"),
                "default system must include copy-guard instruction; got:\n{system}"
            );
            assert!(
                !system.contains("\"...\""),
                "default system must not contain dot-placeholder JSON values; got:\n{system}"
            );
        }
        // Planner uses a direct PlannerOutput schema — no status/content wrapper.
        assert!(
            policy.planner_producer_system.contains("\"tasks\""),
            "default planner_producer_system must show the direct tasks schema; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            !policy.planner_producer_system.contains("\"status\""),
            "default planner_producer_system must not contain the role status field; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            policy
                .planner_producer_system
                .contains("Do not copy example values"),
            "default planner_producer_system must include copy-guard instruction; got:\n{}",
            policy.planner_producer_system
        );
    }

    #[test]
    fn planner_prompt_shows_direct_planner_output_schema() {
        let policy = RolePolicy::default();
        assert!(
            policy.planner_producer_system.contains("\"tasks\""),
            "planner_producer_system must contain the 'tasks' key; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            policy.planner_producer_system.contains("\"id\""),
            "planner_producer_system must show the 'id' field in the example; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            policy.planner_producer_system.contains("\"objective\""),
            "planner_producer_system must show the 'objective' field in the example; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            policy.planner_producer_system.contains("\"depends_on\""),
            "planner_producer_system must show the 'depends_on' field in the example; got:\n{}",
            policy.planner_producer_system
        );
    }

    #[test]
    fn planner_prompt_does_not_show_status_content_schema() {
        let policy = RolePolicy::default();
        assert!(
            !policy.planner_producer_system.contains("\"status\""),
            "planner_producer_system must not contain the status/content wrapper; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            !policy.planner_producer_system.contains("\"content\""),
            "planner_producer_system must not contain the content wrapper field; got:\n{}",
            policy.planner_producer_system
        );
    }

    #[test]
    fn worker_still_uses_status_content_schema() {
        let policy = RolePolicy::default();
        assert!(
            policy.worker_producer_system.contains("\"status\""),
            "worker_producer_system must still contain the status/content wrapper; got:\n{}",
            policy.worker_producer_system
        );
        assert!(
            policy
                .worker_producer_system
                .contains("<YOUR_RESPONSE_HERE>"),
            "worker_producer_system must show accepted schema placeholder; got:\n{}",
            policy.worker_producer_system
        );
        assert!(
            policy
                .worker_producer_system
                .contains("<REASON_FOR_REJECTION>"),
            "worker_producer_system must show rejected schema placeholder; got:\n{}",
            policy.worker_producer_system
        );
    }

    #[test]
    fn critic_still_uses_status_content_schema() {
        let policy = RolePolicy::default();
        assert!(
            policy.planner_critic_system.contains("\"status\""),
            "planner_critic_system must still contain the status/content wrapper; got:\n{}",
            policy.planner_critic_system
        );
        assert!(
            policy.worker_critic_system.contains("\"status\""),
            "worker_critic_system must still contain the status/content wrapper; got:\n{}",
            policy.worker_critic_system
        );
        assert!(
            policy.worker_critic_system.contains("<YOUR_RESPONSE_HERE>"),
            "worker_critic_system must show accepted schema placeholder; got:\n{}",
            policy.worker_critic_system
        );
    }

    #[test]
    fn referee_still_uses_status_content_schema() {
        let policy = RolePolicy::default();
        assert!(
            policy.planner_referee_system.contains("\"status\""),
            "planner_referee_system must still contain the status/content wrapper; got:\n{}",
            policy.planner_referee_system
        );
        assert!(
            policy.worker_referee_system.contains("\"status\""),
            "worker_referee_system must still contain the status/content wrapper; got:\n{}",
            policy.worker_referee_system
        );
        assert!(
            policy
                .worker_referee_system
                .contains("<YOUR_RESPONSE_HERE>"),
            "worker_referee_system must show accepted schema placeholder; got:\n{}",
            policy.worker_referee_system
        );
    }
}
