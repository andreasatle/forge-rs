//! Default project adapter — reproduces the hardcoded behaviour exactly.

use super::{ProjectAdapter, build_file_text_target_views};
use crate::artifacts::ArtifactRead;
use crate::machines::deliberation::state::DeliberationRole;
use crate::roles::{RolePolicy, TargetView};

/// A [`ProjectAdapter`] that returns [`RolePolicy::default()`].
///
/// All roles receive the current hardcoded JSON protocol instructions.
/// Runtime behaviour is unchanged from before the adapter seam was introduced.
pub struct DefaultProjectAdapter;

impl ProjectAdapter for DefaultProjectAdapter {
    fn role_policy(&self) -> RolePolicy {
        RolePolicy::default()
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_project_adapter_returns_default_role_policy() {
        let adapter = DefaultProjectAdapter;
        let policy = adapter.role_policy();
        // All non-planner-producer roles use the status/content wrapper schema.
        for system in [
            &policy.worker_producer_system,
            &policy.planner_critic_system,
            &policy.worker_critic_system,
            &policy.planner_referee_system,
            &policy.worker_referee_system,
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
            policy.planner_producer_system.contains("\"tasks\""),
            "default planner_producer_system must show direct tasks schema; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            !policy.planner_producer_system.contains("\"status\""),
            "default planner_producer_system must not contain status/content wrapper; got:\n{}",
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
    fn default_adapter_preserves_existing_prompt_behavior() {
        let adapter = DefaultProjectAdapter;
        let policy = adapter.role_policy();
        let default = RolePolicy::default();
        assert_eq!(
            policy.planner_producer_system, default.planner_producer_system,
            "DefaultProjectAdapter must preserve planner_producer_system"
        );
        assert_eq!(
            policy.worker_producer_system, default.worker_producer_system,
            "DefaultProjectAdapter must preserve worker_producer_system"
        );
        assert_eq!(
            policy.planner_critic_system, default.planner_critic_system,
            "DefaultProjectAdapter must preserve planner_critic_system"
        );
        assert_eq!(
            policy.worker_critic_system, default.worker_critic_system,
            "DefaultProjectAdapter must preserve worker_critic_system"
        );
        assert_eq!(
            policy.planner_referee_system, default.planner_referee_system,
            "DefaultProjectAdapter must preserve planner_referee_system"
        );
        assert_eq!(
            policy.worker_referee_system, default.worker_referee_system,
            "DefaultProjectAdapter must preserve worker_referee_system"
        );
    }

    #[test]
    fn default_policy_preserves_protocol_footer() {
        let policy = DefaultProjectAdapter.role_policy();
        for system in [
            policy.worker_producer_system.as_str(),
            policy.planner_critic_system.as_str(),
            policy.worker_critic_system.as_str(),
            policy.planner_referee_system.as_str(),
            policy.worker_referee_system.as_str(),
        ] {
            assert!(system.contains("Return exactly one JSON object"));
            assert!(system.contains("Accepted: {\"status\":\"accepted\""));
            assert!(system.contains("Rejected: {\"status\":\"rejected\""));
            assert!(system.contains("Do not copy example values"));
            assert!(system.contains("Execution failures are handled by the framework"));
        }
    }
}
