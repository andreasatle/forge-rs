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
pub(crate) const PRODUCER_GBNF: &str = r#"root ::= "{" ws "\"summary\"" ws ":" ws string ws "}"

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
accepted ::= "{" ws "\"status\"" ws ":" ws "\"accepted\"" ws "," ws "\"content\"" ws ":" ws string ws "}"
rejected ::= "{" ws "\"status\"" ws ":" ws "\"rejected\"" ws "," ws "\"reason\"" ws ":" ws string ws "}"

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
write-file ::= "{" ws "\"tool\"" ws ":" ws "\"write_file\"" ws "," ws "\"path\"" ws ":" ws string ws "," ws "\"content\"" ws ":" ws string ws "}"
replace-text ::= "{" ws "\"tool\"" ws ":" ws "\"replace_text\"" ws "," ws "\"path\"" ws ":" ws string ws "," ws "\"old\"" ws ":" ws string ws "," ws "\"new\"" ws ":" ws string ws "}"
read-file ::= "{" ws "\"tool\"" ws ":" ws "\"read_file\"" ws "," ws "\"path\"" ws ":" ws string ws "}"
list-files ::= "{" ws "\"tool\"" ws ":" ws "\"list_files\"" ws "}"
delete-file ::= "{" ws "\"tool\"" ws ":" ws "\"delete_file\"" ws "," ws "\"path\"" ws ":" ws string ws "}"
summary ::= "{" ws "\"summary\"" ws ":" ws string ws "}"

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
read-file ::= "{" ws "\"tool\"" ws ":" ws "\"read_file\"" ws "," ws "\"path\"" ws ":" ws string ws "}"
list-files ::= "{" ws "\"tool\"" ws ":" ws "\"list_files\"" ws "}"
accepted ::= "{" ws "\"status\"" ws ":" ws "\"accepted\"" ws "," ws "\"content\"" ws ":" ws string ws "}"
rejected ::= "{" ws "\"status\"" ws ":" ws "\"rejected\"" ws "," ws "\"reason\"" ws ":" ws string ws "}"

