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
        // Critic and Referee roles use the status/content wrapper schema.
        for system in [
            &policy.planner_critic_system,
            &policy.worker_critic_system,
            &policy.planner_referee_system,
            &policy.worker_referee_system,
        ] {
            assert!(
                system.contains("`status`"),
                "default policy must contain JSON status field; got:\n{system}"
            );
            assert!(
                system.contains("non-empty task-specific string"),
                "default policy must describe task-specific string fields; got:\n{system}"
            );
            assert!(
                !system.contains('$') && !system.contains("\"...\""),
                "default policy must not contain placeholder JSON values; got:\n{system}"
            );
        }
        // Work-node Producer uses the summary-only schema — no status field.
        assert!(
            !policy.worker_producer_system.contains("`status`"),
            "worker_producer_system must not contain the status field; got:\n{}",
            policy.worker_producer_system
        );
        assert!(
            policy.worker_producer_system.contains("`summary`"),
            "worker_producer_system must describe the summary field; got:\n{}",
            policy.worker_producer_system
        );
        assert!(
            !policy.worker_producer_system.contains('$')
                && !policy.worker_producer_system.contains("\"...\""),
            "worker_producer_system must not contain placeholder JSON values; got:\n{}",
            policy.worker_producer_system
        );
        // Planner uses direct PlannerOutput schema.
        assert!(
            policy.planner_producer_system.contains("`tasks`"),
            "default planner_producer_system must show direct tasks schema; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            !policy.planner_producer_system.contains("\"status\""),
            "default planner_producer_system must not contain status/content wrapper; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            policy.planner_producer_system.contains("PlannerOutput"),
            "default planner_producer_system must describe PlannerOutput; got:\n{}",
            policy.planner_producer_system
        );
        // Critic and Referee roles keep both branches of the protocol footer.
        for system in [
            &policy.planner_critic_system,
            &policy.worker_critic_system,
            &policy.planner_referee_system,
            &policy.worker_referee_system,
        ] {
            assert!(system.contains("Return exactly one JSON object"));
            assert!(system.contains("Accepted: `status` must be \"accepted\""));
            assert!(system.contains("Rejected: `status` must be \"rejected\""));
            assert!(system.contains("Execution failures are handled by the framework"));
        }
        // The Work-node Producer never rejects, so it keeps the summary-only footer.
        assert!(
            policy
                .worker_producer_system
                .contains("Return exactly one JSON object")
        );
        assert!(
            policy
                .worker_producer_system
                .contains("`summary` must be a non-empty task-specific string")
        );
        assert!(
            policy
                .worker_producer_system
                .contains("Execution failures are handled by the framework")
        );
    }
}
