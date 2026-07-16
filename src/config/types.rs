//! Forge configuration types loaded from a YAML file.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize};

use super::team_triggers;
use crate::language::LanguageSpec;
use crate::machines::scheduler::NodeKind;

/// Top-level configuration for a forge run.
#[derive(Debug, Deserialize)]
pub struct ForgeConfig {
    /// The objective the run will work toward. Required.
    #[serde(default)]
    pub objective: Option<String>,
    /// Team definitions for multi-team runs.
    ///
    /// Each team's `trigger` is evaluated against the task manifest on every
    /// `IntegrationSucceeded`/`PlannerTasksIntegrated` transition to decide
    /// when to spawn its nodes. Spawned nodes carry the team's `adapter`/
    /// `northstar` paths (see `Node::adapter`/`Node::northstar`), and the
    /// runner loads and runs each such node under its own team's adapter/
    /// northstar instead of the run's top-level one.
    #[serde(default)]
    pub teams: Vec<TeamConfig>,
    /// Team names that no other team's `Trigger::AfterTeams` list names —
    /// i.e. teams nothing else is scheduled to run after. Computed once by
    /// `team_triggers::compute_terminal_teams` at config-load time (see
    /// `ForgeConfig::from_file`), not re-derived per trigger evaluation.
    #[serde(default)]
    pub terminal_teams: Vec<String>,
    /// Artifact repository settings.
    pub artifact: ArtifactConfig,
    /// Provider settings.
    pub provider: ProviderConfig,
    /// Telemetry settings.
    pub telemetry: TelemetryConfig,
    /// Optional validation commands run after workspace update, before integration.
    #[serde(default)]
    pub validation: Option<ValidationConfig>,
    /// Path to the project adapter YAML file governing role prompt policy
    /// (e.g. `"adapters/planner.yaml"`). Required; there is no default. A
    /// relative path is resolved against the directory containing the
    /// config file, like `artifact.repo_path`.
    #[serde(default)]
    pub adapter: String,
    /// The single language this engagement targets, matching one of the
    /// extensions declared by the root `adapter`'s language plugins (e.g.
    /// `"py"`, `"rs"`). Required.
    ///
    /// Resolves language-plugin selection once per engagement, at config-load
    /// time, rather than per node or per task: concerns with no target file
    /// of their own to select a plugin from (repo bootstrap, API summaries,
    /// the handler-level fallback validator, and the planner's own prompt
    /// context) all key off this value instead of an arbitrary
    /// `BTreeMap<String, LanguageSpec>` iteration order.
    #[serde(default)]
    pub language: String,
    /// Maximum number of nodes the scheduler may have `Running`/`Integrating`
    /// at once (`RunConfig::dispatch_cap`).
    ///
    /// This is a separate knob from any one provider's `--parallel`/
    /// `ResourceManager` capacity: a node can be in flight — spawned,
    /// running local tool calls, or blocked waiting for a provider permit —
    /// without consuming a provider slot at every instant, so keeping more
    /// nodes in flight than any single provider's concurrent-call capacity
    /// keeps that provider saturated whenever a permit frees up. Defaults to
    /// 4, a fixed value chosen to give real overlap for typical
    /// `--parallel 2-4` provider setups without spawning an unbounded
    /// number of dispatch threads.
    #[serde(default = "default_dispatch_cap")]
    pub dispatch_cap: usize,
}

fn default_dispatch_cap() -> usize {
    4
}

