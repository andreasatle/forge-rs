//! Effect handler for `DeliberationMachine`.
//!
//! `DeliberationHandler` executes `DeliberationEffect` values: it unpacks
//! `RunRole` effects, delegates to a [`RoleRunner`],
//! wraps results back into events, and validates producer output before forwarding
//! to the Critic/Referee stages. Workspace context construction, semantic
//! validation, and telemetry recording are all handled here.

use std::sync::Arc;

use crate::artifacts::{ArtifactError, ArtifactRead, ArtifactView};
use crate::machines::scheduler::{FailureKind, NodeKind, TestPlanContext};
use crate::node_runner::TestTargetsFn;
use crate::node_runner::WorkAttempt;
use crate::node_runner::planner::{
    PlannerOutput, PlannerValidationError, parse_planner_content,
    validate_planner_explicit_targets, validate_planner_no_recreate, validate_planner_output,
    validate_planner_tests_required,
};
use crate::roles::TargetView;
use crate::roles::policy::RolePolicy;
use crate::roles::runner::{ProviderRoleRunner, RoleRequest, RoleRunner, RoleToolContext};
use crate::telemetry::{NoopTelemetry, TelemetryEvent, TelemetryRecord, TelemetrySink};

use super::effect::DeliberationEffect;
use super::event::{DeliberationEvent, ProducerValidationRetry};
use super::types::DeliberationRole;
use crate::roles::runner::RoleResult;

/// Maximum retry attempts after the first accepted plan violates structured
/// planner validation.
pub(crate) const MAX_PLAN_VALIDATION_RETRIES: usize = 2;

/// Maximum retry attempts after the first accepted work result contains no
/// artifact file changes.
pub(crate) const MAX_WORK_SEMANTIC_VALIDATION_RETRIES: usize = 2;

/// Maximum bytes per target file to include in the prompt target-state view.
pub(crate) const TARGET_VIEW_BUDGET: usize = 16 * 1024;

/// Structured context used to validate planner output for a Plan node.
#[derive(Clone)]
pub(crate) struct PlanValidationContext {
    pub(crate) top_objective: String,
    pub(crate) existing_files: Vec<String>,
    /// Called with all targets in the plan; returns the test-file paths the
    /// project adapter requires for the source files found in that list.
    /// An empty return means no tests are required for this plan.
    pub(crate) required_test_targets_fn: Arc<TestTargetsFn>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum WorkSemanticValidationError {
    MissingArtifactMutation,
}

impl std::fmt::Display for WorkSemanticValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkSemanticValidationError::MissingArtifactMutation => {
                write!(f, "accepted work did not mutate the WorkAttempt workspace")
            }
        }
    }
}

fn validate_plan_output_for_context(
    planner_out: &PlannerOutput,
    context: &PlanValidationContext,
) -> Result<(), PlannerValidationError> {
    validate_planner_output(planner_out)?;

    let objective_targets: Vec<String> = {
        use crate::node_runner::planner::explicit_objective_targets_pub;
        explicit_objective_targets_pub(&context.top_objective)
    };
    let exempt_targets = (context.required_test_targets_fn)(&objective_targets);

    validate_planner_explicit_targets(planner_out, &context.top_objective, &exempt_targets)?;
    validate_planner_no_recreate(planner_out, &context.top_objective, &context.existing_files)?;
    validate_planner_tests_required(planner_out, context.required_test_targets_fn.as_ref())?;
    Ok(())
}

pub(super) fn planner_validation_feedback(error: &PlannerValidationError) -> String {
    match error {
        PlannerValidationError::EmptyTaskList => error.to_string(),
        PlannerValidationError::DuplicateId(id) => {
            format!("{error}. Assign a unique id to every task; '{id}' appears more than once.")
        }
        PlannerValidationError::EmptyObjective(id) => {
            format!(
                "{error}. Every task must have a non-empty objective. \
                 Add a clear objective to task '{id}'."
            )
        }
        PlannerValidationError::EmptyTargets(id) => {
            format!(
                "{error}. Every task must declare at least one concrete target file. \
                 Add a target to task '{id}'."
            )
        }
        PlannerValidationError::SelfDependency(id) => {
            format!(
                "{error}. A task cannot depend on itself. \
                 Remove '{id}' from its own depends_on list."
            )
        }
        PlannerValidationError::UnknownDependency { task_id, dep_id } => {
            format!(
                "{error}. Task '{task_id}' depends on '{dep_id}', which does not exist in this \
                 plan. Only reference task ids defined in the same plan."
            )
        }
        PlannerValidationError::ExplicitTargetViolation {
            allowed_targets, ..
        } => {
            format!(
                "The objective explicitly targets {}. Remove all non-test targets except {}.",
                allowed_targets.join(", "),
                allowed_targets.join(", ")
            )
        }
        PlannerValidationError::MissingTestsForCodeChange => {
            format!(
                "{error}. Project validation includes a test command, so code changes must include \
                 at least one test-related task and target such as a test file."
            )
        }
        PlannerValidationError::TaskRecreatesExistingFile { .. } => {
            format!(
                "{error}. Remove tasks for existing project files not mentioned in the objective. \
                 Only include tasks for files explicitly named in the run objective."
            )
        }
    }
}