string ::=
  "\"" (
    [^\\"\x7F\x00-\x1F] |
    "\\" (["\\/bfnrt] | "u" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F])
  )* "\"" ws

ws ::= ([ \t\n] ws)?"#;

/// GBNF grammar constraining output to the `PlannerOutput` schema used by
/// adapters that define worker roles: the top-level `kind` field is
/// optional, defaulting to `work` on the parsing side when omitted, and
/// `role` is a required field on every `work`/`plan` task, since the planner
/// must assign every such task to one of the adapter's configured worker
/// roles.
///
/// `kind: "plan"` tasks additionally require `task_kv`, matching `kind:
/// "task"` below: a single-task `"plan"` output collapses into a terminal
/// task row (see
/// [`crate::node_runner::planner::PlannerOutputProcessor::into_plan`]), so
/// any task in a `"plan"` batch may end up needing it — the schema asks for
/// it unconditionally rather than only when the batch happens to contain
/// just one task. `kind: "work"` tasks never become a terminal task row, so
/// they carry no `task_kv`.
///
/// `task_kv` is an open string-keyed object (see `task-kv`/`kv-pair` below):
/// the grammar only constrains its *shape* to string-to-string pairs, not
/// which keys are present. Which keys a project adapter actually requires is
/// declared in its YAML (`PlannerConfig::provides`/`WorkerRoleConfig::requires`)
/// and checked post-parse by
/// [`crate::node_runner::planner::PlannerOutputProcessor::validate_structure`]
/// — the same reason `role` above is grammar-open (any string) rather than
/// constrained to the adapter's actual role names, which are likewise only
/// known at runtime from YAML.
///
/// Also accepts a third top-level `kind`: `"task"`, whose tasks are pure
/// planner intent (see [`crate::node_runner::planner::PlannerOutputKind::Task`])
/// and use a distinct, narrower task-record shape —
/// `{"id":...,"objective":...,"task_kv":{...},"depends_on":[...]}`
/// — with no `operation`, `role`, or `targets`. Unlike `work`/`plan`,
/// `kind: "task"` must be stated explicitly; it has no default.
pub(crate) const PLANNER_GBNF_WITH_ROLES: &str = r#"root ::= work-output | plan-output | task-output
work-output ::= "{" ws ("\"kind\"" ws ":" ws "\"work\"" ws "," ws)? "\"tasks\"" ws ":" ws "[" ws work-task (ws "," ws work-task)* ws "]" ws "}"
plan-output ::= "{" ws "\"kind\"" ws ":" ws "\"plan\"" ws "," ws "\"tasks\"" ws ":" ws "[" ws plan-task (ws "," ws plan-task)* ws "]" ws "}"
work-task ::= "{" ws "\"id\"" ws ":" ws string ws "," ws "\"objective\"" ws ":" ws string ws "," ws "\"operation\"" ws ":" ws operation ws "," ws role-field ws "," ws "\"targets\"" ws ":" ws string-array ws "," ws "\"depends_on\"" ws ":" ws string-array ws "}" ws
plan-task ::= "{" ws "\"id\"" ws ":" ws string ws "," ws "\"objective\"" ws ":" ws string ws "," ws "\"task_kv\"" ws ":" ws task-kv ws "," ws "\"operation\"" ws ":" ws operation ws "," ws role-field ws "," ws "\"targets\"" ws ":" ws string-array ws "," ws "\"depends_on\"" ws ":" ws string-array ws "}" ws
operation ::= "\"create\"" | "\"modify\"" | "\"delete\""
role-field ::= "\"role\"" ws ":" ws string
string-array ::= "[" ws (string (ws "," ws string)*)? ws "]" ws
task-kv ::= "{" ws (kv-pair (ws "," ws kv-pair)*)? ws "}" ws
kv-pair ::= string ws ":" ws string

task-output ::= "{" ws "\"kind\"" ws ":" ws "\"task\"" ws "," ws "\"tasks\"" ws ":" ws "[" ws task-record (ws "," ws task-record)* ws "]" ws "}"
task-record ::= "{" ws "\"id\"" ws ":" ws string ws "," ws "\"objective\"" ws ":" ws string ws "," ws "\"task_kv\"" ws ":" ws task-kv ws "," ws "\"depends_on\"" ws ":" ws string-array ws "}" ws

string ::=
  "\"" (
    [^\\"\x7F\x00-\x1F] |
    "\\" (["\\/bfnrt] | "u" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F])
  )* "\"" ws

ws ::= ([ \t\n] ws)?"#;

/// GBNF grammar constraining output to the `PlannerOutput` schema used by
/// adapters that define **no** worker roles: there is no role to assign a
/// work task to, so the top-level `kind` field is mandatory and restricted
/// to `"plan"` or `"task"` — `"work"` is never grammar-legal, and cannot be
/// reached by omitting `kind` either, since omission is not permitted by
/// this grammar at all.
///
/// Per-task shape otherwise matches [`PLANNER_GBNF_WITH_ROLES`], minus the
/// `role` field — including `plan-task` requiring `task_kv`, for the same
/// terminal-short-circuit reason documented there.
pub(crate) const PLANNER_GBNF_NO_WORK: &str = r#"root ::= plan-output | task-output
plan-output ::= "{" ws "\"kind\"" ws ":" ws "\"plan\"" ws "," ws "\"tasks\"" ws ":" ws "[" ws plan-task (ws "," ws plan-task)* ws "]" ws "}"
plan-task ::= "{" ws "\"id\"" ws ":" ws string ws "," ws "\"objective\"" ws ":" ws string ws "," ws "\"task_kv\"" ws ":" ws task-kv ws "," ws "\"operation\"" ws ":" ws operation ws "," ws "\"targets\"" ws ":" ws string-array ws "," ws "\"depends_on\"" ws ":" ws string-array ws "}" ws
operation ::= "\"create\"" | "\"modify\"" | "\"delete\""
string-array ::= "[" ws (string (ws "," ws string)*)? ws "]" ws
task-kv ::= "{" ws (kv-pair (ws "," ws kv-pair)*)? ws "}" ws
kv-pair ::= string ws ":" ws string

task-output ::= "{" ws "\"kind\"" ws ":" ws "\"task\"" ws "," ws "\"tasks\"" ws ":" ws "[" ws task-record (ws "," ws task-record)* ws "]" ws "}"
task-record ::= "{" ws "\"id\"" ws ":" ws string ws "," ws "\"objective\"" ws ":" ws string ws "," ws "\"task_kv\"" ws ":" ws task-kv ws "," ws "\"depends_on\"" ws ":" ws string-array ws "}" ws

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
/// The Work-node Producer never rejects, so its response is a single
/// `{"summary": "..."}` object with no `status` tag to discriminate. This is
/// a different schema from [`DEFAULT_SYSTEM`], not a restricted view of it;
/// the `status`/`content`/`reason` fields must never appear anywhere in a
/// prompt the Work-node Producer can receive.
///
/// Shared byte-for-byte across every worker role — it must not assert what
/// completing the task means (e.g. "implement"), since that would contradict
/// non-implementer roles like `tester` or `pass_tests`, whose own
/// Identity/Instructions define the work in their own terms.
///
/// Framework protocol, shared across adapters — see [`DEFAULT_SYSTEM`].
/// The "execution failures are handled by the framework" note this used to
/// restate inline now lives in the generic prompt layer — see
/// [`generic_prompt`].
pub(crate) const WORK_PRODUCER_SYSTEM: &str = "Allowed final response:\n\
`summary` must be a non-empty task-specific string describing what you did.\n\
Complete the work this task requires and return a summary describing what you did. \
There is no rejected response — a valid summary means the work is done.";

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
///
/// Shared byte-for-byte across every worker role, rendered immediately above
/// each role's own Identity sentence — it must not assert what completing the
/// task means (e.g. "implement"), since that would contradict non-implementer
/// roles like `tester` or `pass_tests`, whose own Identity/Instructions define
/// the work in their own terms.
pub(crate) const WORKER_PRODUCER_IDENTITY: &str = "You are a software agent completing an \
assigned task precisely. Use available file tools to read, modify, and write artifact files. \
Use tools before making assumptions about file contents — inspect files before editing them.";

/// JSON protocol instructions for planner-style roles under an adapter that
/// defines worker roles: every `work`/`plan` task must be assigned to one of
/// the worker roles listed earlier in the prompt.
///
/// Does not mention which `task_kv` keys are required — that set is
/// adapter-specific (`PlannerConfig::provides`) and appended dynamically by
/// [`planner_protocol_schema_for`], the same reason the exact worker role
/// names aren't baked in here either.
pub(crate) const PLANNER_PROTOCOL_FOOTER_WITH_OPERATION_AND_ROLES: &str = "# PlannerOutput Schema\n\
- `tasks` must be a non-empty array.\n\
- Each task requires `id`, `objective`, `operation`, `role`, `targets`, and `depends_on`.\n\
- `operation` must be \"create\", \"modify\", or \"delete\".\n\
- Each `targets` array must be non-empty and list exact files the task may create, modify, or delete.\n\
- Required `role` field: the name of one of the available worker roles listed above — every task must be assigned to one of those roles.\n\
- Optional top-level `kind` field: \"work\" (default when omitted), \"plan\", or \"task\".\n\
\n\
**When `kind` is \"plan\":** every task becomes a further planning node instead of a work node.\n\
- `targets` may be empty.\n\
- Each task additionally requires a `task_kv` object (see below).\n\
\n\
**When `kind` is \"task\":** `kind` must be stated explicitly.\n\
- Each task requires only `id`, `objective`, `task_kv`, and `depends_on` — no `operation`, `role`, or `targets`.\n\
\n\
- All tasks in one PlannerOutput share the same kind — never mix kinds in one response.";

/// JSON protocol instructions for planner-style roles under an adapter that
/// defines no worker roles: there is no role to assign a work task to, so
/// `kind: "work"` is never offered — the planner must either escalate to
/// further planning (`kind: "plan"`) or emit pure planner intent
/// (`kind: "task"`), and must state `kind` explicitly since there is no
/// `work` default to fall back on.
pub(crate) const PLANNER_PROTOCOL_FOOTER_WITH_OPERATION_NO_WORK: &str = "# PlannerOutput Schema\n\
- `tasks` must be a non-empty array.\n\
- Required top-level `kind` field: \"plan\" or \"task\" — this adapter defines no worker roles, so `kind: \"work\"` is not available.\n\
\n\
**When `kind` is \"plan\":** each task requires `id`, `objective`, `task_kv`, `operation`, `targets`, and `depends_on`.\n\
- `operation` must be \"create\", \"modify\", or \"delete\".\n\
- `targets` may be empty, since the task escalates to further planning instead of naming concrete files yet.\n\
\n\
**When `kind` is \"task\":** each task requires only `id`, `objective`, `task_kv`, and `depends_on` — no `operation` or `targets`.\n\
\n\
- All tasks in one PlannerOutput share the same kind — never mix kinds in one response.";

/// A role prompt split into four explicit sections.
///
/// `identity` frames who the role is; `context` supplies ambient background
/// the role needs; `instructions` describes what the role must do;
/// `constraints` bounds how it may do it (prohibitions, rejection-grounding
/// rules, scope limits).
///
/// This shape is shared by three prompt layers, composed together by
/// `render_role_prompt` for every role in every adapter: the generic layer
/// (see `generic_prompt`), the adapter's own per-role layer (see
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

/// A worker role's Critic and Referee prompts within the generic layer —
/// review-contract content shared by every worker role whose adapter opts
/// it in (see [`crate::project::yaml_config::WorkerRoleConfig::review`]),
/// factored out once here instead of duplicated per adapter (e.g.
/// `implement.yaml` and `create_test.yaml`, whose review criteria are
/// genuinely identical). An adapter whose review criteria differ (e.g.
/// `pass_tests.yaml`, which judges against existing tests rather than the
/// objective) declares its `critic`/`referee` inline instead of opting in.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerReviewPromptConfig {
    /// Addition merged into an opted-in worker role's Critic prompt.
    pub critic: RolePromptConfig,
    /// Addition merged into an opted-in worker role's Referee prompt.
    pub referee: RolePromptConfig,
}

/// The framework's generic prompt layer's shape: `identity`/`context`/
/// `instructions`/`constraints` apply to every role in every adapter,
/// `planner` is an additional layer merged only into Plan-node Producer/
/// Critic/Referee composition, and `worker_review` is an additional layer
/// merged only into an opted-in worker role's Critic/Referee composition —
/// see `GenericPromptConfig::shared`, `GenericPromptConfig::for_planner`,
/// and `GenericPromptConfig::for_worker_review_critic`/
/// `for_worker_review_referee`.
///
/// A Work node isn't decomposing anything, so `planner`-only guidance (e.g.
/// MECE decomposition review) would be irrelevant noise there; it must never
/// reach a Work-node prompt. Symmetrically, `worker_review` guidance has no
/// meaning for a Producer (which never accepts or rejects) or for a
/// Plan-node role, so it must never reach either.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenericPromptConfig {
    /// Who every role is, applied to Plan and Work nodes alike.
    pub identity: String,
    /// Ambient background every role needs, applied to Plan and Work nodes
    /// alike.
    pub context: String,
    /// What every role must do, applied to Plan and Work nodes alike.
    pub instructions: String,
    /// Prohibitions and boundaries every role must respect, applied to Plan
    /// and Work nodes alike.
    pub constraints: String,
    /// Guidance merged only into the Plan-node Producer/Critic/Referee —
    /// decomposition-specific review criteria that a Work node has no use
    /// for.
    #[serde(default)]
    pub planner: RolePromptConfig,
    /// Guidance merged only into an opted-in worker role's Critic/Referee —
    /// review-contract criteria that a Producer, and any Plan-node role, has
    /// no use for.
    #[serde(default)]
    pub worker_review: WorkerReviewPromptConfig,
}

impl GenericPromptConfig {
    /// The shared fields alone, applied to every role — Plan and Work alike.
    pub(crate) fn shared(&self) -> RolePromptConfig {
        RolePromptConfig {
            identity: self.identity.clone(),
            context: self.context.clone(),
            instructions: self.instructions.clone(),
            constraints: self.constraints.clone(),
        }
    }

    /// The shared fields with the `planner` addition appended to each
    /// section — used to compose Plan-node Producer/Critic/Referee prompts
    /// only, never a Work-node prompt.
    pub(crate) fn for_planner(&self) -> RolePromptConfig {
        RolePromptConfig {
            identity: append_layer(&self.identity, &self.planner.identity),
            context: append_layer(&self.context, &self.planner.context),
            instructions: append_layer(&self.instructions, &self.planner.instructions),
            constraints: append_layer(&self.constraints, &self.planner.constraints),
        }
    }

    /// The shared fields with the `worker_review.critic` addition appended
    /// to each section — used to compose an opted-in worker role's Critic
    /// prompt only (see
    /// [`crate::project::yaml_config::WorkerRoleConfig::review`]). Never
    /// used for a Producer or a Plan-node role.
    pub(crate) fn for_worker_review_critic(&self) -> RolePromptConfig {
        RolePromptConfig {
            identity: append_layer(&self.identity, &self.worker_review.critic.identity),
            context: append_layer(&self.context, &self.worker_review.critic.context),
            instructions: append_layer(&self.instructions, &self.worker_review.critic.instructions),
            constraints: append_layer(&self.constraints, &self.worker_review.critic.constraints),
        }
    }

    /// The shared fields with the `worker_review.referee` addition appended
    /// to each section. See [`Self::for_worker_review_critic`].
    pub(crate) fn for_worker_review_referee(&self) -> RolePromptConfig {
        RolePromptConfig {
            identity: append_layer(&self.identity, &self.worker_review.referee.identity),
            context: append_layer(&self.context, &self.worker_review.referee.context),
            instructions: append_layer(
                &self.instructions,
                &self.worker_review.referee.instructions,
            ),
            constraints: append_layer(&self.constraints, &self.worker_review.referee.constraints),
        }
    }
}

/// Append `addition` after `base`, separated by a newline; either side may be
/// empty without leaving a stray blank line.
fn append_layer(base: &str, addition: &str) -> String {
    match (base.is_empty(), addition.is_empty()) {
        (_, true) => base.to_string(),
        (true, false) => addition.to_string(),
        (false, false) => format!("{base}\n{addition}"),
    }
}

/// The framework's generic prompt layer, embedded from `adapters/generic.yaml`
/// at compile time.
///
/// Always loaded, never optional, and requires no per-adapter or per-plugin
/// opt-in. Parsed once and cached for the life of the process.
pub(crate) fn generic_prompt() -> &'static GenericPromptConfig {
    static GENERIC: std::sync::LazyLock<GenericPromptConfig> = std::sync::LazyLock::new(|| {
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

/// Render a composed section's lines as a markdown bullet list, one `-` item
/// per line — each layer in `adapters/*.yaml` writes one sentence per line,
/// so this preserves that structure instead of collapsing it into a single
/// paragraph.
fn to_bullets(section: &str) -> String {
    section
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| format!("- {line}"))
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
    let instructions = to_bullets(&instructions);
    let constraints = to_bullets(&constraints);
    format!(
        "# Identity\n{identity}\n\n# Context\n{context}\n\n# Instructions\n{instructions}\n\n# Constraints\n{constraints}"
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
    /// Role identity, adapter instructions/constraints, and the generic
    /// JSON-format constraints for the Plan-node Producer, with no
    /// task-schema footer.
    ///
    /// Combined with a node-kind-specific footer to build the
    /// [Plan-node](crate::machines::scheduler::NodeKind::Plan) Producer
    /// system prompt, which uses the fixed schema variant selected by
    /// `planner_protocol_schema_for`.
    pub planner_producer_base: String,
    /// Worker role name/description pairs, surfaced to the Plan-node
    /// Producer so it can assign roles explicitly to each task.
    ///
    /// Sourced from the adapter's `workers` list
    /// (`WorkerRoleConfig::plugin_role`/`WorkerRoleConfig::description`). Empty
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
    /// The complete set of `task_kv` keys this adapter's planner commits to
    /// emitting on every `kind: "plan"`/`kind: "task"` task, copied verbatim
    /// from [`crate::project::yaml_config::PlannerConfig::provides`].
    ///
    /// Surfaced to the Plan-node Producer prompt (so the model knows exactly
    /// which keys to fill in) and to
    /// `crate::node_runner::planner::PlannerOutputProcessor` (so a task's
    /// `task_kv` can be checked against it). Empty for an adapter that
    /// declares no `provides`, in which case `task_kv` validation imposes no
    /// requirement.
    pub provides: Vec<String>,
}

impl Default for RolePolicy {
    fn default() -> Self {
        // No adapter or plugin configured: every role prompt is the generic
        // layer alone, composed the same way [`crate::project::yaml::YamlProjectAdapter`]
        // composes it, just with empty adapter/plugin layers. Plan-node
        // roles additionally pick up the generic layer's `planner` addition;
        // Work-node roles use the shared fields only.
        let generic = generic_prompt();
        let shared = generic.shared();
        let for_planner = generic.for_planner();
        let empty = RolePromptConfig::default();
        let planner_producer_base = render_role_prompt(&for_planner, &empty, None);
        Self {
            worker_producer_system: format!(
                "{}\n{WORK_PRODUCER_SYSTEM}",
                render_role_prompt(&shared, &empty, None)
            ),
            planner_critic_system: format!(
                "{}\n{DEFAULT_SYSTEM}",
                render_role_prompt(&for_planner, &empty, None)
            ),
            worker_critic_system: format!(
                "{}\n{DEFAULT_SYSTEM}",
                render_role_prompt(&shared, &empty, None)
            ),
            planner_referee_system: format!(
                "{}\n{DEFAULT_SYSTEM}",
                render_role_prompt(&for_planner, &empty, None)
            ),
            worker_referee_system: format!(
                "{}\n{DEFAULT_SYSTEM}",
                render_role_prompt(&shared, &empty, None)
            ),
            planner_producer_base,
            worker_role_descriptions: Vec::new(),
            worker_role_policies: std::collections::HashMap::new(),
            provides: Vec::new(),
        }
    }
}

/// The planner protocol footer — and therefore the task output schema — for
/// a Plan Producer.
///
/// The [Plan node](crate::machines::scheduler::NodeKind::Plan) is the point
/// where tasks may be assigned worker roles and concrete file operations, or
/// escalate to further planning. `has_worker_roles` must reflect whether the
/// active adapter's [`RolePolicy::worker_role_descriptions`] is non-empty:
/// `kind: "work"` is only offered when there is at least one worker role to
/// assign a work task to — an adapter with none (e.g. a pure decomposition
/// adapter with no `workers:` configured) can only escalate to further
/// planning or emit pure planner intent. Callers only ever invoke this for a
/// Plan node — the [Work node](crate::machines::scheduler::NodeKind::Work)
/// builds its Producer system prompt from `worker_producer_system` instead.
///
/// `provides` is the active adapter's declared
/// [`crate::project::yaml_config::PlannerConfig::provides`] — the exact
/// `task_kv` keys this adapter's tasks must carry. The framework-level
/// footer constants describe `task_kv`'s *shape* only (an object of string
/// keys to string values, same as the grammar); which keys are actually
/// required is adapter-specific, so it is appended here as a dynamic line
/// rather than baked into a `&'static str` constant.
pub(crate) fn planner_protocol_schema_for(has_worker_roles: bool, provides: &[String]) -> String {
    let base = if has_worker_roles {
        PLANNER_PROTOCOL_FOOTER_WITH_OPERATION_AND_ROLES
    } else {
        PLANNER_PROTOCOL_FOOTER_WITH_OPERATION_NO_WORK
    };
    format!("{base}\n{}", task_kv_schema_line(provides))
}

/// Describes `task_kv`'s required contents for the Plan Producer prompt.
///
/// Generic ("an object of string keys to string values") when `provides` is
/// empty — an adapter that declares no `provides` imposes no requirement —
/// otherwise names the exact keys, since the grammar itself only constrains
/// `task_kv`'s shape, not its keys (see [`PLANNER_GBNF_WITH_ROLES`]'s doc).
fn task_kv_schema_line(provides: &[String]) -> String {
    if provides.is_empty() {
        "- `task_kv` must be a JSON object of string keys to string values.".to_string()
    } else {
        let keys = provides
            .iter()
            .map(|key| format!("`{key}`"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "- `task_kv` must be a JSON object containing exactly these keys, each with a \
             non-empty string value: {keys}."
        )
    }
}

#[cfg(test)]
#[path = "gbnf_check.rs"]
pub(crate) mod gbnf_check;

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
