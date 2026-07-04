//! Drives a scheduler run to completion from an already-resolved initial
//! state. Shared by [`super::run::ForgeRuntime::run`] and
//! [`super::run::ForgeRuntime::resume`], which differ only in how they
//! obtain `run_info`, the telemetry sink, the provider stack, and the
//! initial [`SchedulerState`].

use std::error::Error;
use std::path::Path;
use std::rc::Rc;

use crate::config::ForgeConfig;
use crate::machines::scheduler::{SchedulerHandler, SchedulerState, run_scheduler_with_telemetry};
use crate::node_runner::DeliberatingNodeRunner;
use crate::runtime::RunInfo;
use crate::runtime::outcome::RunOutcome;
use crate::runtime::project_setup::ProjectRuntimeSetup;
use crate::runtime::provider_stack::ResolvedProviderStack;
use crate::runtime::repo::load_or_create_artifact;
use crate::telemetry::TelemetrySink;

/// Everything needed to drive a scheduler run once its initial state is
/// known: the config, the run's identity/paths, its telemetry sink, and a
/// resolved provider stack.
pub struct RunSession {
    config: ForgeConfig,
    run_info: RunInfo,
    sink: Rc<dyn TelemetrySink>,
    provider_stack: ResolvedProviderStack,
}

impl RunSession {
    pub fn new(
        config: ForgeConfig,
        run_info: RunInfo,
        sink: Rc<dyn TelemetrySink>,
        provider_stack: ResolvedProviderStack,
    ) -> Self {
        Self {
            config,
            run_info,
            sink,
            provider_stack,
        }
    }

    /// Load the artifact, wire up the node runner and scheduler handler, run
    /// the scheduler to completion, and print/persist the outcome.
    pub fn drive(self, initial_state: SchedulerState) -> Result<(), Box<dyn Error>> {
        let artifact = load_or_create_artifact(
            &self.config.artifact,
            self.config.plugin.as_deref().map(Path::new),
        )?;

        let setup = ProjectRuntimeSetup::build(
            Path::new(&self.config.adapter),
            self.config.plugin.as_deref().map(Path::new),
            self.config.validation.as_ref(),
        )?;
        let runner =
            DeliberatingNodeRunner::new(self.provider_stack.cheap, self.provider_stack.strong)
                .with_cheap_max_tokens(self.provider_stack.cheap_tokens)
                .with_strong_max_tokens(self.provider_stack.strong_tokens)
                .with_role_policy(setup.role_policy)
                .with_required_test_targets_fn(setup.required_test_targets_fn)
                .with_context_file_names(setup.context_file_names)
                .with_validation_plan_for_role_fn(setup.validation_plan_for_role_fn);
        let handler = SchedulerHandler::with_artifact(runner, artifact)
            .with_telemetry(Rc::clone(&self.sink))
            .with_validator(setup.validator)
            .with_checkpoint_dir(self.run_info.run_dir.clone());

        let (output, handler) =
            run_scheduler_with_telemetry(handler, initial_state, self.sink.as_ref());
        let outcome = RunOutcome::from_scheduler_terminal_output(&output);
        outcome.print_progress();

        let final_artifact = handler.artifact();
        let validation_passed = handler.validation_passed();
        outcome.print_summary(&self.config, final_artifact.as_ref(), &self.run_info);

        let final_commit = outcome.final_commit(final_artifact.as_ref());
        if let Err(e) = outcome.finalize_manifest(&self.run_info, final_commit, validation_passed) {
            eprintln!("warning: failed to finalize manifest: {e}");
        }

        outcome.into_result()
    }
}