/// One team entry in `teams`. A team executes work under its own northstar
/// and adapter, activated according to its `trigger`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct TeamConfig {
    /// The team's name, referenced by other teams' `trigger` expressions.
    pub name: String,
    /// Path to a plain text file describing this team's desired end state,
    /// resolved relative to the config file.
    pub northstar: String,
    /// Path to this team's project adapter YAML file, resolved relative to
    /// the config file.
    pub adapter: String,
    /// Whether this team's spawned nodes are planning or execution nodes.
    /// Must agree with `trigger`: `Plan` pairs with `Trigger::Start`, `Work`
    /// pairs with `Trigger::AfterTeams`. Checked by `resolve_team_paths` at
    /// config-load time so the pairing can never silently diverge.
    pub kind: NodeKind,
    /// Parsed trigger expression, e.g. `start` or
    /// `after_teams(team_a, team_b)`.
    pub trigger: Trigger,
    /// This team's `adapter`-declared language plugins, keyed by extension —
    /// populated by [`ForgeConfig::from_file`]'s `resolve_team_paths` step
    /// from the adapter loaded for `adapter`, so per-node plugin selection
    /// (e.g. `required_validation_targets`) can happen without adapter-YAML
    /// I/O from inside the (pure) scheduler transition that dispatches a
    /// node.
    #[serde(default)]
    pub language_plugins: BTreeMap<String, LanguageSpec>,
    /// Whether this team's `adapter`'s first configured worker role derives
    /// its `ForTasks`-spawned node's target file from the task's source
    /// `file_path` (see
    /// [`crate::project::YamlProjectAdapter::primary_role_derives_target`]),
    /// rather than using `file_path` directly — resolved at config-load
    /// time so the scheduler's node-spawn transition doesn't perform any
    /// adapter-YAML I/O itself.
    #[serde(default)]
    pub derives_target: bool,
    /// The engagement-wide active language (`ForgeConfig::language`), copied
    /// onto every team by `resolve_team_paths` so team-scoped dispatch can
    /// select the one active plugin from `language_plugins` without a
    /// back-reference to the root config.
    #[serde(default)]
    pub language: String,
}

/// Parsed form of a `TeamConfig::trigger` expression, consumed by
/// `crate::services::team_trigger::evaluate_trigger` on every scheduler
/// task-completion transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// Runs once, when the forge run starts.
    Start,
    /// Runs after every named team has completed.
    AfterTeams(Vec<String>),
}

impl TryFrom<String> for Trigger {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let trimmed = value.trim();
        if trimmed == "start" {
            return Ok(Self::Start);
        }
        let Some(inner) = trimmed
            .strip_prefix("after_teams(")
            .and_then(|rest| rest.strip_suffix(')'))
        else {
            return Err(format!(
                "trigger must be 'start' or 'after_teams(team[, team...])', got '{value}'"
            ));
        };
        let teams = inner
            .split(',')
            .map(|s| {
                let s = s.trim();
                if s.is_empty() {
                    Err(format!(
                        "after_teams(...) contains an empty team name in '{value}'"
                    ))
                } else {
                    Ok(s.to_string())
                }
            })
            .collect::<Result<Vec<String>, String>>()?;
        Ok(Self::AfterTeams(teams))
    }
}

impl<'de> Deserialize<'de> for Trigger {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Trigger::try_from(raw).map_err(serde::de::Error::custom)
    }
}

/// Mirrors the custom `Deserialize` impl (parsed from `"start"` /
/// `"after_teams(team[, team...])"`) so `Trigger` round-trips through
/// checkpoint serialization as the same string form it was parsed from.
impl Serialize for Trigger {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let rendered = match self {
            Trigger::Start => "start".to_string(),
            Trigger::AfterTeams(teams) => format!("after_teams({})", teams.join(", ")),
        };
        serializer.serialize_str(&rendered)
    }
}

/// Artifact repository configuration.
#[derive(Debug, Deserialize)]
pub struct ArtifactConfig {
    /// Path to the bare git repository that stores artifact commits.
    pub repo_path: String,
    /// Branch name used within the repository.
    pub branch: String,
}

fn default_provider_timeout_seconds() -> u64 {
    300
}

fn default_managed_startup_timeout_seconds() -> u64 {
    60
}

fn default_parallel() -> usize {
    1
}

/// LLM provider configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// Cheap/default provider tier.
    pub cheap: ProviderTierConfig,
    /// Optional strong provider tier. When absent, strong falls back to cheap.
    #[serde(default)]
    pub strong: Option<ProviderTierConfig>,
    /// HTTP request timeout in seconds. Absent configs default to 300.
    #[serde(default = "default_provider_timeout_seconds")]
    pub timeout_seconds: u64,
    /// Timeout for strong-tier completions in seconds. Falls back to
    /// `timeout_seconds` when absent.
    #[serde(default)]
    pub strong_timeout_seconds: Option<u64>,
}

