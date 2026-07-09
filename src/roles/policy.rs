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

use crate::machines::scheduler::NodeKind;

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
/// (`{"kind":"work|decomposition","tasks":[{"id":...,"objective":...,"operation":"create|modify|delete","role":...,"targets":[...],"depends_on":[...]}]}`).
/// The top-level `kind` field is optional (grammar permits its omission,
/// defaulting to `work` on the parsing side); `targets` may be empty since
/// `kind: "decomposition"` tasks have no concrete files yet. `role` is optional
/// (grammar permits its omission, defaulting to no assigned role on the
/// parsing side).
pub(crate) const PLANNER_GBNF: &str = r#"root ::= "{" ws (kind-field ws "," ws)? "\"tasks\"" ws ":" ws "[" ws task (ws "," ws task)* ws "]" ws "}" ws
kind-field ::= "\"kind\"" ws ":" ws ("\"work\"" | "\"decomposition\"")
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

/// GBNF grammar constraining output to the `PlannerOutput` schema used by
/// adapters that define worker roles: identical to [`PLANNER_GBNF`] except
/// `role` is a required field rather than an optional one, since the planner
/// must assign every task to one of the adapter's configured worker roles.
pub(crate) const PLANNER_GBNF_WITH_ROLES: &str = r#"root ::= "{" ws (kind-field ws "," ws)? "\"tasks\"" ws ":" ws "[" ws task (ws "," ws task)* ws "]" ws "}" ws
kind-field ::= "\"kind\"" ws ":" ws ("\"work\"" | "\"decomposition\"")
task ::= "{" ws "\"id\"" ws ":" ws string ws "," ws "\"objective\"" ws ":" ws string ws "," ws "\"operation\"" ws ":" ws operation ws "," ws role-field ws "," ws "\"targets\"" ws ":" ws string-array ws "," ws "\"depends_on\"" ws ":" ws string-array ws "}" ws
operation ::= "\"create\"" | "\"modify\"" | "\"delete\""
role-field ::= "\"role\"" ws ":" ws string
string-array ::= "[" ws (string (ws "," ws string)*)? ws "]" ws

