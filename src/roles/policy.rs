//! Role-specific system prompt policy.
//!
//! [`RolePolicy`] holds the system instruction injected into each role's rendered
//! prompt. The default reproduces the current hardcoded behaviour exactly.
//!
//! `planner_system` and `worker_system` both map to [`DeliberationRole::Producer`]
//! at the current layer. Use `worker_system` when dispatching a Producer effect;
//! `planner_system` becomes useful once [`NodeKind`] flows through the handler chain.
//!
//! [`DeliberationRole::Producer`]: crate::machines::deliberation::DeliberationRole
//! [`NodeKind`]: crate::machines::scheduler::NodeKind

/// Hardcoded JSON protocol instructions shared by all roles in the default policy.
const DEFAULT_SYSTEM: &str = "Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
Accepted: {\"status\":\"accepted\",\"content\":\"<YOUR_RESPONSE_HERE>\"}\n\
Rejected: {\"status\":\"rejected\",\"reason\":\"<REASON_FOR_REJECTION>\"}\n\
Do not copy example values. Replace them with task-specific content.\n\
Producer returns accepted content. \
Critic accepts with a review or rejects with a reason. \
Referee accepts approval or rejects with revision feedback. \
Execution failures are handled by the framework, not the model.";

/// Per-role system prompt policy.
///
/// Each field is injected verbatim as the final paragraph of the rendered
/// prompt for that role. Override individual fields to change what a specific
/// role is told to do without touching the surrounding prompt structure.
#[derive(Clone)]
pub struct RolePolicy {
    /// System instruction for the Planner variant of the Producer role.
    pub planner_system: String,
    /// System instruction for the Worker variant of the Producer role.
    pub worker_system: String,
    /// System instruction for the Critic role.
    pub critic_system: String,
    /// System instruction for the Referee role.
    pub referee_system: String,
}

impl Default for RolePolicy {
    fn default() -> Self {
        Self {
            planner_system: DEFAULT_SYSTEM.to_string(),
            worker_system: DEFAULT_SYSTEM.to_string(),
            critic_system: DEFAULT_SYSTEM.to_string(),
            referee_system: DEFAULT_SYSTEM.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_role_policy_contains_json_protocol_instructions() {
        let policy = RolePolicy::default();
        for system in [
            &policy.planner_system,
            &policy.worker_system,
            &policy.critic_system,
            &policy.referee_system,
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
    }
}
