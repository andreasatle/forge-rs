//! Forge runtime — wires config into machines and drives a single run.

use std::error::Error;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::process::Command;

use crate::artifacts::{Artifact, ArtifactView};
use crate::config::{
    ForgeConfig, ManagedProviderConfig, ProjectConfig, ProjectKind, ProviderConfig,
    ProviderTierConfig, ValidationConfig,
};
use crate::engine::run_machine_with_telemetry;
use crate::language::registry::language_spec;

use super::repo::load_or_create_artifact;
use crate::machines::scheduler::state::{RunConfig, SchedulerState};
use crate::machines::scheduler::{RunRequest, SchedulerHandler, SchedulerMachine, SchedulerOutput};
use crate::node_runner::{DeliberatingNodeRunner, TestTargetsFn};
use crate::project::{CodingProjectAdapter, DefaultProjectAdapter, ProjectAdapter as _};
use crate::providers::{LlamaCppProvider, RetryingProvider};
use crate::roles::RolePolicy;
use crate::runtime::checkpoint::node_counts;
use crate::runtime::managed_provider::{
    ManagedLlamaCppRuntimeConfig, ManagedProviderServer, resolve_llama_cpp_config,
};
use crate::runtime::resume::find_resumable_run;
use crate::runtime::{
    ManagedProviderServerMetadata, ProviderRunMetadata, ProviderTierMetadata, create_run,
    finalize_manifest,
};
use crate::telemetry::{FileTelemetry, TelemetryEvent, TelemetryRecord, TelemetrySink};
use crate::validation::{
    AlwaysPassValidator, CommandSpec, CommandValidator, ValidationPlan, ValidationScope,
    ValidationStage, ValidationStep, Validator,
};

/// Entry point for a single forge run driven by a [`ForgeConfig`].
pub struct ForgeRuntime;

impl ForgeRuntime {
    /// Run forge to completion using the given config.
    ///
    /// Responsibilities:
    /// 1. Load or create the bare artifact repository.
    /// 2. Create the telemetry sink.
    /// 3. Build the provider stack.
    /// 4. Drive the scheduler to completion.
    /// 5. Print a summary to stdout.
    pub fn run(config: ForgeConfig) -> Result<(), Box<dyn Error>> {
        let artifact =
            load_or_create_artifact(&config.artifact, config.project.language.as_deref())?;

        let runs_root = PathBuf::from(&config.telemetry.directory);
        let provider_metadata = make_provider_run_metadata(&config.provider)?;
        let run_info = create_run(
            &runs_root,
            &config.objective,
            &config.artifact.repo_path,
            &provider_metadata,
        )?;
        eprintln!("[run] started {}", run_info.run_id);
        let sink: Rc<dyn TelemetrySink> =
            Rc::new(FileTelemetry::new(run_info.telemetry_dir.clone()));

        let _managed_provider_servers = start_managed_provider_servers(&config.provider)?;

        let cheap_llama = LlamaCppProvider::new(
            &provider_metadata.cheap.base_url,
            provider_metadata.cheap.timeout_seconds,
        );
        let cheap = RetryingProvider::new(cheap_llama, 3);

        let strong_llama = LlamaCppProvider::new(
            &provider_metadata.strong.base_url,
            provider_metadata.strong.timeout_seconds,
        );
        let strong = RetryingProvider::new(strong_llama, 3);

        let cheap_tokens = provider_metadata.cheap.n_predict as u32;
        let strong_tokens = provider_metadata.strong.n_predict as u32;

        let role_policy = make_role_policy(&config.project);
        let context_file_names = make_context_file_names(&config.project);
        let required_test_targets_fn =
            make_required_test_targets_fn(&config.project, config.validation.as_ref());
        let validation_plan = make_validation_plan(
            config.project.language.as_deref(),
            config.validation.as_ref(),
        )?;
        let runner = DeliberatingNodeRunner::new(cheap, strong)
            .with_cheap_max_tokens(cheap_tokens)
            .with_strong_max_tokens(strong_tokens)
            .with_role_policy(role_policy)
            .with_required_test_targets_fn(required_test_targets_fn)
            .with_context_file_names(context_file_names)
            .with_validation_plan(validation_plan);
        let validator = make_validator(
            config.project.language.as_deref(),
            config.validation.as_ref(),
        )?;
        let handler = SchedulerHandler::with_artifact(runner, artifact)
            .with_telemetry(Rc::clone(&sink))
            .with_validator(validator)
            .with_checkpoint_dir(run_info.run_dir.clone());

        let initial_state = SchedulerMachine::initial_state(
            RunRequest {
                objective: config.objective.clone(),
            },
            RunConfig {
                has_strong_tier: config.provider.strong.is_some(),
            },
        );

        let (output, handler) = run_machine_with_telemetry(handler, initial_state, sink.as_ref());
        print_run_progress_result(&output);

        let final_artifact = handler.artifact();
        let validation_passed = handler.validation_passed();
        print_summary(&output, &config, final_artifact.as_ref(), &run_info);

        let failure_reason_str: Option<String> =
            if let SchedulerOutput::Failed { reason, .. } = &output {
                Some(reason.to_string())
            } else {
                None
            };
        let (status, final_commit) = match &output {
            SchedulerOutput::Complete { .. } => (
                "succeeded",
                final_artifact.as_ref().map(|a| a.commit_sha.as_str()),
            ),
            SchedulerOutput::Failed { .. } => ("failed", None),
        };
        if let Err(e) = finalize_manifest(
            &run_info,
            status,
            final_commit,
            validation_passed,
            failure_reason_str.as_deref(),
        ) {
            eprintln!("warning: failed to finalize manifest: {e}");
        }

        runtime_result_from_scheduler_output(output)
    }