string ::=
  "\"" (
    [^\\"\x7F\x00-\x1F] |
    "\\" (["\\/bfnrt] | "u" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F])
  )* "\"" ws

ws ::= ([ \t\n] ws)?"#;

/// GBNF grammar constraining output to the `NodeKind::Decomposition` Producer
/// schema: `{"kind":"decomposition","objectives":[{"id":...,"objective":...,"depends_on":[...]}]}`
/// or `{"kind":"plan"}`. Unlike [`PLANNER_GBNF`], `kind` is required (never
/// omitted) and `"work"` is not a valid value; `decomposition` objectives
/// carry no `targets` or `role` field at all, and `plan` carries no
/// `objectives` field or any other field — the bare `kind` tag is the entire
/// signal that the objective is atomic.
pub(crate) const DECOMPOSITION_GBNF: &str = r#"root ::= decomposition | plan
decomposition ::= "{" ws "\"kind\"" ws ":" ws "\"decomposition\"" ws "," ws "\"objectives\"" ws ":" ws "[" ws objective (ws "," ws objective)* ws "]" ws "}" ws
plan ::= "{" ws "\"kind\"" ws ":" ws "\"plan\"" ws "}" ws
objective ::= "{" ws "\"id\"" ws ":" ws string ws "," ws "\"objective\"" ws ":" ws string ws "," ws "\"depends_on\"" ws ":" ws string-array ws "}" ws
string-array ::= "[" ws (string (ws "," ws string)*)? ws "]" ws

string ::=
  "\"" (
    [^\\"\x7F\x00-\x1F] |
    "\\" (["\\/bfnrt] | "u" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F])
  )* "\"" ws

ws ::= ([ \t\n] ws)?"#;

/// JSON protocol instructions for Worker, Critic, and Referee roles.
///
/// This is framework protocol, not project-specific content: it is identical
/// regardless of which [`crate::project::ProjectAdapter`] supplies the
/// surrounding prompt, so adapters (e.g. the YAML-driven coding adapters)
/// compose their project-specific text with this constant rather than
/// re-stating it.
///
/// The JSON-format constraint and the "execution failures are handled by the
/// framework" note this used to restate inline now live in the generic
/// prompt layer — see [`generic_prompt`].
pub(crate) const DEFAULT_SYSTEM: &str = "Allowed final responses:\n\
Accepted: `status` must be \"accepted\"; `content` must be a non-empty task-specific string.\n\
Rejected: `status` must be \"rejected\"; `reason` must be a non-empty task-specific string.\n\
Producer returns accepted content. \
Critic accepts with a review or rejects with a reason. \
Referee accepts approval or rejects with revision feedback.";

/// JSON protocol instructions for the Work-node Producer role.
///
/// The Work-node Producer's job is to implement — it never rejects, so its
/// response is a single `{"summary": "..."}` object with no `status` tag to
/// discriminate. This is a different schema from [`DEFAULT_SYSTEM`], not a
/// restricted view of it; the `status`/`content`/`reason` fields must never
/// appear anywhere in a prompt the Work-node Producer can receive.
///
/// Framework protocol, shared across adapters — see [`DEFAULT_SYSTEM`].
/// The "execution failures are handled by the framework" note this used to
/// restate inline now lives in the generic prompt layer — see
/// [`generic_prompt`].
pub(crate) const WORK_PRODUCER_SYSTEM: &str = "Allowed final response:\n\
`summary` must be a non-empty task-specific string describing what you did.\n\
Implement the requested change and return a summary describing what you did. \
There is no rejected response — a valid summary means the work is done.";

/// JSON protocol instructions for planner-style roles whose task schema has
/// no `operation` field — targets alone describe the task.
///
/// Framework protocol, shared by every adapter that models tasks without
/// concrete create/modify/delete operations — see [`PLANNER_PROTOCOL_FOOTER_WITH_OPERATION`].
pub(crate) const PLANNER_PROTOCOL_FOOTER: &str = "PlannerOutput: `tasks` must be a non-empty array.\n\
Each task requires `id`, `objective`, `targets`, and `depends_on`.\n\
Each `targets` array must be non-empty and list exact files the task may create, modify, or delete.";

/// JSON protocol instructions for the [`NodeKind::Decomposition`] Producer
/// role: a required top-level `kind` field of exactly `"decomposition"` or
/// `"plan"`, with no `"work"` option.
///
/// Framework protocol, fixed by structural node kind rather than adapter
/// configuration — see [`planner_protocol_schema_for`].
pub(crate) const DECOMPOSITION_PROTOCOL_FOOTER: &str = "DecompositionOutput: top-level `kind` is required; must be \"decomposition\" or \"plan\".\n\
\"decomposition\": the objective spans multiple concerns and needs further breakdown. `objectives` is required and must be non-empty; each objective requires `id`, `objective`, and `depends_on` only — no `targets`, no `role`. Each objective becomes a further Decomposition node.\n\
\"plan\": the objective is already concrete and atomic, ready for a leaf planner to structure the work. No `objectives` field or any other field is permitted: return exactly `{\"kind\": \"plan\"}`. This is a complete, valid, and immediately acceptable response on its own. The objective becomes a single Plan node.";

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
pub(crate) const PLANNER_PROTOCOL_FOOTER_WITH_OPERATION: &str = "PlannerOutput: `tasks` must be a non-empty array.\n\
Each task requires `id`, `objective`, `operation`, `targets`, and `depends_on`.\n\
`operation` must be \"create\", \"modify\", or \"delete\".\n\
Each `targets` array must be non-empty and list exact files the task may create, modify, or delete.\n\
Optional `role` field: the name of one of the available worker roles to assign this task to. \
Omit `role` to leave the task unassigned.\n\
Optional top-level `kind` field: \"work\" (default when omitted) or \"decomposition\". \
When `kind` is \"decomposition\", every task becomes a further planning node instead of a work node, and `targets` may be empty. \
All tasks in one PlannerOutput share the same kind — never mix work and decomposition tasks in one response.";

/// JSON protocol instructions for planner-style roles under an adapter that
/// defines worker roles: identical to
/// [`PLANNER_PROTOCOL_FOOTER_WITH_OPERATION`] except `role` is described as
/// required, since every task must be assigned to one of the worker roles
/// listed earlier in the prompt rather than left unassigned.
pub(crate) const PLANNER_PROTOCOL_FOOTER_WITH_OPERATION_AND_ROLES: &str = "PlannerOutput: `tasks` must be a non-empty array.\n\
Each task requires `id`, `objective`, `operation`, `role`, `targets`, and `depends_on`.\n\
`operation` must be \"create\", \"modify\", or \"delete\".\n\
Each `targets` array must be non-empty and list exact files the task may create, modify, or delete.\n\
Required `role` field: the name of one of the available worker roles listed above. \
Every task must be assigned to one of those roles.\n\
Optional top-level `kind` field: \"work\" (default when omitted) or \"decomposition\". \
When `kind` is \"decomposition\", every task becomes a further planning node instead of a work node, and `targets` may be empty. \
All tasks in one PlannerOutput share the same kind — never mix work and decomposition tasks in one response.";

/// A role prompt split into four explicit sections.
///
/// `identity` frames who the role is; `context` supplies ambient background
/// the role needs; `instructions` describes what the role must do;
/// `constraints` bounds how it may do it (prohibitions, rejection-grounding
/// rules, scope limits).
///
/// This shape is shared by three prompt layers, composed together by
/// [`render_role_prompt`] for every role in every adapter: the generic layer
/// (see [`generic_prompt`]), the adapter's own per-role layer (see
/// [`crate::project::yaml_config::RolePromptConfig`], re-exported as this
/// same type), and the language plugin's layer (see
/// [`crate::language::LanguageSpec`]).
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RolePromptConfig {
    /// Who the role is.
    pub identity: String,
    /// Ambient background the role needs.
    pub context: String,
    /// What the role must do.
    pub instructions: String,
    /// Prohibitions and boundaries on how the role may do it.
    pub constraints: String,
}

/// The framework's generic prompt layer, embedded from `adapters/generic.yaml`
/// at compile time.
///
/// Content here applies to every role in every adapter, regardless of project
/// or language: it is always loaded, never optional, and requires no
/// per-adapter or per-plugin opt-in. Parsed once and cached for the life of
/// the process.
pub(crate) fn generic_prompt() -> &'static RolePromptConfig {
    static GENERIC: std::sync::LazyLock<RolePromptConfig> = std::sync::LazyLock::new(|| {
        const GENERIC_YAML: &str = include_str!("../../adapters/generic.yaml");
        serde_yaml::from_str(GENERIC_YAML).expect("adapters/generic.yaml must parse")
    });
    &GENERIC
}

/// Compose one rendered prompt section from its generic, adapter, and
/// (optional) plugin layers, in that order, joined by newlines. Empty layers
/// are omitted rather than leaving a blank line.
fn compose_section(generic: &str, adapter: &str, plugin: Option<&str>) -> String {
    [Some(generic), Some(adapter), plugin]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render a role prompt's identity, context, instructions, and constraints as
/// separate labeled sections, composing the generic prompt layer, the
/// adapter's role-specific layer, and the language plugin's layer (when
/// present) for each section — see [`compose_section`].
pub(crate) fn render_role_prompt(
    generic: &RolePromptConfig,
    adapter: &RolePromptConfig,
    plugin: Option<&RolePromptConfig>,
) -> String {
    let identity = compose_section(
        &generic.identity,
        &adapter.identity,
        plugin.map(|p| p.identity.as_str()),
    );
    let context = compose_section(
        &generic.context,
        &adapter.context,
        plugin.map(|p| p.context.as_str()),
    );
    let instructions = compose_section(
        &generic.instructions,
        &adapter.instructions,
        plugin.map(|p| p.instructions.as_str()),
    );
    let constraints = compose_section(
        &generic.constraints,
        &adapter.constraints,
        plugin.map(|p| p.constraints.as_str()),
    );
    format!(
        "Identity:\n{identity}\n\nContext:\n{context}\n\nInstructions:\n{instructions}\n\nConstraints:\n{constraints}"
    )
}

/// Render a language plugin's prompt sections as their own
/// Identity/Context/Instructions/Constraints block, in the same shape as
/// [`render_role_prompt`] — used when a plugin is selected per node by
/// [`crate::language::select_plugin`] rather than composed once at
/// adapter-load time, since the adapter has no target files to select by.
pub(crate) fn render_plugin_prompt(plugin: &RolePromptConfig) -> String {
    let empty = RolePromptConfig::default();
    render_role_prompt(&empty, &empty, Some(plugin))
}

/// Producer/Critic/Referee system prompts for one named worker role.
///
/// Selected in place of the shared `worker_*_system` fields on [`RolePolicy`]
/// when a Work node carries a `worker_role` that matches an entry in
/// [`RolePolicy::worker_role_policies`].
#[derive(Clone, Debug, PartialEq)]
pub struct WorkerRolePolicy {
    /// System instruction for this role's Producer.
    pub producer_system: String,
    /// System instruction for this role's Critic.
    pub critic_system: String,
    /// System instruction for this role's Referee.
    pub referee_system: String,
}

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
    ///
    /// Applies to [`NodeKind::Work`] only — [`NodeKind::Decomposition`]
    /// and [`NodeKind::Plan`] use the fixed schema variants selected by
    /// [`planner_protocol_schema_for`].
    pub planner_protocol_schema: String,
    /// `planner_producer_system` with the trailing protocol-schema footer
    /// removed: role identity, adapter instructions/constraints, and the
    /// generic JSON-format constraints, but no task-schema footer.
    ///
    /// Combined with a node-kind-specific footer to build the
    /// [`NodeKind::Decomposition`] and [`NodeKind::Plan`] Producer system
    /// prompts, which use fixed schema variants rather than the adapter's
    /// configured `planner_protocol_schema`.
    pub planner_producer_base: String,
    /// Worker role name/description pairs, surfaced to the Plan-node
    /// Producer so it can assign roles explicitly to each task.
    ///
    /// Sourced from the adapter's `workers` list
    /// (`WorkerRoleConfig::role`/`WorkerRoleConfig::description`). Empty
    /// when the adapter defines no worker roles, in which case the section
    /// is omitted from the rendered prompt.
    pub worker_role_descriptions: Vec<(String, String)>,
    /// Per-role Producer/Critic/Referee prompts, keyed by worker role name.
    ///
    /// A Work node whose `worker_role` matches a key here uses that entry's
    /// prompts instead of `worker_producer_system`/`worker_critic_system`/
    /// `worker_referee_system`. Nodes with no role, or a role absent from
    /// this map, fall back to the shared fields.
    pub worker_role_policies: std::collections::HashMap<String, WorkerRolePolicy>,
}

impl Default for RolePolicy {
    fn default() -> Self {
        // No adapter or plugin configured: every role prompt is the generic
        // layer alone, composed the same way [`crate::project::yaml::YamlProjectAdapter`]
        // composes it, just with empty adapter/plugin layers.
        let generic = generic_prompt();
        let empty = RolePromptConfig::default();
        let planner_producer_base = render_role_prompt(generic, &empty, None);
        Self {
            planner_producer_system: format!("{planner_producer_base}\n{PLANNER_PROTOCOL_FOOTER}"),
            worker_producer_system: format!(
                "{}\n{WORK_PRODUCER_SYSTEM}",
                render_role_prompt(generic, &empty, None)
            ),
            planner_critic_system: format!(
                "{}\n{DEFAULT_SYSTEM}",
                render_role_prompt(generic, &empty, None)
            ),
            worker_critic_system: format!(
                "{}\n{DEFAULT_SYSTEM}",
                render_role_prompt(generic, &empty, None)
            ),
            planner_referee_system: format!(
                "{}\n{DEFAULT_SYSTEM}",
                render_role_prompt(generic, &empty, None)
            ),
            worker_referee_system: format!(
                "{}\n{DEFAULT_SYSTEM}",
                render_role_prompt(generic, &empty, None)
            ),
            planner_protocol_schema: PLANNER_PROTOCOL_FOOTER.to_string(),
            planner_producer_base,
            worker_role_descriptions: Vec::new(),
            worker_role_policies: std::collections::HashMap::new(),
        }
    }
}

/// Select the planner protocol footer — and therefore the task output schema
/// — for a Plan-family Producer, based on structural node kind rather than
/// adapter configuration.
///
/// [`NodeKind::Decomposition`] nodes only ever decompose further, with no
/// worker-role assignment; [`NodeKind::Plan`] nodes are the point where tasks
/// are assigned worker roles and concrete file operations.
pub(crate) fn planner_protocol_schema_for<'a>(
    node_kind: &NodeKind,
    policy: &'a RolePolicy,
) -> &'a str {
    match node_kind {
        NodeKind::Decomposition => DECOMPOSITION_PROTOCOL_FOOTER,
        NodeKind::Plan => PLANNER_PROTOCOL_FOOTER_WITH_OPERATION_AND_ROLES,
        NodeKind::Work => &policy.planner_protocol_schema,
    }
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
