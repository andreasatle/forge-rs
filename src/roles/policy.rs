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

/// GBNF grammar constraining output to the Work-node Producer's
/// `{"summary": "..."}` schema.
pub(crate) const PRODUCER_GBNF: &str = r#"root ::= "{" ws "\"summary\"" ws ":" ws string ws "}" ws

string ::=
  "\"" (
    [^\\"\x7F\x00-\x1F] |
    "\\" (["\\/bfnrt] | "u" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F])
  )* "\"" ws

ws ::= ([ \t\n] ws)?"#;

/// GBNF grammar constraining output to the Critic/Referee accept-or-reject
/// schema: `{"status":"accepted","content":"..."}` or
/// `{"status":"rejected","reason":"..."}`.
pub(crate) const ROLE_GBNF: &str = r#"root ::= accepted | rejected
accepted ::= "{" ws "\"status\"" ws ":" ws "\"accepted\"" ws "," ws "\"content\"" ws ":" ws string ws "}" ws
rejected ::= "{" ws "\"status\"" ws ":" ws "\"rejected\"" ws "," ws "\"reason\"" ws ":" ws string ws "}" ws

string ::=
  "\"" (
    [^\\"\x7F\x00-\x1F] |
    "\\" (["\\/bfnrt] | "u" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F])
  )* "\"" ws

ws ::= ([ \t\n] ws)?"#;

/// GBNF grammar constraining output to the `PlannerOutput` schema used by
/// adapters that model tasks as concrete operations
/// (`{"tasks":[{"id":...,"objective":...,"operation":"create|modify|delete","targets":[...],"depends_on":[...]}]}`).
pub(crate) const PLANNER_GBNF: &str = r#"root ::= "{" ws "\"tasks\"" ws ":" ws "[" ws task (ws "," ws task)* ws "]" ws "}" ws
task ::= "{" ws "\"id\"" ws ":" ws string ws "," ws "\"objective\"" ws ":" ws string ws "," ws "\"operation\"" ws ":" ws operation ws "," ws "\"targets\"" ws ":" ws string-array-nonempty ws "," ws "\"depends_on\"" ws ":" ws string-array ws "}" ws
operation ::= "\"create\"" | "\"modify\"" | "\"delete\""
string-array-nonempty ::= "[" ws (string (ws "," ws string)*) ws "]" ws
string-array ::= "[" ws (string (ws "," ws string)*)? ws "]" ws

string ::=
  "\"" (
    [^\\"\x7F\x00-\x1F] |
    "\\" (["\\/bfnrt] | "u" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F])
  )* "\"" ws

ws ::= ([ \t\n] ws)?"#;

/// GBNF grammar constraining output to the `PlannerOutput` schema used by
/// [`crate::project::DefaultProjectAdapter`], whose tasks have no `operation`
/// field
/// (`{"tasks":[{"id":...,"objective":...,"targets":[...],"depends_on":[...]}]}`).
pub(crate) const PLANNER_NO_OPERATION_GBNF: &str = r#"root ::= "{" ws "\"tasks\"" ws ":" ws "[" ws task (ws "," ws task)* ws "]" ws "}" ws
task ::= "{" ws "\"id\"" ws ":" ws string ws "," ws "\"objective\"" ws ":" ws string ws "," ws "\"targets\"" ws ":" ws string-array-nonempty ws "," ws "\"depends_on\"" ws ":" ws string-array ws "}" ws
string-array-nonempty ::= "[" ws (string (ws "," ws string)*) ws "]" ws
string-array ::= "[" ws (string (ws "," ws string)*)? ws "]" ws

string ::=
  "\"" (
    [^\\"\x7F\x00-\x1F] |
    "\\" (["\\/bfnrt] | "u" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F])
  )* "\"" ws

ws ::= ([ \t\n] ws)?"#;

/// Generic JSON-output-format constraints shared by every role's protocol
/// footer: exactly one JSON object, no markdown or code fence, and no text
/// before or after the JSON.
///
/// Framework protocol, identical across every role and adapter. Extracted so
/// it renders as its own labeled `Constraints:` section, distinct from the
/// role-specific and adapter-specific constraints it is composed with —
/// rather than being restated inline in [`DEFAULT_SYSTEM`],
/// [`WORK_PRODUCER_SYSTEM`], and [`PLANNER_PROTOCOL_FOOTER_WITH_OPERATION`].
pub(crate) const GENERIC_CONSTRAINTS: &str = "Return exactly one JSON object. No markdown. No code fence. \
No explanation. No text before or after the JSON.";

/// JSON protocol instructions for Worker, Critic, and Referee roles.
///
/// This is framework protocol, not project-specific content: it is identical
/// regardless of which [`crate::project::ProjectAdapter`] supplies the
/// surrounding prompt, so adapters (e.g. the YAML-driven coding adapters)
/// compose their project-specific text with this constant rather than
/// re-stating it.
///
/// Callers compose this after [`GENERIC_CONSTRAINTS`] — the JSON-format
/// constraint it used to restate inline has been extracted there.
pub(crate) const DEFAULT_SYSTEM: &str = "Allowed final responses:\n\
Accepted: `status` must be \"accepted\"; `content` must be a non-empty task-specific string.\n\
Rejected: `status` must be \"rejected\"; `reason` must be a non-empty task-specific string.\n\
Producer returns accepted content. \
Critic accepts with a review or rejects with a reason. \
Referee accepts approval or rejects with revision feedback. \
Execution failures are handled by the framework, not the model.";

/// JSON protocol instructions for the Work-node Producer role.
///
/// The Work-node Producer's job is to implement — it never rejects, so its
/// response is a single `{"summary": "..."}` object with no `status` tag to
/// discriminate. This is a different schema from [`DEFAULT_SYSTEM`], not a
/// restricted view of it; the `status`/`content`/`reason` fields must never
/// appear anywhere in a prompt the Work-node Producer can receive.
///
/// Framework protocol, shared across adapters — see [`DEFAULT_SYSTEM`].
/// Callers compose this after [`GENERIC_CONSTRAINTS`], see [`DEFAULT_SYSTEM`].
pub(crate) const WORK_PRODUCER_SYSTEM: &str = "Allowed final response:\n\
`summary` must be a non-empty task-specific string describing what you did.\n\
Implement the requested change and return a summary describing what you did. \
There is no rejected response — a valid summary means the work is done. \
Execution failures are handled by the framework, not the model.";

/// JSON protocol instructions for planner-style roles whose task schema has
/// no `operation` field — targets alone describe the task.
///
/// Framework protocol, shared by every adapter that models tasks without
/// concrete create/modify/delete operations — see [`PLANNER_PROTOCOL_FOOTER_WITH_OPERATION`].
/// Callers compose this after [`GENERIC_CONSTRAINTS`], see [`DEFAULT_SYSTEM`].
pub(crate) const PLANNER_PROTOCOL_FOOTER: &str = "PlannerOutput: `tasks` must be a non-empty array.\n\
Each task requires `id`, `objective`, `targets`, and `depends_on`.\n\
Each `targets` array must be non-empty and list exact files the task may create, modify, or delete.";

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
/// Callers compose this after [`GENERIC_CONSTRAINTS`], see [`DEFAULT_SYSTEM`].
pub(crate) const PLANNER_PROTOCOL_FOOTER_WITH_OPERATION: &str = "PlannerOutput: `tasks` must be a non-empty array.\n\
Each task requires `id`, `objective`, `operation`, `targets`, and `depends_on`.\n\
`operation` must be \"create\", \"modify\", or \"delete\".\n\
Each `targets` array must be non-empty and list exact files the task may create, modify, or delete.";

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
    /// The `PlannerOutput` task-schema footer used inside
    /// `planner_producer_system`, kept separately so retry prompts can
    /// re-show the exact schema variant the model was originally given
    /// (with or without the `operation` field) instead of guessing.
    pub planner_protocol_schema: String,
    /// Language-specific guidance injected as its own section between the
    /// adapter system prompt and the tool section, when set.
    ///
    /// Sourced from [`LanguageSpec::prompt_guidance`], not from the project
    /// adapter — adapters describe project-specific behavior, languages
    /// describe language-specific conventions.
    ///
    /// [`LanguageSpec::prompt_guidance`]: crate::language::LanguageSpec::prompt_guidance
    pub language_guidance: Option<String>,
    /// Language-specific constraints injected as their own section
    /// immediately after `language_guidance`, when set.
    ///
    /// Sourced from [`LanguageSpec::constraints`] — prohibitions and
    /// conventions distinct from the general guidance in
    /// `language_guidance`.
    ///
    /// [`LanguageSpec::constraints`]: crate::language::LanguageSpec::constraints
    pub language_constraints: Option<String>,
}

impl Default for RolePolicy {
    fn default() -> Self {
        Self {
            planner_producer_system: format!("{GENERIC_CONSTRAINTS}\n{PLANNER_PROTOCOL_FOOTER}"),
            worker_producer_system: format!("{GENERIC_CONSTRAINTS}\n{WORK_PRODUCER_SYSTEM}"),
            planner_critic_system: format!("{GENERIC_CONSTRAINTS}\n{DEFAULT_SYSTEM}"),
            worker_critic_system: format!("{GENERIC_CONSTRAINTS}\n{DEFAULT_SYSTEM}"),
            planner_referee_system: format!("{GENERIC_CONSTRAINTS}\n{DEFAULT_SYSTEM}"),
            worker_referee_system: format!("{GENERIC_CONSTRAINTS}\n{DEFAULT_SYSTEM}"),
            planner_protocol_schema: PLANNER_PROTOCOL_FOOTER.to_string(),
            language_guidance: None,
            language_constraints: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_role_policy_contains_json_protocol_instructions() {
        let policy = RolePolicy::default();
        // Critic and Referee roles use the status/content wrapper schema.
        for system in [
            &policy.planner_critic_system,
            &policy.worker_critic_system,
            &policy.planner_referee_system,
            &policy.worker_referee_system,
        ] {
            assert!(
                system.contains("`status`"),
                "default system must contain JSON status field; got:\n{system}"
            );
            assert!(
                system.contains("non-empty task-specific string"),
                "default system must describe task-specific string fields; got:\n{system}"
            );
            assert!(
                !system.contains('$') && !system.contains("\"...\""),
                "default system must not contain placeholder JSON values; got:\n{system}"
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
            policy
                .worker_producer_system
                .contains("non-empty task-specific string"),
            "worker_producer_system must describe task-specific string fields; got:\n{}",
            policy.worker_producer_system
        );
        assert!(
            !policy.worker_producer_system.contains('$')
                && !policy.worker_producer_system.contains("\"...\""),
            "worker_producer_system must not contain placeholder JSON values; got:\n{}",
            policy.worker_producer_system
        );
        // Planner uses a direct PlannerOutput schema — no status/content wrapper.
        assert!(
            policy.planner_producer_system.contains("`tasks`"),
            "default planner_producer_system must show the direct tasks schema; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            !policy.planner_producer_system.contains("\"status\""),
            "default planner_producer_system must not contain the role status field; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            policy.planner_producer_system.contains("PlannerOutput"),
            "default planner_producer_system must describe PlannerOutput; got:\n{}",
            policy.planner_producer_system
        );
    }

    #[test]
    fn planner_prompt_shows_direct_planner_output_schema() {
        let policy = RolePolicy::default();
        assert!(
            policy.planner_producer_system.contains("`tasks`"),
            "planner_producer_system must contain the 'tasks' key; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            policy.planner_producer_system.contains("`id`"),
            "planner_producer_system must show the 'id' field in the example; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            policy.planner_producer_system.contains("`objective`"),
            "planner_producer_system must show the 'objective' field in the example; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            policy.planner_producer_system.contains("`depends_on`"),
            "planner_producer_system must show the 'depends_on' field in the example; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            policy.planner_producer_system.contains("`targets`"),
            "planner_producer_system must show the 'targets' field in the example; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            policy
                .planner_producer_system
                .contains("`targets` array must be non-empty"),
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
    fn worker_producer_uses_summary_only_schema() {
        // The Work-node Producer implements; it never rejects, so its schema
        // has no status/content/reason wrapper at all — just `summary`.
        let policy = RolePolicy::default();
        assert!(
            policy.worker_producer_system.contains("`summary`"),
            "worker_producer_system must describe the summary field; got:\n{}",
            policy.worker_producer_system
        );
        assert!(
            !policy.worker_producer_system.contains("`status`"),
            "worker_producer_system must not contain the status field; got:\n{}",
            policy.worker_producer_system
        );
        assert!(
            !policy.worker_producer_system.contains("`content`"),
            "worker_producer_system must not contain the content field; got:\n{}",
            policy.worker_producer_system
        );
        assert!(
            !policy.worker_producer_system.contains("\"rejected\""),
            "worker_producer_system must never mention the rejected status; got:\n{}",
            policy.worker_producer_system
        );
        assert!(
            !policy.worker_producer_system.contains("\"accepted\""),
            "worker_producer_system must never mention the accepted status; got:\n{}",
            policy.worker_producer_system
        );
    }

    #[test]
    fn critic_still_uses_status_content_schema() {
        let policy = RolePolicy::default();
        assert!(
            policy.planner_critic_system.contains("`status`"),
            "planner_critic_system must still contain the status/content wrapper; got:\n{}",
            policy.planner_critic_system
        );
        assert!(
            policy.worker_critic_system.contains("`status`"),
            "worker_critic_system must still contain the status/content wrapper; got:\n{}",
            policy.worker_critic_system
        );
        assert!(
            policy
                .worker_critic_system
                .contains("Accepted: `status` must be \"accepted\""),
            "worker_critic_system must describe accepted schema; got:\n{}",
            policy.worker_critic_system
        );
    }

    #[test]
    fn referee_still_uses_status_content_schema() {
        let policy = RolePolicy::default();
        assert!(
            policy.planner_referee_system.contains("`status`"),
            "planner_referee_system must still contain the status/content wrapper; got:\n{}",
            policy.planner_referee_system
        );
        assert!(
            policy.worker_referee_system.contains("`status`"),
            "worker_referee_system must still contain the status/content wrapper; got:\n{}",
            policy.worker_referee_system
        );
        assert!(
            policy
                .worker_referee_system
                .contains("Accepted: `status` must be \"accepted\""),
            "worker_referee_system must describe accepted schema; got:\n{}",
            policy.worker_referee_system
        );
    }
}