    /// Resume a previously interrupted forge run.
    ///
    /// Scans `config.telemetry.directory` for a run whose `manifest.json` has
    /// `status == "running"` and loads its `graph.json` checkpoint. Exactly one
    /// such run must exist; zero or multiple produce a clear error.
    ///
    /// The restored state is normalized before re-entry: any node that was
    /// mid-execution at crash time is reset to `Pending` so the scheduler
    /// re-dispatches it. Completed work (durable in git) is preserved.
    pub fn resume(config: ForgeConfig) -> Result<(), Box<dyn Error>> {
        let runs_root = PathBuf::from(&config.telemetry.directory);
        let (run_dir, initial_state) = find_resumable_run(&runs_root)?;
        // Re-derive has_strong_tier: it describes what provider tiers exist *now*,
        // not run history, so stale or pre-fix checkpoints don't silently inherit
        // the wrong value.
        let has_strong_tier = config.provider.strong.is_some();
        let initial_state = match initial_state {
            SchedulerState::Active { graph, .. } => SchedulerState::Active {
                graph,
                run_config: RunConfig { has_strong_tier },
            },
            SchedulerState::Waiting { graph, .. } => SchedulerState::Waiting {
                graph,
                run_config: RunConfig { has_strong_tier },
            },
            other => other,
        };
        let run_id = run_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        eprintln!("[run] resumed {run_id}");

        let artifact =
            load_or_create_artifact(&config.artifact, config.project.language.as_deref())?;
        let sink: Rc<dyn TelemetrySink> = Rc::new(FileTelemetry::new(run_dir.join("telemetry")));

        let graph = match &initial_state {
            SchedulerState::Active { graph, .. } => graph,
            _ => unreachable!("normalize_for_resume always returns Active"),
        };
        let (node_count, completed_count) = node_counts(graph);
        sink.record(TelemetryRecord::new(
            "Checkpoint",
            TelemetryEvent::CheckpointLoaded {
                node_count,
                completed_count,
            },
        ));

        let provider_metadata = make_provider_run_metadata(&config.provider)?;
        let _managed_provider_servers = start_managed_provider_servers(&config.provider)?;

        let cheap_llama = LlamaCppProvider::new(
            &provider_metadata.cheap.base_url,
            provider_metadata.cheap.timeout_seconds,
        );
        let cheap = RetryingProvider::new(cheap_llama, 3);

        let strong_llama = LlamaCppProvider::new(
            &provider_metadata.strong.base_url,
            provider_metadata.strong.timeout_seconds,
        );
        let strong = RetryingProvider::new(strong_llama, 3);

        let cheap_tokens = provider_metadata.cheap.n_predict as u32;
        let strong_tokens = provider_metadata.strong.n_predict as u32;

        let role_policy = make_role_policy(&config.project);
        let context_file_names = make_context_file_names(&config.project);
        let required_test_targets_fn =
            make_required_test_targets_fn(&config.project, config.validation.as_ref());
        let validation_plan = make_validation_plan(
            config.project.language.as_deref(),
            config.validation.as_ref(),
        )?;
        let runner = DeliberatingNodeRunner::new(cheap, strong)
            .with_cheap_max_tokens(cheap_tokens)
            .with_strong_max_tokens(strong_tokens)
            .with_role_policy(role_policy)
            .with_required_test_targets_fn(required_test_targets_fn)
            .with_context_file_names(context_file_names)
            .with_validation_plan(validation_plan);
        let validator = make_validator(
            config.project.language.as_deref(),
            config.validation.as_ref(),
        )?;
        let handler = SchedulerHandler::with_artifact(runner, artifact)
            .with_telemetry(Rc::clone(&sink))
            .with_validator(validator)
            .with_checkpoint_dir(run_dir.clone());

        let run_info = crate::runtime::RunInfo {
            run_id,
            run_dir: run_dir.clone(),
            telemetry_dir: run_dir.join("telemetry"),
            started_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
        };

        let (output, handler) = run_machine_with_telemetry(handler, initial_state, sink.as_ref());
        print_run_progress_result(&output);

        let final_artifact = handler.artifact();
        let validation_passed = handler.validation_passed();
        print_summary(&output, &config, final_artifact.as_ref(), &run_info);

        let failure_reason_str: Option<String> =
            if let SchedulerOutput::Failed { reason, .. } = &output {
                Some(reason.to_string())
            } else {
                None
            };
        let (status, final_commit) = match &output {
            SchedulerOutput::Complete { .. } => (
                "succeeded",
                final_artifact.as_ref().map(|a| a.commit_sha.as_str()),
            ),
            SchedulerOutput::Failed { .. } => ("failed", None),
        };
        if let Err(e) = finalize_manifest(
            &run_info,
            status,
            final_commit,
            validation_passed,
            failure_reason_str.as_deref(),
        ) {
            eprintln!("warning: failed to finalize manifest: {e}");
        }

        runtime_result_from_scheduler_output(output)
    }
}

fn make_provider_run_metadata(
    provider: &ProviderConfig,
) -> Result<ProviderRunMetadata, Box<dyn Error>> {
    let cheap = resolve_provider_tier(&provider.cheap, provider.timeout_seconds);
    let strong = match &provider.strong {
        Some(strong) => resolve_provider_tier(
            strong,
            provider
                .strong_timeout_seconds
                .unwrap_or(provider.timeout_seconds),
        ),
        None => {
            let mut strong = cheap.clone();
            strong.timeout_seconds = provider
                .strong_timeout_seconds
                .unwrap_or(provider.timeout_seconds);
            strong
        }
    };
    Ok(ProviderRunMetadata { cheap, strong })
}

fn resolve_provider_tier(tier: &ProviderTierConfig, timeout_seconds: u64) -> ProviderTierMetadata {
    match tier {
        ProviderTierConfig::Unmanaged(config) => ProviderTierMetadata {
            base_url: config.base_url.clone(),
            model: config.model.clone(),
            n_predict: config.n_predict,
            timeout_seconds,
            managed: false,
            managed_server: None,
        },
        ProviderTierConfig::Managed(managed) => {
            let config = resolve_managed_llama_cpp(managed);
            ProviderTierMetadata {
                base_url: config.base_url.clone(),
                model: config.model.identity().to_string(),
                n_predict: managed.llama_cpp.n_predict,
                timeout_seconds,
                managed: true,
                managed_server: Some(managed_server_metadata(&config)),
            }
        }
    }
}

fn resolve_managed_llama_cpp(managed: &ManagedProviderConfig) -> ManagedLlamaCppRuntimeConfig {
    resolve_llama_cpp_config(&managed.llama_cpp)
}

fn managed_server_metadata(config: &ManagedLlamaCppRuntimeConfig) -> ManagedProviderServerMetadata {
    ManagedProviderServerMetadata {
        kind: "llama_cpp".to_string(),
        command: config.command.clone(),
        port: config.port,
        context_size: config.context_size,
        startup_timeout_seconds: config.startup_timeout_seconds,
    }
}

fn start_managed_provider_servers(
    provider: &ProviderConfig,
) -> Result<Vec<ManagedProviderServer>, Box<dyn Error>> {
    let mut servers = Vec::new();
    if let ProviderTierConfig::Managed(managed) = &provider.cheap {
        let config = resolve_managed_llama_cpp(managed);
        servers.push(ManagedProviderServer::start_llama_cpp(&config)?);
    }
    if let Some(ProviderTierConfig::Managed(managed)) = &provider.strong {
        let config = resolve_managed_llama_cpp(managed);
        servers.push(ManagedProviderServer::start_llama_cpp(&config)?);
    }
    Ok(servers)
}

fn runtime_result_from_scheduler_output(output: SchedulerOutput) -> Result<(), Box<dyn Error>> {
    match output {
        SchedulerOutput::Failed { reason, .. } => Err(format!("run failed: {reason}").into()),
        SchedulerOutput::Complete { .. } => Ok(()),
    }
}

fn print_run_progress_result(output: &SchedulerOutput) {
    match output {
        SchedulerOutput::Complete { .. } => eprintln!("[run] complete"),
        SchedulerOutput::Failed { .. } => eprintln!("[run] failed"),
    }
}

fn make_role_policy(project: &ProjectConfig) -> RolePolicy {
    match project.kind {
        ProjectKind::Default => DefaultProjectAdapter.role_policy(),
        ProjectKind::Coding => CodingProjectAdapter.role_policy(),
    }
}

