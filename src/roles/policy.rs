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

/// GBNF grammar constraining the Work-node Producer's inner tool-call loop:
/// any of the five file tool calls, or the final `{"summary":"..."}`
/// response. Active for every provider call while tool use is still
/// permitted; [`PRODUCER_GBNF`] takes over once tool use ends, forcing the
/// final response.
///
/// Field order within each tool call matches the schema described to the
/// model in [`super::prompt::render_tool_section`].
pub(crate) const PRODUCER_TOOL_GBNF: &str = r#"root ::= write-file | replace-text | read-file | list-files | delete-file | summary
write-file ::= "{" ws "\"tool\"" ws ":" ws "\"write_file\"" ws "," ws "\"path\"" ws ":" ws string ws "," ws "\"content\"" ws ":" ws string ws "}" ws
replace-text ::= "{" ws "\"tool\"" ws ":" ws "\"replace_text\"" ws "," ws "\"path\"" ws ":" ws string ws "," ws "\"old\"" ws ":" ws string ws "," ws "\"new\"" ws ":" ws string ws "}" ws
read-file ::= "{" ws "\"tool\"" ws ":" ws "\"read_file\"" ws "," ws "\"path\"" ws ":" ws string ws "}" ws
list-files ::= "{" ws "\"tool\"" ws ":" ws "\"list_files\"" ws "}" ws
delete-file ::= "{" ws "\"tool\"" ws ":" ws "\"delete_file\"" ws "," ws "\"path\"" ws ":" ws string ws "}" ws
summary ::= "{" ws "\"summary\"" ws ":" ws string ws "}" ws

string ::=
  "\"" (
    [^\\"\x7F\x00-\x1F] |
    "\\" (["\\/bfnrt] | "u" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F])
  )* "\"" ws

ws ::= ([ \t\n] ws)?"#;

/// GBNF grammar constraining the Critic/Referee inner tool-call loop: the two
/// read-only tool calls (`read_file`, `list_files`), or the final
/// accept-or-reject response. Active for every provider call while tool use
/// is still permitted; [`ROLE_GBNF`] takes over once tool use ends, forcing
/// the final response.
///
/// Critic and Referee never receive write tools, so this grammar omits
/// `write_file`, `replace_text`, and `delete_file` entirely.
pub(crate) const REVIEWER_TOOL_GBNF: &str = r#"root ::= read-file | list-files | accepted | rejected
read-file ::= "{" ws "\"tool\"" ws ":" ws "\"read_file\"" ws "," ws "\"path\"" ws ":" ws string ws "}" ws
list-files ::= "{" ws "\"tool\"" ws ":" ws "\"list_files\"" ws "}" ws
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
/// (`{"kind":"work|plan","tasks":[{"id":...,"objective":...,"operation":"create|modify|delete","role":...,"targets":[...],"depends_on":[...]}]}`).
/// The top-level `kind` field is optional (grammar permits its omission,
/// defaulting to `work` on the parsing side); `targets` may be empty since
/// `kind: "plan"` tasks have no concrete files yet. `role` is optional
/// (grammar permits its omission, defaulting to no assigned role on the
/// parsing side).
pub(crate) const PLANNER_GBNF: &str = r#"root ::= "{" ws (kind-field ws "," ws)? "\"tasks\"" ws ":" ws "[" ws task (ws "," ws task)* ws "]" ws "}" ws
kind-field ::= "\"kind\"" ws ":" ws ("\"work\"" | "\"plan\"")
task ::= "{" ws "\"id\"" ws ":" ws string ws "," ws "\"objective\"" ws ":" ws string ws "," ws "\"operation\"" ws ":" ws operation ws "," ws (role-field ws "," ws)? "\"targets\"" ws ":" ws string-array ws "," ws "\"depends_on\"" ws ":" ws string-array ws "}" ws
operation ::= "\"create\"" | "\"modify\"" | "\"delete\""
role-field ::= "\"role\"" ws ":" ws string
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
Each `targets` array must be non-empty and list exact files the task may create, modify, or delete.\n\
Optional `role` field: the name of one of the available worker roles to assign this task to. \
Omit `role` to leave the task unassigned.\n\
Optional top-level `kind` field: \"work\" (default when omitted) or \"plan\". \
When `kind` is \"plan\", every task becomes a further planning node instead of a work node, and `targets` may be empty. \
All tasks in one PlannerOutput share the same kind — never mix work and plan tasks in one response.";

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
    /// Worker role name/description pairs, surfaced to the Plan-node
    /// Producer so it can assign roles explicitly to each task.
    ///
    /// Sourced from the adapter's `workers` list
    /// (`WorkerRoleConfig::role`/`WorkerRoleConfig::description`). Empty
    /// when the adapter defines no worker roles, in which case the section
    /// is omitted from the rendered prompt.
    pub worker_role_descriptions: Vec<(String, String)>,
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
            worker_role_descriptions: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_system_prompts_have_expected_role_schemas() {
        let policy = RolePolicy::default();

        assert_schema(
            &policy.planner_producer_system,
            &["`tasks`"],
            &["`status`", "`summary`"],
        );
        assert_schema(
            &policy.worker_producer_system,
            &["`summary`"],
            &["`status`", "`tasks`"],
        );

        for system in [
            &policy.planner_critic_system,
            &policy.worker_critic_system,
            &policy.planner_referee_system,
            &policy.worker_referee_system,
        ] {
            assert_schema(
                system,
                &["`status`", "`content`", "`reason`"],
                &["`summary`", "`tasks`"],
            );
        }
    }

    #[test]
    fn default_system_prompts_have_no_placeholder_values() {
        let policy = RolePolicy::default();
        for system in [
            &policy.planner_producer_system,
            &policy.worker_producer_system,
            &policy.planner_critic_system,
            &policy.worker_critic_system,
            &policy.planner_referee_system,
            &policy.worker_referee_system,
            &policy.planner_protocol_schema,
        ] {
            assert!(
                !system.contains('$'),
                "system prompt contains `$`: {system}"
            );
            assert!(
                !system.contains("\"...\""),
                "system prompt contains placeholder JSON value: {system}"
            );
        }
    }

    fn assert_schema(system: &str, required: &[&str], forbidden: &[&str]) {
        for field in required {
            assert!(
                system.contains(field),
                "schema is missing {field}: {system}"
            );
        }
        for field in forbidden {
            assert!(
                !system.contains(field),
                "schema includes unexpected {field}: {system}"
            );
        }
    }
}