/// Configuration for one provider tier.
#[derive(Debug, Clone)]
pub enum ProviderTierConfig {
    /// Forge connects to an already-running provider server.
    Unmanaged(UnmanagedProviderConfig),
    /// Forge owns the local provider server process.
    Managed(ManagedProviderConfig),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderTierConfigDef {
    #[serde(default)]
    unmanaged: Option<UnmanagedProviderConfig>,
    #[serde(default)]
    managed: Option<ManagedProviderConfig>,
}

impl<'de> Deserialize<'de> for ProviderTierConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let def = ProviderTierConfigDef::deserialize(deserializer)?;
        match (def.unmanaged, def.managed) {
            (Some(config), None) => Ok(Self::Unmanaged(config)),
            (None, Some(config)) => Ok(Self::Managed(config)),
            (Some(_), Some(_)) => Err(serde::de::Error::custom(
                "provider tier must specify exactly one of unmanaged or managed",
            )),
            (None, None) => Err(serde::de::Error::custom(
                "provider tier must specify unmanaged or managed",
            )),
        }
    }
}

/// Already-running provider server configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnmanagedProviderConfig {
    /// Base URL of the provider server (e.g. `"http://localhost:8080"`).
    pub base_url: String,
    /// Expected model served by the provider at `base_url`.
    pub model: String,
    /// Maximum tokens to predict per completion call.
    pub n_predict: usize,
    /// Which HTTP API dialect `base_url` speaks. Defaults to `llama_cpp`,
    /// the dialect every unmanaged config used before `ollama` existed.
    #[serde(default)]
    pub backend: ProviderBackend,
    /// Number of concurrent requests this already-running provider server
    /// can safely serve at once. Sizes this tier's
    /// [`crate::runtime::ResourceManager`] permit pool. Defaults to 1,
    /// matching prior single-request behavior.
    #[serde(default = "default_parallel")]
    pub parallel: usize,
}

/// HTTP API dialect an unmanaged (or managed) provider server speaks.
///
/// Selects which [`crate::providers::ProviderClient`] implementation talks
/// to a tier's `base_url`; the two servers are not wire-compatible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderBackend {
    /// llama.cpp `llama-server`'s `/completion` API.
    #[default]
    LlamaCpp,
    /// Ollama's `/api/generate` API.
    Ollama,
}

/// Managed local provider server configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedProviderConfig {
    /// llama.cpp server management settings.
    pub llama_cpp: ManagedLlamaCppConfig,
}

/// Managed llama.cpp `llama-server` process configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedLlamaCppConfig {
    /// Executable path or command name for `llama-server`.
    pub command: String,
    /// Model source passed to `llama-server`.
    pub model: ManagedLlamaCppModelConfig,
    /// Host passed to `llama-server --host`.
    pub host: String,
    /// Port passed to `llama-server --port`.
    pub port: u16,
    /// Optional llama.cpp context size, passed as `--ctx-size`.
    #[serde(default)]
    pub context_size: Option<usize>,
    /// Seconds to wait for readiness after spawning the process.
    #[serde(default = "default_managed_startup_timeout_seconds")]
    pub startup_timeout_seconds: u64,
    /// Maximum tokens to predict per completion call.
    pub n_predict: usize,
    /// Number of concurrent slots to request from `llama-server` via
    /// `--parallel`. Sizes this tier's [`crate::runtime::ResourceManager`]
    /// permit pool. Defaults to 1, matching prior single-request behavior.
    #[serde(default = "default_parallel")]
    pub parallel: usize,
}

/// Managed llama.cpp model source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedLlamaCppModelConfig {
    /// Local GGUF model path passed as `--model <path>`.
    Path(String),
    /// Hugging Face llama.cpp reference passed as `-hf <repo:quant>`.
    HuggingFace(String),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedLlamaCppModelConfigDef {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    hf: Option<String>,
}

impl<'de> Deserialize<'de> for ManagedLlamaCppModelConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum ModelDef {
            LocalPathString(String),
            Source(ManagedLlamaCppModelConfigDef),
        }

        match ModelDef::deserialize(deserializer)? {
            ModelDef::LocalPathString(path) => Ok(Self::Path(path)),
            ModelDef::Source(def) => match (def.path, def.hf) {
                (Some(path), None) => Ok(Self::Path(path)),
                (None, Some(hf)) => Ok(Self::HuggingFace(hf)),
                (Some(_), Some(_)) => Err(serde::de::Error::custom(
                    "managed llama.cpp model must specify exactly one of path or hf",
                )),
                (None, None) => Err(serde::de::Error::custom(
                    "managed llama.cpp model must specify path or hf",
                )),
            },
        }
    }
}