fn make_context_file_names(project: &ProjectConfig) -> Vec<String> {
    match project.kind {
        ProjectKind::Default => DefaultProjectAdapter.context_file_names(),
        ProjectKind::Coding => CodingProjectAdapter.context_file_names(),
    }
}

fn make_required_test_targets_fn(
    project: &ProjectConfig,
    validation: Option<&ValidationConfig>,
) -> Arc<TestTargetsFn> {
    if !project_requires_tests(project.language.as_deref(), validation) {
        return Arc::new(|_| vec![]);
    }
    match project.kind {
        ProjectKind::Coding => {
            Arc::new(|targets| CodingProjectAdapter.required_test_targets(targets))
        }
        ProjectKind::Default => Arc::new(|_| vec![]),
    }
}

fn project_requires_tests(
    language: Option<&str>,
    validation_config: Option<&ValidationConfig>,
) -> bool {
    if let Some(lang) = language
        && let Some(spec) = language_spec(lang)
    {
        return spec.validation_includes_test_command();
    }

    // For user-supplied validation commands there is no YAML spec with an
    // explicit `runs_tests` flag, so we fall back to a heuristic: any token
    // in any command that equals "test" or ends with "test"/"tests" implies a
    // test runner is configured.
    validation_config
        .map(|config| {
            config
                .commands
                .iter()
                .any(|cmd| validation_command_is_test_like(cmd))
        })
        .unwrap_or(false)
}

fn validation_command_is_test_like(cmd: &str) -> bool {
    cmd.split_whitespace().any(|token| {
        let lower = token.to_ascii_lowercase();
        lower == "test" || lower.ends_with("test") || lower.ends_with("tests")
    })
}

/// Build a [`ValidationPlan`] from the language spec or explicit config.
///
/// The plan is stamped onto every Work node at plan-expansion time.  This
/// captures the validation contract at node-creation time so it survives
/// checkpoint/resume unchanged, regardless of any later config edits.
fn make_validation_plan(
    language: Option<&str>,
    validation_config: Option<&ValidationConfig>,
) -> Result<Option<ValidationPlan>, Box<dyn Error>> {
    if let Some(lang) = language {
        let spec = language_spec(lang).ok_or_else(|| format!("unknown language: '{lang}'"))?;
        let steps = spec
            .validation
            .commands
            .into_iter()
            .map(|cmd| ValidationStep {
                command: std::iter::once(cmd.program).chain(cmd.args).collect(),
                when_artifacts_present: cmd.when_files_present,
                scope: cmd.scope,
                stage: ValidationStage::PreIntegration,
                must_pass: true,
            })
            .collect();
        return Ok(Some(ValidationPlan {
            steps,
            timeout_seconds: 120,
        }));
    }

    match validation_config {
        Some(v) if !v.commands.is_empty() => {
            let timeout_seconds = v.timeout_seconds.unwrap_or(120);
            let steps = v
                .commands
                .iter()
                .map(|cmd| ValidationStep {
                    command: vec!["sh".to_string(), "-c".to_string(), cmd.clone()],
                    when_artifacts_present: vec![],
                    scope: ValidationScope::Workspace,
                    stage: ValidationStage::PreIntegration,
                    must_pass: true,
                })
                .collect();
            Ok(Some(ValidationPlan {
                steps,
                timeout_seconds,
            }))
        }
        _ => Ok(None),
    }
}

fn make_validator(
    language: Option<&str>,
    validation_config: Option<&ValidationConfig>,
) -> Result<Rc<dyn Validator>, Box<dyn Error>> {
    if let Some(lang) = language {
        let spec = language_spec(lang).ok_or_else(|| format!("unknown language: '{lang}'"))?;
        let timeout = Duration::from_secs(120);
        return Ok(Rc::new(CommandValidator::new(
            spec.validation.commands,
            timeout,
        )));
    }

    match validation_config {
        Some(v) if !v.commands.is_empty() => {
            let timeout = Duration::from_secs(v.timeout_seconds.unwrap_or(120));
            let specs = v
                .commands
                .iter()
                .map(|cmd| CommandSpec {
                    program: "sh".to_string(),
                    args: vec!["-c".to_string(), cmd.clone()],
                    when_files_present: vec![],
                    scope: ValidationScope::Workspace,
                })
                .collect();
            Ok(Rc::new(CommandValidator::new(specs, timeout)))
        }
        _ => Ok(Rc::new(AlwaysPassValidator)),
    }
}

