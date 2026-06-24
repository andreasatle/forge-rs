//! Default project adapter — reproduces the hardcoded behaviour exactly.

use super::ProjectAdapter;
use crate::roles::RolePolicy;

/// A [`ProjectAdapter`] that returns [`RolePolicy::default()`].
///
/// All roles receive the current hardcoded JSON protocol instructions.
/// Runtime behaviour is unchanged from before the adapter seam was introduced.
pub struct DefaultProjectAdapter;

impl ProjectAdapter for DefaultProjectAdapter {
    fn role_policy(&self) -> RolePolicy {
        RolePolicy::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_project_adapter_returns_default_role_policy() {
        let adapter = DefaultProjectAdapter;
        let policy = adapter.role_policy();
        // Worker, Critic, Referee use the status/content wrapper schema.
        for system in [
            &policy.worker_system,
            &policy.critic_system,
            &policy.referee_system,
        ] {
            assert!(
                system.contains("\"status\""),
                "default policy must contain JSON status field; got:\n{system}"
            );
            assert!(
                system.contains("Do not copy example values"),
                "default policy must include copy-guard instruction; got:\n{system}"
            );
            assert!(
                !system.contains("\"...\""),
                "default policy must not contain dot-placeholder JSON values; got:\n{system}"
            );
        }
        // Planner uses direct PlannerOutput schema.
        assert!(
            policy.planner_system.contains("\"tasks\""),
            "default planner_system must show direct tasks schema; got:\n{}",
            policy.planner_system
        );
        assert!(
            !policy.planner_system.contains("\"status\""),
            "default planner_system must not contain status/content wrapper; got:\n{}",
            policy.planner_system
        );
        assert!(
            policy.planner_system.contains("Do not copy example values"),
            "default planner_system must include copy-guard instruction; got:\n{}",
            policy.planner_system
        );
    }

    #[test]
    fn default_adapter_preserves_existing_prompt_behavior() {
        let adapter = DefaultProjectAdapter;
        let policy = adapter.role_policy();
        let default = RolePolicy::default();
        assert_eq!(
            policy.planner_system, default.planner_system,
            "DefaultProjectAdapter must preserve planner_system"
        );
        assert_eq!(
            policy.worker_system, default.worker_system,
            "DefaultProjectAdapter must preserve worker_system"
        );
        assert_eq!(
            policy.critic_system, default.critic_system,
            "DefaultProjectAdapter must preserve critic_system"
        );
        assert_eq!(
            policy.referee_system, default.referee_system,
            "DefaultProjectAdapter must preserve referee_system"
        );
    }
}