impl ManagedLlamaCppModelConfig {
    /// User-facing model identity used in run metadata.
    pub fn identity(&self) -> &str {
        match self {
            Self::Path(path) => path,
            Self::HuggingFace(hf) => hf,
        }
    }
}

/// Telemetry output configuration.
#[derive(Debug, Deserialize)]
pub struct TelemetryConfig {
    /// Directory path where telemetry files will be written.
    pub directory: String,
}

/// Validation configuration for post-workspace-update checks.
#[derive(Debug, Deserialize)]
pub struct ValidationConfig {
    /// Shell commands run in order inside the workspace; stop on first failure.
    pub commands: Vec<String>,
    /// Maximum seconds to wait for each command. Stored but not yet enforced.
    pub timeout_seconds: Option<u64>,
}

impl ForgeConfig {
    /// Load a `ForgeConfig` from a YAML file at `path`.
    ///
    /// `adapter` is resolved and loaded first, before any other config field
    /// is validated or any provider/artifact setup happens — a missing or
    /// invalid adapter (or any language plugin it declares) fails
    /// immediately. Each team's `adapter`/`northstar` is resolved and
    /// validated the same way immediately after. Relative paths (in
    /// `adapter`, each team's `adapter`/`northstar`, `artifact.repo_path`,
    /// and `telemetry.directory`) are resolved against the directory
    /// containing the config file, not the process working directory.
    ///
    /// Returns an error if:
    /// - `adapter` is absent or blank.
    /// - `adapter` does not resolve to a loadable adapter YAML file, or any
    ///   plugin it declares fails to load.
    /// - any team's `adapter` is blank, or does not resolve to a loadable
    ///   adapter YAML file (or plugin it declares fails to load), or declares
    ///   a worker role that is missing from one of its plugins' `plugin_roles`
    ///   lists.
    /// - any team's `northstar` is blank, or does not resolve to a readable
    ///   file.
    /// - the team-trigger graph formed by `Trigger::AfterTeams` references
    ///   contains a cycle (a team whose `after_teams` chain transitively
    ///   refers back to itself).
    pub fn from_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let config_path = Path::new(path);
        let content = std::fs::read_to_string(config_path)?;
        let mut config: ForgeConfig = serde_yaml::from_str(&content)?;

        // Resolve relative paths against the config file's directory so that
        // `forge start path/to/forge.yaml` works correctly from any cwd.
        let config_dir = config_path.parent().filter(|p| !p.as_os_str().is_empty());

        if config.objective.is_none() {
            return Err("objective is required".into());
        }

        if config.adapter.trim().is_empty() {
            return Err("adapter is required".into());
        }
        if config.language.trim().is_empty() {
            return Err("language is required".into());
        }
        if let Some(dir) = config_dir {
            config.adapter = resolve_relative(&config.adapter, dir);
        }
        let root_adapter = crate::project::load_adapter(Path::new(&config.adapter))?;
        if !root_adapter.language_plugins().is_empty()
            && !root_adapter
                .language_plugins()
                .contains_key(&config.language)
        {
            return Err(format!(
                "language '{}' matches no plugin extension declared by adapter '{}'",
                config.language, config.adapter
            )
            .into());
        }

        let mut provides_all: Vec<String> = Vec::new();
        let mut requirements_all: Vec<TaskKeyRequirement> = Vec::new();
        collect_adapter_task_keys(
            &root_adapter,
            &config.adapter,
            &mut provides_all,
            &mut requirements_all,
        );

        resolve_team_paths(
            &mut config.teams,
            config_dir,
            &config.language,
            &mut provides_all,
            &mut requirements_all,
        )?;
        validate_task_key_consistency(&provides_all, &requirements_all)?;
        config.terminal_teams = team_triggers::compute_terminal_teams(&config.teams)?;

        if let Some(dir) = config_dir {
            config.artifact.repo_path = resolve_relative(&config.artifact.repo_path, dir);
            config.telemetry.directory = resolve_relative(&config.telemetry.directory, dir);
        }

        validate_provider_model_identity(&config.provider)?;

        if config.dispatch_cap == 0 {
            return Err("dispatch_cap must be at least 1".into());
        }

        Ok(config)
    }

    /// The run's root objective text. `from_file` guarantees `objective` is
    /// present.
    pub fn root_objective(&self) -> &str {
        self.objective
            .as_deref()
            .expect("from_file guarantees objective is set")
    }
}

