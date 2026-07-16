//! Renders the static prompt template for a role/node-kind combination.
//!
//! Reuses [`build_role_prompt`] — the same composition path
//! [`RoleRunner::run_role`](super::runner::RoleRunner::run_role)
//! uses — so the Identity/Context/Instructions/Constraints and tool-section
//! content is rendered verbatim from the loaded adapter and plugin YAML.
//! Fields a real run only fills in at dispatch time (objective, target file
//! contents, prior role content, revision feedback) are replaced with
//! clearly labeled placeholders instead.

use crate::machines::deliberation::{DeliberationContext, DeliberationRole, RevisionFeedback};
use crate::machines::scheduler::{NodeKind, TestPlanContext};
use crate::roles::{RolePolicy, TargetView, TargetViewKind};

use super::runner::{RoleRequest, build_role_prompt};
use super::tooling::file_tool_policy_for_request;

const OBJECTIVE_PLACEHOLDER: &str = "{OBJECTIVE: task objective from planner}";
const PROJECT_STATE_PLACEHOLDER: &str = "{PROJECT_STATE: northstar + current artifact API summary}";
const TARGET_STATE_PLACEHOLDER: &str =
    "{TARGET_STATE_VIEW: current file contents for target files}";
const REVIEW_CONTRACT_COVERED_PLACEHOLDER: &str =
    "{NODE_REVIEW_CONTRACT: required test target covered by declared follow-up work}";
const REVIEW_CONTRACT_MISSING_PLACEHOLDER: &str =
    "{NODE_REVIEW_CONTRACT: required test target not covered by declared follow-up work}";
const PRODUCER_CONTENT_PLACEHOLDER: &str = "{PRIOR_PRODUCER_CONTENT: producer summary}";
const CRITIC_CONTENT_PLACEHOLDER: &str = "{PRIOR_CRITIC_CONTENT: critic review}";
const REVISION_FEEDBACK_PLACEHOLDER: &str = "{REVISION_FEEDBACK: previous rejection reasons}";
const TARGET_FILE_PLACEHOLDER: &str = "<target file>";

/// Render the exact prompt a real run would send for `role` on a `node_kind`
/// node (and, for [`NodeKind::Work`], the named `worker_role`), with every
/// dynamically computed field replaced by a labeled placeholder.
///
/// `policy` should come from the same adapter a real run would load, via
/// [`crate::project::ProjectAdapter::role_policy`], so all static content is
/// rendered from the real generic/adapter YAML layers.
pub fn render_prompt_preview(
    policy: &RolePolicy,
    node_kind: NodeKind,
    role: DeliberationRole,
    worker_role: Option<String>,
) -> String {
    // Decomposition/Plan nodes have no target files of their own — they plan
    // over the whole objective, not specific files — so a real run never
    // attaches target files, a target state view, or adapter-required test
    // targets to their prompts. Only mock those fields for Work nodes.
    let has_tools = matches!(node_kind, NodeKind::Work);

    let context = DeliberationContext {
        target_files: if has_tools {
            vec![TARGET_FILE_PLACEHOLDER.to_string()]
        } else {
            vec![]
        },
        testing_requirement: None,
        artifact: None,
        northstar: Some(PROJECT_STATE_PLACEHOLDER.to_string()),
        plugin_prompt: None,
    };

    let target_views = if has_tools {
        vec![TargetView {
            id: TARGET_FILE_PLACEHOLDER.to_string(),
            exists: true,
            kind: TargetViewKind::FullText,
            representation: TARGET_STATE_PLACEHOLDER.to_string(),
        }]
    } else {
        vec![]
    };

    // Two distinct targets, one planned and one not, so the preview actually
    // demonstrates the covered/missing split a real run's Node Review
    // Contract can show, rather than collapsing both branches into one.
    let test_plan_context = TestPlanContext {
        required_validation_targets: if has_tools {
            vec![
                REVIEW_CONTRACT_COVERED_PLACEHOLDER.to_string(),
                REVIEW_CONTRACT_MISSING_PLACEHOLDER.to_string(),
            ]
        } else {
            vec![]
        },
        planned_test_targets: if has_tools {
            vec![REVIEW_CONTRACT_COVERED_PLACEHOLDER.to_string()]
        } else {
            vec![]
        },
    };

    // Producer never sees prior producer/critic content — it is the role
    // that generates it. Critic sees the Producer's output; Referee sees
    // both the Producer's output and the Critic's review. See
    // `DeliberationMachine`'s transition table for the source of this
    // shape: `producer_content`/`critic_content` are only ever populated
    // for the corresponding downstream role.
    let producer_content = matches!(role, DeliberationRole::Critic | DeliberationRole::Referee)
        .then(|| PRODUCER_CONTENT_PLACEHOLDER.to_string());
    let critic_content =
        matches!(role, DeliberationRole::Referee).then(|| CRITIC_CONTENT_PLACEHOLDER.to_string());
    // Revision feedback is only ever forwarded to the Producer on a retry
    // after a Referee rejection.
    let feedback = if matches!(role, DeliberationRole::Producer) {
        vec![RevisionFeedback {
            reason: REVISION_FEEDBACK_PLACEHOLDER.to_string(),
        }]
    } else {
        vec![]
    };

    let request = RoleRequest {
        role,
        objective: OBJECTIVE_PLACEHOLDER.to_string(),
        context,
        test_plan_context,
        target_views,
        producer_content,
        critic_content,
        feedback,
        node_kind,
        worker_role,
        tool_context: None,
    };

    let file_tool_policy = file_tool_policy_for_request(
        &request.role,
        &request.node_kind,
        &request.context.target_files,
        &request.test_plan_context.required_validation_targets,
    );

    build_role_prompt(&request, policy, &file_tool_policy, has_tools).base_prompt
}