pub(super) fn planner_parse_failure_feedback() -> String {
    "Planner output must be valid PlannerOutput JSON with a top-level tasks array. \
     Return only the structured plan JSON, not prose or markdown."
        .to_string()
}

pub(super) fn validate_work_output(
    artifact_changed: bool,
) -> Result<(), WorkSemanticValidationError> {
    if artifact_changed {
        return Ok(());
    }
    Err(WorkSemanticValidationError::MissingArtifactMutation)
}

pub(super) fn work_validation_feedback(error: &WorkSemanticValidationError) -> String {
    match error {
        WorkSemanticValidationError::MissingArtifactMutation => {
            "Accepted Work results must modify the artifact. Use write_file by default when creating a file or replacing most or all of an existing file. Use replace_text only for small, localized edits after reading the file and providing an exact old string that occurs once; whitespace, indentation, or formatting differences will cause replace_text to fail. If a replace_text attempt could not be validated for a whole-file rewrite, switch to write_file instead of retrying another replace_text.".to_string()
        }
    }
}

/// Executes `DeliberationEffect` values by delegating role execution to a
/// [`RoleRunner`].
///
pub struct DeliberationHandler<R> {
    pub(crate) runner: R,
    /// Artifact view made available to roles as file tool context.
    pub(crate) artifact_view: Option<ArtifactView>,
    /// Live candidate workspace for artifact-producing Work.
    pub(crate) work_attempt: Option<WorkAttempt>,
    /// Whether this deliberation is for a plan node or a work node.
    /// Forwarded to every Producer RoleRequest to select the correct policy field.
    pub(crate) node_kind: NodeKind,
    /// Whether Work+Producer accepted output must mutate the artifact workspace.
    pub(crate) work_requires_artifact_mutation: bool,
    /// Structured test-target planning context forwarded to role prompts.
    pub(crate) test_plan_context: TestPlanContext,
    /// For plan nodes: optional structured validation applied to planner
    /// output before the plan is accepted.
    pub(crate) plan_validation_context: Option<PlanValidationContext>,
}

/// Compatibility alias: a [`DeliberationHandler`] backed by a
/// [`ProviderRoleRunner`].
pub type ProviderBackedDeliberationHandler<P> = DeliberationHandler<ProviderRoleRunner<P>>;

impl<P> DeliberationHandler<ProviderRoleRunner<P>> {
    /// Wrap a provider for explicit non-artifact Work.
    ///
    /// This is intended for demos/tests that want Producer/Critic/Referee
    /// deliberation without file tools. Accepted Work from this handler is a
    /// summary only and does not run artifact semantic validation.
    pub fn new_non_artifact_work(provider: P) -> Self {
        Self {
            runner: ProviderRoleRunner::new(provider),
            artifact_view: None,
            work_attempt: None,
            node_kind: NodeKind::Work,
            work_requires_artifact_mutation: false,
            test_plan_context: TestPlanContext::default(),
            plan_validation_context: None,
        }
    }

    /// Wrap a provider for explicit non-artifact Work with runner options.
    pub fn new_non_artifact_work_with_policy(
        provider: P,
        max_tokens: u32,
        policy: RolePolicy,
    ) -> Self {
        Self {
            runner: ProviderRoleRunner::new_with_max_tokens(provider, max_tokens)
                .with_policy(policy),
            artifact_view: None,
            work_attempt: None,
            node_kind: NodeKind::Work,
            work_requires_artifact_mutation: false,
            test_plan_context: TestPlanContext::default(),
            plan_validation_context: None,
        }
    }