fn print_summary(
    output: &SchedulerOutput,
    config: &ForgeConfig,
    artifact: Option<&Artifact>,
    run_info: &crate::runtime::RunInfo,
) {
    let result_str = match output {
        SchedulerOutput::Complete { .. } => "COMPLETE",
        SchedulerOutput::Failed { .. } => "FAILED",
    };

    println!("Result      : {result_str}");
    println!("Run ID      : {}", run_info.run_id);
    println!("Artifact repo: {}", config.artifact.repo_path);

    if let Some(a) = artifact {
        let short_sha = &a.commit_sha[..a.commit_sha.len().min(7)];
        println!("Commit      : {short_sha}");
        println!("Telemetry   : {}", run_info.telemetry_dir.display());

        let view = ArtifactView {
            repo_path: a.repo_path.clone(),
            commit_sha: a.commit_sha.clone(),
        };
        if let Ok(files) = view.list_files()
            && !files.is_empty()
        {
            println!("\nGenerated files:");
            for f in &files {
                println!("  {}", f.display());
            }
        }
    } else {
        println!("Commit      : unknown");
        println!("Telemetry   : {}", run_info.telemetry_dir.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ArtifactConfig, ForgeConfig, ProjectConfig, ProjectKind, ProviderConfig, TelemetryConfig,
        UnmanagedProviderConfig,
    };
    use crate::machines::scheduler::machine::{RecoverySummary, SchedulerOutput};
    use crate::machines::scheduler::state::{FailureReason, RunGraph};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_path(label: &str) -> PathBuf {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "forge-runtime-test-{label}-{}-{seq}",
            std::process::id()
        ))
    }

    fn artifact_config(path: &PathBuf) -> ArtifactConfig {
        ArtifactConfig {
            repo_path: path.to_str().unwrap().to_string(),
            branch: "main".to_string(),
        }
    }

    fn test_provider_metadata() -> ProviderRunMetadata {
        make_provider_run_metadata(&unmanaged_provider("provider", "llama-test", 512))
            .expect("test provider metadata must resolve")
    }

    fn unmanaged_provider(base_url: &str, model: &str, n_predict: usize) -> ProviderConfig {
        ProviderConfig {
            cheap: ProviderTierConfig::Unmanaged(UnmanagedProviderConfig {
                base_url: base_url.to_string(),
                model: model.to_string(),
                n_predict,
            }),
            strong: None,
            timeout_seconds: 120,
            strong_timeout_seconds: None,
        }
    }

    fn managed_tier(
        command: &str,
        model: &str,
        host: &str,
        port: u16,
        n_predict: usize,
        context_size: Option<usize>,
        startup_timeout_seconds: u64,
    ) -> ProviderTierConfig {
        ProviderTierConfig::Managed(crate::config::ManagedProviderConfig {
            llama_cpp: crate::config::ManagedLlamaCppConfig {
                command: command.to_string(),
                model: crate::config::ManagedLlamaCppModelConfig::Path(model.to_string()),
                host: host.to_string(),
                port,
                context_size,
                startup_timeout_seconds,
                n_predict,
            },
        })
    }

    fn empty_graph() -> RunGraph {
        RunGraph {
            nodes: vec![],
            next_id: 0,
        }
    }

    // ── adapter selection ────────────────────────────────────────────────────

    #[test]
    fn runtime_selects_coding_adapter() {
        let policy = make_role_policy(&ProjectConfig {
            kind: ProjectKind::Coding,
            language: None,
        });
        assert!(
            policy.planner_producer_system.contains("software planning"),
            "coding adapter must produce software-planning planner prompt; got:\n{}",
            policy.planner_producer_system
        );
        assert!(
            policy
                .worker_producer_system
                .contains("software implementation"),
            "coding adapter must produce software-implementation worker prompt; got:\n{}",
            policy.worker_producer_system
        );
    }

    #[test]
    fn runtime_default_adapter_preserves_behavior() {
        let policy = make_role_policy(&ProjectConfig {
            kind: ProjectKind::Default,
            language: None,
        });
        let expected = crate::project::DefaultProjectAdapter.role_policy();
        assert_eq!(
            policy.worker_producer_system, expected.worker_producer_system,
            "default adapter must produce unchanged worker prompt"
        );
    }

    #[test]
    fn runtime_requires_tests_when_validation_command_runs_tests() {
        let config = ValidationConfig {
            commands: vec!["custom-tool test".to_string()],
            timeout_seconds: None,
        };
        assert!(
            project_requires_tests(None, Some(&config)),
            "validation commands containing a test subcommand must require tests"
        );
    }

    #[test]
    fn runtime_does_not_require_tests_without_test_validation() {
        let config = ValidationConfig {
            commands: vec!["custom-tool lint .".to_string()],
            timeout_seconds: None,
        };
        assert!(
            !project_requires_tests(None, Some(&config)),
            "non-test validation commands must not require test targets"
        );
    }

    #[test]
    fn runtime_threads_provider_timeout() {
        // Verify that timeout_seconds from config reaches the provider constructor.
        // No live HTTP: we only check that the value is read from config and the
        // provider is constructed without error.
        let config = ForgeConfig {
            objective: "test".to_string(),
            artifact: ArtifactConfig {
                repo_path: "/tmp/test.git".to_string(),
                branch: "main".to_string(),
            },
            provider: ProviderConfig {
                timeout_seconds: 42,
                ..unmanaged_provider("http://localhost:8080", "llama-test", 512)
            },
            telemetry: TelemetryConfig {
                directory: "/tmp/telemetry".to_string(),
            },
            validation: None,
            project: ProjectConfig::default(),
        };
        assert_eq!(config.provider.timeout_seconds, 42);
        let metadata = make_provider_run_metadata(&config.provider).unwrap();
        let _provider =
            LlamaCppProvider::new(&metadata.cheap.base_url, metadata.cheap.timeout_seconds);
        // Construction succeeds: the timeout is wired through.
    }

    #[test]
    fn strong_tier_falls_back_when_no_strong_provider_configured() {
        let config = unmanaged_provider("http://localhost:8080", "llama-test", 512);
        // When strong fields are absent, both tiers must resolve to the cheap values.
        let metadata = make_provider_run_metadata(&config).unwrap();
        assert_eq!(metadata.strong.base_url, "http://localhost:8080");
        assert_eq!(metadata.strong.n_predict, 512);
        assert_eq!(metadata.strong.timeout_seconds, 120);
    }

    #[test]
    fn provider_run_metadata_uses_effective_tier_identities() {
        let config = ProviderConfig {
            cheap: ProviderTierConfig::Unmanaged(UnmanagedProviderConfig {
                base_url: "http://localhost:8080".to_string(),
                model: "cheap-model".to_string(),
                n_predict: 512,
            }),
            strong: Some(ProviderTierConfig::Unmanaged(UnmanagedProviderConfig {
                base_url: "http://localhost:8081".to_string(),
                model: "strong-model".to_string(),
                n_predict: 1024,
            })),
            timeout_seconds: 120,
            strong_timeout_seconds: Some(180),
        };

        let metadata = make_provider_run_metadata(&config).unwrap();

        assert_eq!(metadata.cheap.base_url, "http://localhost:8080");
        assert_eq!(metadata.cheap.model, "cheap-model");
        assert_eq!(metadata.cheap.n_predict, 512);
        assert_eq!(metadata.cheap.timeout_seconds, 120);
        assert_eq!(metadata.strong.base_url, "http://localhost:8081");
        assert_eq!(metadata.strong.model, "strong-model");
        assert_eq!(metadata.strong.n_predict, 1024);
        assert_eq!(metadata.strong.timeout_seconds, 180);
    }

    #[test]
    fn provider_run_metadata_falls_back_for_strong_tier() {
        let config = unmanaged_provider("http://localhost:8080", "cheap-model", 512);

        let metadata = make_provider_run_metadata(&config).unwrap();

        assert_eq!(metadata.strong.base_url, metadata.cheap.base_url);
        assert_eq!(metadata.strong.model, metadata.cheap.model);
        assert_eq!(metadata.strong.n_predict, metadata.cheap.n_predict);
        assert_eq!(
            metadata.strong.timeout_seconds,
            metadata.cheap.timeout_seconds
        );
    }

    #[test]
    fn provider_run_metadata_records_managed_llama_cpp_server() {
        let config = ProviderConfig {
            cheap: managed_tier(
                "llama-server",
                "models/cheap.gguf",
                "127.0.0.1",
                18080,
                512,
                Some(8192),
                45,
            ),
            strong: None,
            timeout_seconds: 120,
            strong_timeout_seconds: None,
        };

        let metadata = make_provider_run_metadata(&config).unwrap();

        assert_eq!(metadata.cheap.base_url, "http://127.0.0.1:18080");
        assert_eq!(metadata.cheap.model, "models/cheap.gguf");
        assert!(metadata.cheap.managed);
        let server = metadata
            .cheap
            .managed_server
            .expect("managed metadata must be present");
        assert_eq!(server.kind, "llama_cpp");
        assert_eq!(server.command, "llama-server");
        assert_eq!(server.port, 18080);
        assert_eq!(server.context_size, Some(8192));
        assert_eq!(server.startup_timeout_seconds, 45);
        assert!(
            metadata.strong.managed,
            "strong tier should record the shared managed server when it falls back to cheap"
        );
        assert_eq!(metadata.strong.base_url, metadata.cheap.base_url);
    }

    #[test]
    fn provider_run_metadata_records_separate_strong_managed_server() {
        let config = ProviderConfig {
            cheap: ProviderTierConfig::Unmanaged(UnmanagedProviderConfig {
                base_url: "http://localhost:8080".to_string(),
                model: "models/cheap.gguf".to_string(),
                n_predict: 512,
            }),
            strong: Some(managed_tier(
                "/opt/llama-server",
                "models/strong.gguf",
                "127.0.0.1",
                28080,
                1024,
                None,
                60,
            )),
            timeout_seconds: 120,
            strong_timeout_seconds: Some(180),
        };

        let metadata = make_provider_run_metadata(&config).unwrap();

        assert!(!metadata.cheap.managed);
        assert_eq!(metadata.cheap.base_url, "http://localhost:8080");
        assert!(metadata.strong.managed);
        assert_eq!(metadata.strong.base_url, "http://127.0.0.1:28080");
        assert_eq!(metadata.strong.model, "models/strong.gguf");
    }

    #[test]
    fn failed_runtime_run_returns_error_or_nonzero_status() {
        let output = SchedulerOutput::Failed {
            graph: empty_graph(),
            reason: FailureReason::ProtocolViolation("something went wrong".to_string()),
        };
        let result = runtime_result_from_scheduler_output(output);
        assert!(result.is_err(), "Failed output must produce an error");
        assert!(
            result.unwrap_err().to_string().contains("run failed"),
            "error message must mention run failed"
        );
    }

    #[test]
    fn runtime_error_includes_provider_failure_reason() {
        let output = SchedulerOutput::Failed {
            graph: empty_graph(),
            reason: FailureReason::TerminalRecovery {
                terminal_message: "deliberation failed".to_string(),
                failure_message: "provider error (Retryable): connection refused".to_string(),
            },
        };
        let result = runtime_result_from_scheduler_output(output);
        let err = result.expect_err("failed output must become an error");
        let message = err.to_string();
        assert!(message.contains("run failed"));
        assert!(message.contains("provider error (Retryable): connection refused"));
    }

    #[test]
    fn successful_runtime_run_still_returns_ok() {
        let output = SchedulerOutput::Complete {
            graph: empty_graph(),
            recovery_summary: RecoverySummary {
                recovered: false,
                retry_count: 0,
                elevate_count: 0,
                split_count: 0,
            },
        };
        let result = runtime_result_from_scheduler_output(output);
        assert!(result.is_ok(), "Complete output must return Ok");
    }

    #[test]
    fn load_or_create_artifact_creates_missing_repo() {
        let path = temp_path("create-missing");
        let _ = std::fs::remove_dir_all(&path);

        let config = artifact_config(&path);
        let result = load_or_create_artifact(&config, None);

        assert!(result.is_ok(), "expected artifact creation to succeed");
        assert!(path.exists(), "bare repo directory must be created");

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn load_or_create_artifact_sets_branch() {
        let path = temp_path("branch");
        let _ = std::fs::remove_dir_all(&path);

        let config = artifact_config(&path);
        let artifact = load_or_create_artifact(&config, None).unwrap();

        assert_eq!(artifact.branch, "main");

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn load_or_create_artifact_loads_existing_repo() {
        let path = temp_path("load-existing");
        let _ = std::fs::remove_dir_all(&path);

        let config = artifact_config(&path);
        let first = load_or_create_artifact(&config, None).unwrap();
        let second = load_or_create_artifact(&config, None).unwrap();

        assert_eq!(
            first.commit_sha, second.commit_sha,
            "loading twice must yield the same commit"
        );

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn relative_repo_path_canonicalized_and_integrates_from_temp_workspace() {
        use crate::artifacts::{WorkspaceFileOps, create_workspace, integrate};

        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let rel = format!("target/forge-relative-test-{}-{seq}", std::process::id());
        let _ = std::fs::remove_dir_all(&rel);

        let config = ArtifactConfig {
            repo_path: rel.clone(),
            branch: "main".to_string(),
        };

        let artifact = load_or_create_artifact(&config, None).unwrap();

        assert!(
            artifact.repo_path.is_absolute(),
            "repo_path must be canonicalized to absolute"
        );

        let workspace_path =
            std::env::temp_dir().join(format!("forge-rel-workspace-{}-{seq}", std::process::id()));
        let mut workspace = create_workspace(&artifact, workspace_path.clone());

        workspace
            .write_file("result.txt", "from relative repo\n")
            .unwrap();

        let integrated = integrate(&artifact, &workspace).unwrap();

        assert_ne!(
            integrated.commit_sha, artifact.commit_sha,
            "integration from temp workspace must produce a new commit"
        );

        let _ = std::fs::remove_dir_all(&rel);
        let _ = std::fs::remove_dir_all(&workspace_path);
    }

    #[test]
    fn runtime_creates_telemetry_directory() {
        let dir = temp_path("telemetry-dir");
        let _ = std::fs::remove_dir_all(&dir);

        let _sink = FileTelemetry::new(dir.clone());

        assert!(dir.exists(), "telemetry directory must be created");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Creates a bare repo with two branches and HEAD pointing to the non-default one:
    ///   main  -> Commit A  (contains a.txt)
    ///   other -> Commit B  (contains b.txt)
    ///   HEAD  -> other
    ///
    /// Returns (bare_repo_path, sha_on_main, sha_on_other).
    fn make_two_branch_bare_repo(base: &Path) -> (PathBuf, String, String) {
        let seed = base.join("seed");
        std::fs::create_dir_all(&seed).unwrap();

        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&seed)
                    .status()
                    .unwrap()
                    .success(),
                "git {} failed",
                args.join(" ")
            );
        };
        let sha = |args: &[&str]| -> String {
            String::from_utf8(
                Command::new("git")
                    .args(args)
                    .current_dir(&seed)
                    .output()
                    .unwrap()
                    .stdout,
            )
            .unwrap()
            .trim()
            .to_owned()
        };

        git(&["init", "--quiet", "--initial-branch=main"]);
        git(&["config", "user.name", "Forge Test"]);
        git(&["config", "user.email", "forge-test@example.invalid"]);
        std::fs::write(seed.join("a.txt"), "on main\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "--quiet", "-m", "Commit A"]);
        let sha_main = sha(&["rev-parse", "HEAD"]);

        git(&["checkout", "--quiet", "-b", "other"]);
        std::fs::write(seed.join("b.txt"), "on other\n").unwrap();
        git(&["add", "b.txt"]);
        git(&["commit", "--quiet", "-m", "Commit B"]);
        let sha_other = sha(&["rev-parse", "HEAD"]);

        // Clone bare with HEAD -> other (whatever the seed is currently on).
        let bare = base.join("artifact.git");
        assert!(
            Command::new("git")
                .args(["clone", "--quiet", "--bare"])
                .arg(&seed)
                .arg(&bare)
                .status()
                .unwrap()
                .success(),
            "git clone --bare failed"
        );

        (bare, sha_main, sha_other)
    }

    #[test]
    fn load_existing_artifact_uses_configured_branch_not_head() {
        let base = temp_path("branch-not-head");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let (repo_path, sha_main, sha_other) = make_two_branch_bare_repo(&base);
        assert_ne!(sha_main, sha_other, "test requires two distinct commits");

        let config = ArtifactConfig {
            repo_path: repo_path.to_str().unwrap().to_string(),
            branch: "main".to_string(),
        };

        let artifact = load_or_create_artifact(&config, None).unwrap();
        assert_eq!(
            artifact.commit_sha, sha_main,
            "must resolve configured branch (main), not bare repo HEAD (other)"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn runtime_uses_always_pass_when_validation_absent() {
        use crate::artifacts::Workspace;

        let ws = Workspace::at_path(std::env::temp_dir(), "abc".to_string());
        let validator = make_validator(None, None).unwrap();
        let result = validator.validate(&ws);
        assert!(
            result.passed,
            "absent validation config must yield a passing validator"
        );
    }

    #[test]
    fn runtime_uses_command_validator_when_configured() {
        use crate::artifacts::Workspace;
        use crate::config::ValidationConfig;

        let ws = Workspace::at_path(std::env::temp_dir(), "abc".to_string());
        // A failing command proves the CommandValidator is active, not AlwaysPassValidator.
        let config = ValidationConfig {
            commands: vec!["false".to_string()],
            timeout_seconds: None,
        };
        let validator = make_validator(None, Some(&config)).unwrap();
        let result = validator.validate(&ws);
        assert!(
            !result.passed,
            "configured command validator must run commands and fail on non-zero exit"
        );
    }

    #[test]
    fn runtime_language_validator_uses_language_spec_commands() {
        use crate::artifacts::Workspace;

        let ws = Workspace::at_path(std::env::temp_dir(), "abc".to_string());
        // Rust language spec provides validation commands — they won't pass
        // in a non-Rust workspace, but we can verify a CommandValidator is returned
        // by checking it is not the AlwaysPassValidator (which always passes).
        //
        // We use "rust" which provides cargo commands; in a bare temp dir they will
        // fail, confirming a real CommandValidator was wired up.
        let validator = make_validator(Some("rust"), None).unwrap();
        let result = validator.validate(&ws);
        // cargo fmt --check, cargo check, cargo test will all fail in a temp dir
        assert!(
            !result.passed,
            "rust language validator must run cargo commands that fail in a temp dir; got: {}",
            result.summary
        );
    }

    #[test]
    fn runtime_backward_compat_validation_yaml_translates_to_sh_wrapper() {
        use crate::artifacts::Workspace;
        use crate::config::ValidationConfig;

        let ws = Workspace::at_path(std::env::temp_dir(), "abc".to_string());
        // Raw YAML commands are wrapped in sh -c for backward compatibility.
        // A passing shell command confirms the translation works.
        let config = ValidationConfig {
            commands: vec!["true".to_string()],
            timeout_seconds: None,
        };
        let validator = make_validator(None, Some(&config)).unwrap();
        let result = validator.validate(&ws);
        assert!(
            result.passed,
            "sh-wrapped 'true' must pass via backward-compat translation; got: {}",
            result.summary
        );
    }

    #[test]
    fn missing_configured_branch_returns_error() {
        let base = temp_path("missing-branch");
        let _ = std::fs::remove_dir_all(&base);

        // Create a repo whose only branch is "other".
        let config_other = ArtifactConfig {
            repo_path: base.join("artifact.git").to_str().unwrap().to_string(),
            branch: "other".to_string(),
        };
        load_or_create_artifact(&config_other, None).unwrap();

        // Now try to load with branch "main", which does not exist.
        let config_main = ArtifactConfig {
            repo_path: base.join("artifact.git").to_str().unwrap().to_string(),
            branch: "main".to_string(),
        };
        let result = load_or_create_artifact(&config_main, None);
        assert!(
            result.is_err(),
            "must fail when configured branch is absent"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("main"),
            "error must mention the missing branch name, got: {msg}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn runtime_summary_uses_post_integration_artifact_commit() {
        use crate::artifacts::WorkspaceFileOps;
        use crate::machines::scheduler::{
            NodeId, NodeKind, NodeRequest, PlanOutput, RunRequest, SchedulerHandler,
            SchedulerMachine, WorkOutput,
        };
        use crate::node_runner::{NodeRunRequest, NodeRunResult, NodeRunWorkResult, NodeRunner};
        use crate::telemetry::NoopTelemetry;
        use std::fs;
        use std::process::Command;

        // Returns PlanAccepted for Plan nodes and mutates the WorkAttempt
        // workspace for Work nodes, so the full RunNode → IntegrateWork path
        // is exercised and the artifact commit actually advances.
        struct FileWritingRunner;
        impl NodeRunner for FileWritingRunner {
            fn run_node(
                &self,
                request: NodeRunRequest,
                _telemetry: &dyn crate::telemetry::TelemetrySink,
            ) -> NodeRunResult {
                match request.kind {
                    NodeKind::Plan => NodeRunResult::PlanAccepted(PlanOutput {
                        children: vec![NodeRequest {
                            id: NodeId("work".to_string()),
                            kind: NodeKind::Work,
                            objective: "generate result.txt".to_string(),
                            target_files: vec![],
                            required_test_targets: vec![],
                            dependencies: vec![],
                            validation_plan: None,
                        }],
                    }),
                    NodeKind::Work => {
                        request
                            .work_attempt
                            .expect("artifact Work must receive a WorkAttempt")
                            .workspace
                            .borrow_mut()
                            .write_file("result.txt", "generated\n")
                            .expect("test runner must write result.txt");
                        NodeRunResult::WorkAccepted(NodeRunWorkResult {
                            work: WorkOutput {
                                summary: "wrote result.txt".to_string(),
                            },
                        })
                    }
                }
            }
        }

        let base = temp_path("post-integration");
        let _ = fs::remove_dir_all(&base);
        let seed = base.join("seed");
        fs::create_dir_all(&seed).unwrap();

        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&seed)
                    .status()
                    .unwrap()
                    .success(),
                "git {} failed",
                args.join(" ")
            );
        };
        git(&["init", "--quiet", "--initial-branch=main"]);
        git(&["config", "user.name", "Runtime Test"]);
        git(&["config", "user.email", "runtime-test@example.invalid"]);
        fs::write(seed.join("seed.txt"), "initial\n").unwrap();
        git(&["add", "seed.txt"]);
        git(&["commit", "--quiet", "-m", "Initial"]);

        let repo_path = base.join("artifact.git");
        assert!(
            Command::new("git")
                .args(["clone", "--quiet", "--bare"])
                .arg(&seed)
                .arg(&repo_path)
                .status()
                .unwrap()
                .success(),
            "git clone --bare failed"
        );

        let initial_sha = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&repo_path)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();

        let artifact = Artifact {
            repo_path,
            branch: "main".to_string(),
            commit_sha: initial_sha.clone(),
        };

        let handler = SchedulerHandler::with_artifact(FileWritingRunner, artifact);
        let initial_state = SchedulerMachine::initial_state(
            RunRequest {
                objective: "generate a file".to_string(),
            },
            RunConfig::default(),
        );
        let (_output, handler) = run_machine_with_telemetry(handler, initial_state, &NoopTelemetry);

        let final_artifact = handler
            .artifact()
            .expect("artifact must be present after run");
        assert_ne!(
            final_artifact.commit_sha, initial_sha,
            "runtime summary must use the post-integration artifact commit, not the initial one"
        );

        let _ = fs::remove_dir_all(&base);
    }

    // ── validation_passed manifest tests ─────────────────────────────────────

    /// Build a bare-repo artifact and return (base_dir, artifact, initial_sha).
    fn make_bare_artifact(label: &str) -> (PathBuf, Artifact, String) {
        use std::fs;
        use std::process::Command;

        let base = temp_path(label);
        let _ = fs::remove_dir_all(&base);
        let seed = base.join("seed");
        fs::create_dir_all(&seed).unwrap();

        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&seed)
                    .status()
                    .unwrap()
                    .success(),
                "git {} failed",
                args.join(" ")
            );
        };
        git(&["init", "--quiet", "--initial-branch=main"]);
        git(&["config", "user.name", "Runtime Test"]);
        git(&["config", "user.email", "runtime-test@example.invalid"]);
        fs::write(seed.join("seed.txt"), "initial\n").unwrap();
        git(&["add", "seed.txt"]);
        git(&["commit", "--quiet", "-m", "Initial"]);

        let repo_path = base.join("artifact.git");
        assert!(
            Command::new("git")
                .args(["clone", "--quiet", "--bare"])
                .arg(&seed)
                .arg(&repo_path)
                .status()
                .unwrap()
                .success(),
            "git clone --bare failed"
        );

        let initial_sha = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&repo_path)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();

        let artifact = Artifact {
            repo_path,
            branch: "main".to_string(),
            commit_sha: initial_sha.clone(),
        };

        (base, artifact, initial_sha)
    }

    #[test]
    fn successful_validated_run_sets_validation_passed_true() {
        use crate::artifacts::WorkspaceFileOps;
        use crate::machines::scheduler::{
            NodeId, NodeKind, NodeRequest, PlanOutput, RunRequest, SchedulerHandler,
            SchedulerMachine, WorkOutput,
        };
        use crate::node_runner::{NodeRunRequest, NodeRunResult, NodeRunWorkResult, NodeRunner};
        use crate::runtime::{create_run, finalize_manifest};
        use crate::telemetry::NoopTelemetry;

        struct FileWritingRunner;
        impl NodeRunner for FileWritingRunner {
            fn run_node(
                &self,
                request: NodeRunRequest,
                _telemetry: &dyn crate::telemetry::TelemetrySink,
            ) -> NodeRunResult {
                match request.kind {
                    NodeKind::Plan => NodeRunResult::PlanAccepted(PlanOutput {
                        children: vec![NodeRequest {
                            id: NodeId("work".to_string()),
                            kind: NodeKind::Work,
                            objective: "write result.txt".to_string(),
                            target_files: vec![],
                            required_test_targets: vec![],
                            dependencies: vec![],
                            validation_plan: None,
                        }],
                    }),
                    NodeKind::Work => {
                        request
                            .work_attempt
                            .expect("artifact Work must receive a WorkAttempt")
                            .workspace
                            .borrow_mut()
                            .write_file("result.txt", "generated\n")
                            .expect("test runner must write result.txt");
                        NodeRunResult::WorkAccepted(NodeRunWorkResult {
                            work: WorkOutput {
                                summary: "wrote result.txt".to_string(),
                            },
                        })
                    }
                }
            }
        }

        let (base, artifact, _) = make_bare_artifact("vp-manifest-true");
        let runs_root = base.join("runs");

        let run_info = create_run(&runs_root, "test", "repo", &test_provider_metadata()).unwrap();
        let handler = SchedulerHandler::with_artifact(FileWritingRunner, artifact);
        let initial_state = SchedulerMachine::initial_state(
            RunRequest {
                objective: "generate a file".to_string(),
            },
            RunConfig::default(),
        );
        let (output, handler) = run_machine_with_telemetry(handler, initial_state, &NoopTelemetry);

        let validation_passed = handler.validation_passed();
        let status = match &output {
            SchedulerOutput::Complete { .. } => "succeeded",
            SchedulerOutput::Failed { .. } => "failed",
        };
        finalize_manifest(&run_info, status, None, validation_passed, None).unwrap();

        let content = std::fs::read_to_string(run_info.run_dir.join("manifest.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["status"], "succeeded");
        assert_eq!(
            v["validation_passed"],
            serde_json::Value::Bool(true),
            "manifest must record validation_passed=true for a successful validated run"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn validation_failure_sets_validation_passed_false_in_manifest() {
        use crate::artifacts::{Workspace, WorkspaceFileOps};
        use crate::machines::scheduler::{
            NodeId, NodeKind, NodeRequest, PlanOutput, RunRequest, SchedulerHandler,
            SchedulerMachine, WorkOutput,
        };
        use crate::node_runner::{NodeRunRequest, NodeRunResult, NodeRunWorkResult, NodeRunner};
        use crate::runtime::{create_run, finalize_manifest};
        use crate::telemetry::NoopTelemetry;
        use crate::validation::{ValidationResult, Validator};

        struct FileWritingRunner;
        impl NodeRunner for FileWritingRunner {
            fn run_node(
                &self,
                request: NodeRunRequest,
                _telemetry: &dyn crate::telemetry::TelemetrySink,
            ) -> NodeRunResult {
                match request.kind {
                    NodeKind::Plan => NodeRunResult::PlanAccepted(PlanOutput {
                        children: vec![NodeRequest {
                            id: NodeId("work".to_string()),
                            kind: NodeKind::Work,
                            objective: "write result.txt".to_string(),
                            target_files: vec![],
                            required_test_targets: vec![],
                            dependencies: vec![],
                            validation_plan: None,
                        }],
                    }),
                    NodeKind::Work => {
                        request
                            .work_attempt
                            .expect("artifact Work must receive a WorkAttempt")
                            .workspace
                            .borrow_mut()
                            .write_file("result.txt", "generated\n")
                            .expect("test runner must write result.txt");
                        NodeRunResult::WorkAccepted(NodeRunWorkResult {
                            work: WorkOutput {
                                summary: "wrote result.txt".to_string(),
                            },
                        })
                    }
                }
            }
        }

        struct AlwaysFailValidator;
        impl Validator for AlwaysFailValidator {
            fn validate(&self, _workspace: &Workspace) -> ValidationResult {
                ValidationResult {
                    passed: false,
                    summary: "intentional failure".to_string(),
                    failure: None,
                }
            }
        }

        let (base, artifact, _) = make_bare_artifact("vp-manifest-false");
        let runs_root = base.join("runs");

        let run_info = create_run(&runs_root, "test", "repo", &test_provider_metadata()).unwrap();
        let handler = SchedulerHandler::with_artifact(FileWritingRunner, artifact)
            .with_validator(Rc::new(AlwaysFailValidator));
        let initial_state = SchedulerMachine::initial_state(
            RunRequest {
                objective: "generate a file".to_string(),
            },
            RunConfig::default(),
        );
        let (output, handler) = run_machine_with_telemetry(handler, initial_state, &NoopTelemetry);

        let validation_passed = handler.validation_passed();
        let status = match &output {
            SchedulerOutput::Complete { .. } => "succeeded",
            SchedulerOutput::Failed { .. } => "failed",
        };
        finalize_manifest(&run_info, status, None, validation_passed, None).unwrap();

        let content = std::fs::read_to_string(run_info.run_dir.join("manifest.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["status"], "failed");
        assert_eq!(
            v["validation_passed"],
            serde_json::Value::Bool(false),
            "manifest must record validation_passed=false when validator rejects"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn failed_manifest_contains_concrete_failure_reason() {
        use crate::machines::scheduler::{RunRequest, SchedulerHandler, SchedulerMachine};
        use crate::node_runner::{NodeRunRequest, NodeRunResult, NodeRunner};
        use crate::runtime::{create_run, finalize_manifest};
        use crate::telemetry::NoopTelemetry;

        struct AlwaysFailRunner;
        impl NodeRunner for AlwaysFailRunner {
            fn run_node(
                &self,
                _request: NodeRunRequest,
                _telemetry: &dyn crate::telemetry::TelemetrySink,
            ) -> NodeRunResult {
                use crate::machines::scheduler::types::{FailureKind, NodeFailure, RecoveryAction};
                NodeRunResult::Failed(NodeFailure {
                    kind: FailureKind::DeliberationFailure,
                    message: "provider error (Retryable): connection refused".to_string(),
                    recovery: RecoveryAction::Terminal {
                        message: "deliberation failed".to_string(),
                    },
                })
            }
        }

        let (base, artifact, _) = make_bare_artifact("vp-manifest-null");
        let runs_root = base.join("runs");

        let run_info = create_run(&runs_root, "test", "repo", &test_provider_metadata()).unwrap();
        let handler = SchedulerHandler::with_artifact(AlwaysFailRunner, artifact);
        let initial_state = SchedulerMachine::initial_state(
            RunRequest {
                objective: "do something".to_string(),
            },
            RunConfig::default(),
        );
        let (output, handler) = run_machine_with_telemetry(handler, initial_state, &NoopTelemetry);

        let validation_passed = handler.validation_passed();
        let failure_reason_str: Option<String> =
            if let SchedulerOutput::Failed { reason, .. } = &output {
                Some(reason.to_string())
            } else {
                None
            };
        let status = if matches!(output, SchedulerOutput::Failed { .. }) {
            "failed"
        } else {
            "succeeded"
        };
        finalize_manifest(
            &run_info,
            status,
            None,
            validation_passed,
            failure_reason_str.as_deref(),
        )
        .unwrap();

        let content = std::fs::read_to_string(run_info.run_dir.join("manifest.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["status"], "failed");
        assert_eq!(
            v["validation_passed"],
            serde_json::Value::Null,
            "manifest must record validation_passed=null when failure occurs before validation"
        );
        assert!(
            v["failure_reason"]
                .as_str()
                .unwrap()
                .contains("provider error (Retryable): connection refused"),
            "manifest must record the concrete provider failure reason"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    // ── validation_command_is_test_like ──────────────────────────────────────

    #[test]
    fn cargo_test_command_is_test_like() {
        assert!(
            validation_command_is_test_like("cargo test"),
            "'cargo test' must be detected as a test command"
        );
    }

    #[test]
    fn pytest_command_is_test_like() {
        assert!(
            validation_command_is_test_like("uv run pytest"),
            "'uv run pytest' must be detected as a test command"
        );
    }

    #[test]
    fn test_token_alone_is_test_like() {
        assert!(
            validation_command_is_test_like("test"),
            "bare 'test' token must be detected as a test command"
        );
    }

    #[test]
    fn fmt_check_command_is_not_test_like() {
        assert!(
            !validation_command_is_test_like("cargo fmt --check"),
            "'cargo fmt --check' must not be detected as a test command"
        );
    }

    #[test]
    fn lint_command_is_not_test_like() {
        assert!(
            !validation_command_is_test_like("uv run ruff check ."),
            "'uv run ruff check .' must not be detected as a test command"
        );
    }

    // ── project_requires_tests ───────────────────────────────────────────────

    #[test]
    fn project_requires_tests_true_for_validation_config_with_test_command() {
        let config = ValidationConfig {
            commands: vec!["cargo test".to_string()],
            timeout_seconds: None,
        };
        assert!(
            project_requires_tests(None, Some(&config)),
            "ValidationConfig with 'cargo test' must set requires_tests = true"
        );
    }

    #[test]
    fn project_requires_tests_false_for_validation_config_without_test_command() {
        let config = ValidationConfig {
            commands: vec!["cargo fmt --check".to_string()],
            timeout_seconds: None,
        };
        assert!(
            !project_requires_tests(None, Some(&config)),
            "ValidationConfig without test command must set requires_tests = false"
        );
    }

    #[test]
    fn project_requires_tests_false_when_no_validation() {
        assert!(
            !project_requires_tests(None, None),
            "absent validation must set requires_tests = false"
        );
    }

    // ── RunConfig derivation ─────────────────────────────────────────────────

    #[test]
    fn run_config_has_strong_tier_false_when_provider_strong_is_none() {
        use crate::machines::scheduler::{RunRequest, SchedulerMachine};

        let provider = unmanaged_provider("http://localhost:8080", "cheap", 512);
        assert!(
            provider.strong.is_none(),
            "test requires no strong tier configured"
        );

        let run_config = RunConfig {
            has_strong_tier: provider.strong.is_some(),
        };
        let state = SchedulerMachine::initial_state(
            RunRequest {
                objective: "test".to_string(),
            },
            run_config,
        );

        let embedded = match &state {
            SchedulerState::Active { run_config, .. } => run_config,
            _ => panic!("initial_state must return Active"),
        };
        assert!(
            !embedded.has_strong_tier,
            "has_strong_tier must be false when provider.strong is None; \
             RunConfig::default() would give true, causing silent retry on identical model"
        );
    }

    #[test]
    fn run_config_has_strong_tier_true_when_provider_strong_is_some() {
        use crate::machines::scheduler::{RunRequest, SchedulerMachine};

        let provider = ProviderConfig {
            cheap: ProviderTierConfig::Unmanaged(UnmanagedProviderConfig {
                base_url: "http://localhost:8080".to_string(),
                model: "cheap".to_string(),
                n_predict: 512,
            }),
            strong: Some(ProviderTierConfig::Unmanaged(UnmanagedProviderConfig {
                base_url: "http://localhost:8081".to_string(),
                model: "strong".to_string(),
                n_predict: 1024,
            })),
            timeout_seconds: 120,
            strong_timeout_seconds: None,
        };

        let run_config = RunConfig {
            has_strong_tier: provider.strong.is_some(),
        };
        let state = SchedulerMachine::initial_state(
            RunRequest {
                objective: "test".to_string(),
            },
            run_config,
        );

        let embedded = match &state {
            SchedulerState::Active { run_config, .. } => run_config,
            _ => panic!("initial_state must return Active"),
        };
        assert!(
            embedded.has_strong_tier,
            "has_strong_tier must be true when provider.strong is Some"
        );
    }
}