fn validate_provider_model_identity(
    provider: &ProviderConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_provider_tier("provider.cheap", &provider.cheap)?;
    if let Some(strong) = &provider.strong {
        validate_provider_tier("provider.strong", strong)?;
    }
    if provider.timeout_seconds == 0 {
        return Err("provider.timeout_seconds must be positive".into());
    }
    if let Some(timeout) = provider.strong_timeout_seconds
        && timeout == 0
    {
        return Err("provider.strong_timeout_seconds must be positive when set".into());
    }
    Ok(())
}

fn validate_provider_tier(
    field: &str,
    tier: &ProviderTierConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    match tier {
        ProviderTierConfig::Unmanaged(config) => {
            if config.base_url.trim().is_empty() {
                return Err(format!("{field}.unmanaged.base_url must be non-empty").into());
            }
            validate_http_url(&format!("{field}.unmanaged.base_url"), &config.base_url)?;
            if config.model.trim().is_empty() {
                return Err(format!("{field}.unmanaged.model must be non-empty").into());
            }
        }
        ProviderTierConfig::Managed(managed) => {
            let llama = &managed.llama_cpp;
            if llama.command.trim().is_empty() {
                return Err(format!("{field}.managed.llama_cpp.command must be non-empty").into());
            }
            if llama.port == 0 {
                return Err(format!("{field}.managed.llama_cpp.port must not be 0").into());
            }
            match &llama.model {
                ManagedLlamaCppModelConfig::Path(path) if path.trim().is_empty() => {
                    return Err(
                        format!("{field}.managed.llama_cpp.model.path must be non-empty").into(),
                    );
                }
                ManagedLlamaCppModelConfig::HuggingFace(hf) if hf.trim().is_empty() => {
                    return Err(
                        format!("{field}.managed.llama_cpp.model.hf must be non-empty").into(),
                    );
                }
                _ => {}
            }
            if llama.host.trim().is_empty() {
                return Err(format!("{field}.managed.llama_cpp.host must be non-empty").into());
            }
            if llama.startup_timeout_seconds == 0 {
                return Err(format!(
                    "{field}.managed.llama_cpp.startup_timeout_seconds must be positive"
                )
                .into());
            }
        }
    }
    Ok(())
}

fn validate_http_url(field: &str, value: &str) -> Result<(), Box<dyn std::error::Error>> {
    let trimmed = value.trim();
    let Some((scheme, rest)) = trimmed.split_once("://") else {
        return Err(format!(
            "{field} must be a valid URL with an http or https scheme (got '{value}')"
        )
        .into());
    };
    if scheme != "http" && scheme != "https" {
        return Err(format!(
            "{field} must use the http or https scheme, got '{scheme}' (in '{value}')"
        )
        .into());
    }
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.trim().is_empty() {
        return Err(format!("{field} must include a host (got '{value}')").into());
    }
    Ok(())
}

fn resolve_relative(path_str: &str, base: &Path) -> String {
    let p = Path::new(path_str);
    if p.is_absolute() {
        path_str.to_string()
    } else {
        base.join(p).to_string_lossy().into_owned()
    }
}