    /// Wrap a provider in a handler with an artifact view for Work nodes, an
    /// explicit token budget forwarded to the role runner, the node kind
    /// used to select the matching plan/work system prompt from the policy,
    /// the role policy to inject into the runner, and an optional context used
    /// to reject planner tasks that violate structured plan rules.
    #[cfg(test)]
    pub(crate) fn new_with_view(
        provider: P,
        artifact_view: Option<ArtifactView>,
        max_tokens: u32,
        node_kind: NodeKind,
        policy: RolePolicy,
        plan_validation_context: Option<PlanValidationContext>,
        test_plan_context: TestPlanContext,
    ) -> Self {
        Self::new_with_work_attempt(
            provider,
            artifact_view,
            max_tokens,
            node_kind,
            policy,
            plan_validation_context,
            test_plan_context,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_work_attempt(
        provider: P,
        artifact_view: Option<ArtifactView>,
        max_tokens: u32,
        node_kind: NodeKind,
        policy: RolePolicy,
        plan_validation_context: Option<PlanValidationContext>,
        test_plan_context: TestPlanContext,
        work_attempt: Option<WorkAttempt>,
    ) -> Self {
        assert!(
            node_kind != NodeKind::Work || artifact_view.is_some(),
            "artifact-producing Work handlers require an ArtifactView; use \
             new_non_artifact_work for explicit summary-only Work"
        );
        Self {
            runner: ProviderRoleRunner::new_with_max_tokens(provider, max_tokens)
                .with_policy(policy),
            artifact_view,
            work_attempt,
            work_requires_artifact_mutation: node_kind == NodeKind::Work,
            test_plan_context,
            node_kind,
            plan_validation_context,
        }
    }
}

impl<R: RoleRunner> DeliberationHandler<R> {
    /// Execute one deliberation effect and return the resulting event.
    ///
    /// Terminal deliberation outcomes are represented by terminal state plus
    /// `output()`, so this only dispatches non-terminal effects.
    pub fn handle_effect(&self, effect: DeliberationEffect) -> DeliberationEvent {
        self.handle_effect_with_telemetry(effect, &NoopTelemetry)
    }

    /// Execute one deliberation effect and record role-layer protocol telemetry.
    pub fn handle_effect_with_telemetry(
        &self,
        effect: DeliberationEffect,
        telemetry: &dyn TelemetrySink,
    ) -> DeliberationEvent {
        match effect {
            DeliberationEffect::RunRole {
                role,
                objective,
                context,
                producer_content,
                critic_content,
                feedback,
            } => {
                let (tool_context, target_views) =
                    match self.role_tool_context_and_target_views(&role, &context.target_files) {
                        Ok(context) => context,
                        Err(error) => {
                            let reason = format!(
                                "WorkAttempt workspace view could not be constructed for \
                                 {role:?}: {error}. Use write_file by default when creating a \
                                 file or replacing most or all of an existing file. Use \
                                 replace_text only for small, localized edits after reading the \
                                 file and providing an exact old string that occurs once; \
                                 whitespace, indentation, or formatting differences will cause \
                                 replace_text to fail."
                            );
                            telemetry.record(TelemetryRecord::new_with_subsource(
                                "DeliberationHandler",
                                format!("{role:?}"),
                                TelemetryEvent::WorkAttemptViewConstructionFailed {
                                    role: format!("{role:?}"),
                                    reason: reason.clone(),
                                },
                            ));
                            return failed_role_event(
                                role,
                                FailureKind::WorkSemanticValidationFailure,
                                reason,
                            );
                        }
                    };

                let request = RoleRequest {
                    role: role.clone(),
                    objective,
                    context,
                    test_plan_context: self.test_plan_context.clone(),
                    target_views,
                    producer_content,
                    critic_content,
                    feedback,
                    node_kind: self.node_kind.clone(),
                    tool_context,
                };
                let output = self.runner.run_role(request, telemetry);
                match (&role, output.result) {
                    (DeliberationRole::Producer, RoleResult::Accepted { content }) => {
                        DeliberationEvent::ProducerAccepted {
                            content,
                            artifact_changed: output.artifact_changed,
                        }
                    }
                    (_, result) => role_result_event(role, result),
                }
            }
            DeliberationEffect::ValidateProducer {
                content,
                artifact_changed,
            } => {
                let result = self.validate_producer_semantics(&content, artifact_changed);
                match result {
                    Ok(()) => DeliberationEvent::ProducerValidationAccepted { content },
                    Err(retry) => DeliberationEvent::ProducerValidationRejected { content, retry },
                }
            }
        }
    }

    pub(crate) fn role_tool_context_and_target_views(
        &self,
        role: &DeliberationRole,
        target_files: &[String],
    ) -> Result<(Option<RoleToolContext>, Vec<TargetView>), ArtifactError> {
        if self.node_kind == NodeKind::Plan {
            return Ok((None, vec![]));
        }

        let Some(base) = &self.artifact_view else {
            return Ok((None, vec![]));
        };

        let view: Box<dyn ArtifactRead> = match &self.work_attempt {
            Some(attempt) => Box::new(attempt.workspace.clone()),
            None => Box::new(base.clone()),
        };

        let target_views =
            crate::project::build_file_text_target_views(&*view, target_files, TARGET_VIEW_BUDGET);

        Ok((
            Some(RoleToolContext {
                artifact_view: view,
                writable_workspace: match role {
                    DeliberationRole::Producer => self
                        .work_attempt
                        .as_ref()
                        .map(|attempt| attempt.workspace.clone()),
                    DeliberationRole::Critic | DeliberationRole::Referee => None,
                },
            }),
            target_views,
        ))
    }

    pub(crate) fn validate_producer_semantics(
        &self,
        content: &str,
        artifact_changed: bool,
    ) -> Result<(), ProducerValidationRetry> {
        if self.node_kind == NodeKind::Plan && self.plan_validation_context.is_some() {
            return self.validate_plan_producer_content(content);
        }

        if self.node_kind == NodeKind::Work && self.work_requires_artifact_mutation {
            return self.validate_work_producer_output(artifact_changed);
        }

        Ok(())
    }

    pub(crate) fn validate_plan_producer_content(
        &self,
        content: &str,
    ) -> Result<(), ProducerValidationRetry> {
        let context = self
            .plan_validation_context
            .as_ref()
            .expect("plan_validation_context must be Some when this method is called");

        let Some(planner_out) = parse_planner_content(content) else {
            return Err(ProducerValidationRetry {
                feedback_reason: planner_parse_failure_feedback(),
                max_retries: MAX_PLAN_VALIDATION_RETRIES,
                failure_kind: FailureKind::PlannerValidationFailure,
                failure_reason:
                    "planner validation failed: content is not valid PlannerOutput JSON".to_string(),
            });
        };

        match validate_plan_output_for_context(&planner_out, context) {
            Ok(()) => Ok(()),
            Err(e) => Err(ProducerValidationRetry {
                feedback_reason: planner_validation_feedback(&e),
                max_retries: MAX_PLAN_VALIDATION_RETRIES,
                failure_kind: FailureKind::PlannerValidationFailure,
                failure_reason: format!("planner validation failed: {e}"),
            }),
        }
    }

    pub(crate) fn validate_work_producer_output(
        &self,
        artifact_changed: bool,
    ) -> Result<(), ProducerValidationRetry> {
        match validate_work_output(artifact_changed) {
            Ok(()) => Ok(()),
            Err(e) => Err(ProducerValidationRetry {
                feedback_reason: work_validation_feedback(&e),
                max_retries: MAX_WORK_SEMANTIC_VALIDATION_RETRIES,
                failure_kind: FailureKind::WorkSemanticValidationFailure,
                failure_reason: format!("work semantic validation failed: {e}"),
            }),
        }
    }
}

fn role_result_event(role: DeliberationRole, result: RoleResult) -> DeliberationEvent {
    match (role, result) {
        (DeliberationRole::Producer, RoleResult::Accepted { content }) => {
            DeliberationEvent::ProducerAccepted {
                content,
                artifact_changed: false,
            }
        }
        (DeliberationRole::Producer, RoleResult::Rejected { reason }) => {
            DeliberationEvent::ProducerRejected { reason }
        }
        (DeliberationRole::Producer, RoleResult::Failed { kind, reason }) => {
            DeliberationEvent::ProducerFailed { kind, reason }
        }
        (DeliberationRole::Critic, RoleResult::Accepted { content }) => {
            DeliberationEvent::CriticAccepted { content }
        }
        (DeliberationRole::Critic, RoleResult::Rejected { reason }) => {
            DeliberationEvent::CriticRejected { reason }
        }
        (DeliberationRole::Critic, RoleResult::Failed { kind, reason }) => {
            DeliberationEvent::CriticFailed { kind, reason }
        }
        (DeliberationRole::Referee, RoleResult::Accepted { content }) => {
            DeliberationEvent::RefereeAccepted { content }
        }
        (DeliberationRole::Referee, RoleResult::Rejected { reason }) => {
            DeliberationEvent::RefereeRejected { reason }
        }
        (DeliberationRole::Referee, RoleResult::Failed { kind, reason }) => {
            DeliberationEvent::RefereeFailed { kind, reason }
        }
    }
}

fn failed_role_event(
    role: DeliberationRole,
    kind: FailureKind,
    reason: String,
) -> DeliberationEvent {
    role_result_event(role, RoleResult::Failed { kind, reason })
}

#[cfg(test)]
#[path = "handler_tests.rs"]
mod handler_tests;
