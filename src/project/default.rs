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
        // Invariant: the default adapter is a pure passthrough — it must
        // return RolePolicy::default() unchanged. The content of each field
        // is already protected by the roles::policy tests, so this checks
        // structural equality rather than re-deriving those tests here.
        let adapter = DefaultProjectAdapter;
        let policy = adapter.role_policy();
        let expected = RolePolicy::default();

        assert_eq!(
            policy.planner_producer_system,
            expected.planner_producer_system
        );
        assert_eq!(
            policy.worker_producer_system,
            expected.worker_producer_system
        );
        assert_eq!(policy.planner_critic_system, expected.planner_critic_system);
        assert_eq!(policy.worker_critic_system, expected.worker_critic_system);
        assert_eq!(
            policy.planner_referee_system,
            expected.planner_referee_system
        );
        assert_eq!(policy.worker_referee_system, expected.worker_referee_system);
        assert_eq!(policy.planner_producer_base, expected.planner_producer_base);
        assert_eq!(
            policy.worker_role_descriptions,
            expected.worker_role_descriptions
        );
    }
}