/// Resolves and validates each team's `adapter`/`northstar` in place, the
/// same way `ForgeConfig::from_file` resolves and validates the top-level
/// `adapter` field — including the worker-role/plugin check, so a team with
/// a worker role missing from its plugins' `plugin_roles` lists fails here rather
/// than at first dispatch.
///
/// Each team's adapter's declared `provides`/`requires` are appended to
/// `provides_all`/`requirements_all` as they're loaded, for
/// `validate_task_key_consistency` to check once every adapter (root and
/// team) has been collected.
fn resolve_team_paths(
    teams: &mut [TeamConfig],
    config_dir: Option<&Path>,
    language: &str,
    provides_all: &mut Vec<String>,
    requirements_all: &mut Vec<TaskKeyRequirement>,
) -> Result<(), Box<dyn std::error::Error>> {
    for team in teams {
        if team.adapter.trim().is_empty() {
            return Err(format!("team '{}': adapter is required", team.name).into());
        }
        if team.northstar.trim().is_empty() {
            return Err(format!("team '{}': northstar is required", team.name).into());
        }
        match (&team.kind, &team.trigger) {
            (NodeKind::Plan, Trigger::Start) | (NodeKind::Work, Trigger::AfterTeams(_)) => {}
            (NodeKind::Plan, Trigger::AfterTeams(_)) => {
                return Err(format!(
                    "team '{}': kind 'plan' requires trigger 'start', got 'after_teams(...)'",
                    team.name
                )
                .into());
            }
            (NodeKind::Work, Trigger::Start) => {
                return Err(format!(
                    "team '{}': kind 'work' requires trigger 'after_teams(...)', got 'start'",
                    team.name
                )
                .into());
            }
        }
        if let Some(dir) = config_dir {
            team.adapter = resolve_relative(&team.adapter, dir);
            team.northstar = resolve_relative(&team.northstar, dir);
        }
        let adapter = crate::project::load_adapter(Path::new(&team.adapter))?;
        crate::node_runner::project_setup::validate_worker_roles(
            &adapter,
            adapter.language_plugins(),
        )
        .map_err(|e| format!("team '{}': {e}", team.name))?;
        if !adapter.language_plugins().is_empty()
            && !adapter.language_plugins().contains_key(language)
        {
            return Err(format!(
                "team '{}': language '{language}' matches no plugin extension declared by adapter '{}'",
                team.name, team.adapter
            )
            .into());
        }
        team.derives_target = adapter.primary_role_derives_target();
        team.language_plugins = adapter.language_plugins().clone();
        team.language = language.to_string();
        collect_adapter_task_keys(&adapter, &team.adapter, provides_all, requirements_all);
        std::fs::metadata(&team.northstar).map_err(|e| {
            format!(
                "team '{}': northstar at {} could not be read: {e}",
                team.name, team.northstar
            )
        })?;
    }
    Ok(())
}

/// One worker role's declared need for a specific `task_kv` key, tagged
/// with where it came from for [`validate_task_key_consistency`]'s error
/// message.
#[derive(Debug, Clone, PartialEq)]
struct TaskKeyRequirement {
    /// Path to the adapter YAML file declaring this requirement.
    adapter_path: String,
    /// The worker role name declaring this requirement (empty when the role
    /// declares no `plugin_role`).
    role_name: String,
    /// The required `task_kv` key.
    key: String,
}

/// Appends `adapter`'s declared `PlannerConfig::provides` to `provides_all`
/// and every one of its worker roles' declared `requires` to
/// `requirements_all`, tagged with `adapter_path` for error reporting.
fn collect_adapter_task_keys(
    adapter: &crate::project::YamlProjectAdapter,
    adapter_path: &str,
    provides_all: &mut Vec<String>,
    requirements_all: &mut Vec<TaskKeyRequirement>,
) {
    provides_all.extend(adapter.provides().iter().cloned());
    for worker in adapter.worker_roles() {
        let role_name = worker.plugin_role.clone().unwrap_or_default();
        requirements_all.extend(worker.requires.iter().map(|key| TaskKeyRequirement {
            adapter_path: adapter_path.to_string(),
            role_name: role_name.clone(),
            key: key.clone(),
        }));
    }
}

/// Confirms every declared `requires` key across every adapter in this
/// engagement (root and every team) is covered by some adapter's declared
/// `provides` — the same task manifest is shared by every team, so any
/// team's `ForTasks`-spawned node can read any planner-produced row
/// regardless of which team's `trigger` chain it sits behind, and this
/// checks that "some declared source" exists for everything anyone needs
/// rather than deriving `provides` from the union of `requires` itself (see
/// [`crate::project::yaml_config::PlannerConfig::provides`]'s doc for why
/// that would defeat the point).
///
/// A gap here is a config-load error, not a runtime one: this is the same
/// treatment as the adapter/plugin/worker-role checks already performed by
/// `ForgeConfig::from_file`/`resolve_team_paths`.
fn validate_task_key_consistency(
    provides_all: &[String],
    requirements_all: &[TaskKeyRequirement],
) -> Result<(), Box<dyn std::error::Error>> {
    for requirement in requirements_all {
        if !provides_all.iter().any(|p| p == &requirement.key) {
            return Err(format!(
                "adapter '{}' worker role '{}' requires task_kv key '{}', but no adapter in this \
                 engagement declares it in planner.provides",
                requirement.adapter_path, requirement.role_name, requirement.key
            )
            .into());
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "types_tests.rs"]
mod tests;
