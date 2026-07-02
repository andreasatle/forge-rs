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
///
/// This is framework protocol, not project-specific content: it is identical
/// regardless of which [`crate::project::ProjectAdapter`] supplies the
/// surrounding prompt, so adapters (e.g. the YAML-driven coding adapters)
/// compose their project-specific text with this constant rather than
/// re-stating it.
pub(crate) const DEFAULT_SYSTEM: &str = "Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
Accepted: {\"status\":\"accepted\",\"content\":\"$RESPONSE_SUMMARY\"}\n\
Rejected: {\"status\":\"rejected\",\"reason\":\"$REASON_FOR_REJECTION\"}\n\
Do not copy example values. Replace them with task-specific content.\n\
Producer returns accepted content. \
Critic accepts with a review or rejects with a reason. \
Referee accepts approval or rejects with revision feedback. \
Execution failures are handled by the framework, not the model.";

/// JSON protocol instructions for the Work-node Producer role.
///
/// The Work-node Producer's job is to implement — it never rejects. Only the
/// Critic and Referee roles evaluate and may reject, so this schema omits the
/// rejected branch entirely; it must never appear anywhere in a prompt the
/// Work-node Producer can receive.
///
/// Framework protocol, shared across adapters — see [`DEFAULT_SYSTEM`].
pub(crate) const WORK_PRODUCER_SYSTEM: &str = "Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
Accepted: {\"status\":\"accepted\",\"content\":\"$RESPONSE_SUMMARY\"}\n\
Do not copy example values. Replace them with task-specific content.\n\
Implement the requested change and return accepted content describing what you did. \
Execution failures are handled by the framework, not the model.";

/// JSON protocol instructions for the Planner (Plan-node Producer) role.
///
/// The planner returns a [`PlannerOutput`] directly — no `status`/`content`
/// wrapper. This avoids double-encoding and works correctly under JSON grammar.
const PLANNER_SYSTEM: &str = "Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
Every task must include non-empty `targets` listing the exact files the task may create, modify, or delete.\n\
{\"tasks\":[{\"id\":\"task-id\",\"objective\":\"Task objective.\",\"targets\":[\"path/to/file\"],\"depends_on\":[]}]}\n\
Do not copy example values. Replace them with actual task IDs and objectives.";

/// Generic role-identity and task-decomposition instruction for the Plan-node
/// Producer role.
///
/// Not specific to any project adapter or language: it describes how to
/// decompose an objective into a schedulable task graph, without reference to
/// files, tests, or artifact operations. Adapters compose this with their own
/// project-specific targeting rules.
pub(crate) const PLANNER_PRODUCER_IDENTITY: &str = "You are a software planning agent. \
Decompose the objective into bounded, independent tasks. Each task must address exactly one \
concern. Express dependencies explicitly. Do not include implementation details in plan nodes \
— describe what to achieve, not how. Output a structured task list that the execution \
framework can schedule.";

/// Generic role-identity and tool-usage instruction for the Work-node
/// Producer role.
///
/// Not specific to any project adapter or language: it establishes the role
/// and that file tools exist and should be used before assuming file
/// contents, without reference to coding conventions like tests.
pub(crate) const WORKER_PRODUCER_IDENTITY: &str = "You are a software implementation agent. \
Implement the requested change precisely. Use available file tools to read, modify, and write \
artifact files. Use tools before making assumptions about file contents — inspect files before \
editing them.";

/// JSON protocol instructions for planner-style roles whose task schema
/// includes an explicit `operation` field (`create`/`modify`/`delete`)
/// alongside `targets`.
///
/// Framework protocol, shared by every adapter that models tasks as
/// concrete artifact operations — see [`DEFAULT_SYSTEM`].
pub(crate) const PLANNER_PROTOCOL_FOOTER_WITH_OPERATION: &str = "Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.\n\
{\"tasks\":[{\"id\":\"task-id\",\"objective\":\"Task objective.\",\"operation\":\"modify\",\"targets\":[\"path/to/file\"],\"depends_on\":[]}]}\n\
Do not copy example values. Replace them with actual task IDs and objectives.";

/// Shared Plan-node Critic prompt for coding-style project adapters.
///
/// Byte-identical between `coding.yaml` and `coding_tdd.yaml`; the two
/// variants differ only in their Producer prompts, so
/// [`crate::project::YamlProjectAdapter`] falls back to this constant when a
/// config's `role_prompts.planner_critic` is omitted.
pub(crate) const CODING_PLANNER_CRITIC: &str = "You are a software planning review agent. \
Evaluate the proposed task graph, not the final implementation artifact. Judge whether the graph \
covers the objective, tasks are bounded, each task addresses one concern, dependencies are \
sensible, task objectives are actionable, and worker nodes have enough detail. Reject any task \
that does not identify a concrete file target or produce a verifiable artifact change. Reject \
pure-reasoning tasks such as \"define content\", \"decide design\", \"analyze approach\", or \
\"plan implementation\" unless they are embedded in an artifact-changing task. Do not judge \
whether files already changed, final code compiles, or the final artifact already exists. Ground \
every rejection in the current node objective, declared target files, plan metadata, adapter \
policy, validation contract, or observable artifact correctness. Do not reject solely for \
unstated preferences about style, algorithm, architecture, or performance. For example, do not \
reject recursive code solely because an iterative version might be faster unless the contract \
requires iteration or a performance bound. If you have a style or performance concern outside the \
contract, mention it in accepted content as advisory only. Accept with a plan review summary or \
reject with a specific, actionable plan revision reason.";

/// Shared Work-node Critic prompt for coding-style project adapters.
///
/// See [`CODING_PLANNER_CRITIC`] for why this is a shared framework
/// constant rather than duplicated YAML text.
pub(crate) const CODING_WORKER_CRITIC: &str = "You are a software review agent. Evaluate the \
producer output for correctness and completeness. Identify missing work, unsupported claims, and \
incomplete implementation. Check for missed edge cases and unnecessary complexity. Apply the \
rendered node review contract for current-node test and follow-up acceptance scope. Use read_file \
to inspect the specific files the producer was expected to modify. Do not accept based only on \
the producer summary or on file existence from list_files. Verify actual file contents satisfy \
the objective. Ground every rejection in the current node objective, declared target files, plan \
metadata, adapter policy, validation contract, or observable artifact correctness. Do not reject \
solely for unstated preferences about style, algorithm, architecture, or performance. For \
example, do not reject recursive code solely because an iterative version might be faster unless \
the contract requires iteration or a performance bound. If you have a style or performance \
concern outside the contract, mention it in accepted content as advisory only. Accept with a \
review summary or reject with a specific, actionable reason.";

/// Shared Plan-node Referee prompt for coding-style project adapters.
///
/// See [`CODING_PLANNER_CRITIC`] for why this is a shared framework
/// constant rather than duplicated YAML text.
pub(crate) const CODING_PLANNER_REFEREE: &str = "You are a software planning acceptance agent. \
Decide whether the proposed task graph is a structurally valid, schedulable plan. Accept when \
tasks collectively cover the objective, dependencies make sense, and the graph is suitable for \
scheduling. A schedulable coding task must have an observable artifact outcome: it must create, \
modify, or delete named files. Reject plans containing tasks that cannot be verified through file \
changes or artifact inspection. Reject with plan revision feedback when a necessary task is \
omitted, task objectives are too vague, dependencies are wrong or missing, tasks are too large, \
or any task is a pure-reasoning step with no artifact target. Do not reject because final code \
has not been written, artifact files do not yet exist, or final output is not yet visible.\n\
Ground every rejection in the current node objective, declared target files, plan metadata, \
adapter policy, validation contract, or observable artifact correctness. Do not reject solely for \
unstated preferences about style, algorithm, architecture, or performance. For example, do not \
reject recursive code solely because an iterative version might be faster unless the contract \
requires iteration or a performance bound. If you have a style or performance concern outside the \
contract, mention it in accepted content as advisory only.";

/// Shared Work-node Referee prompt for coding-style project adapters.
///
/// See [`CODING_PLANNER_CRITIC`] for why this is a shared framework
/// constant rather than duplicated YAML text.
pub(crate) const CODING_WORKER_REFEREE: &str = "You are a software acceptance agent. Decide \
whether the work satisfies the objective and acceptance criteria. Perform a final completeness \
check: every requirement must be addressed, not just the last task. Before accepting, use \
read_file to inspect the specific files the producer was expected to modify. Apply the rendered \
node review contract for current-node test and follow-up acceptance scope. Do not rely on \
list_files to verify completion — file existence is not evidence of correct content. Reject if \
the artifact contents do not satisfy the objective, even if the producer or critic claims they \
do. Accept only when the work is complete and correct. Ground every rejection in the current node \
objective, declared target files, plan metadata, adapter policy, validation contract, or \
observable artifact correctness. Do not reject solely for unstated preferences about style, \
algorithm, architecture, or performance. For example, do not reject recursive code solely because \
an iterative version might be faster unless the contract requires iteration or a performance \
bound. If you have a style or performance concern outside the contract, mention it in accepted \
content as advisory only. Reject with specific revision feedback otherwise.";

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
    /// Language-specific guidance injected as its own section between the
    /// adapter system prompt and the tool section, when set.
    ///
    /// Sourced from [`LanguageSpec::prompt_guidance`], not from the project
    /// adapter — adapters describe project-specific behavior, languages
    /// describe language-specific conventions.
    ///
    /// [`LanguageSpec::prompt_guidance`]: crate::language::LanguageSpec::prompt_guidance
    pub language_guidance: Option<String>,
}

impl Default for RolePolicy {
    fn default() -> Self {
        Self {
            planner_producer_system: PLANNER_SYSTEM.to_string(),
            worker_producer_system: WORK_PRODUCER_SYSTEM.to_string(),
            planner_critic_system: DEFAULT_SYSTEM.to_string(),
            worker_critic_system: DEFAULT_SYSTEM.to_string(),
            planner_referee_system: DEFAULT_SYSTEM.to_string(),
            worker_referee_system: DEFAULT_SYSTEM.to_string(),
            language_guidance: None,
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
        assert!(
            policy.planner_producer_system.contains("\"targets\""),
            "planner_producer_system must show the 'targets' field in the example; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            policy
                .planner_producer_system
                .contains("non-empty `targets`"),
            "planner_producer_system must require non-empty targets; got:\n{}",
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
    fn worker_producer_uses_accepted_only_schema() {
        // The Work-node Producer implements; it never rejects. Only Critic
        // and Referee evaluate and may reject.
        let policy = RolePolicy::default();
        assert!(
            policy.worker_producer_system.contains("\"status\""),
            "worker_producer_system must still contain the status/content wrapper; got:\n{}",
            policy.worker_producer_system
        );
        assert!(
            policy.worker_producer_system.contains("$RESPONSE_SUMMARY"),
            "worker_producer_system must show accepted schema placeholder; got:\n{}",
            policy.worker_producer_system
        );
        assert!(
            !policy
                .worker_producer_system
                .contains("$REASON_FOR_REJECTION"),
            "worker_producer_system must never show the rejected schema placeholder; got:\n{}",
            policy.worker_producer_system
        );
        assert!(
            !policy.worker_producer_system.contains("\"rejected\""),
            "worker_producer_system must never mention the rejected status; got:\n{}",
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
            policy.worker_critic_system.contains("$RESPONSE_SUMMARY"),
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
            policy.worker_referee_system.contains("$RESPONSE_SUMMARY"),
            "worker_referee_system must show accepted schema placeholder; got:\n{}",
            policy.worker_referee_system
        );
    }
}
