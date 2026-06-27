//! Effect handler for `DeliberationMachine`.
//!
//! `DeliberationHandler` is a thin adapter: it unpacks a `RunRole` effect,
//! delegates to a [`RoleRunner`], and wraps the result back into a
//! `RoleReturned` event. All prompt rendering, provider calls, JSON parsing,
//! protocol retries, and file tool loops live in the runner layer.

use std::cell::RefCell;

use crate::artifacts::{
    ArtifactRead, ArtifactUpdate, ArtifactView, FileChange, StagedArtifactView,
};
use crate::machines::deliberation::event::RoleResult;
use crate::machines::deliberation::state::{DeliberationRole, RevisionFeedback};
use crate::machines::scheduler::{FailureKind, NodeKind};
use crate::node_runner::planner::{
    PlannerValidationError, parse_planner_content, validate_planner_explicit_targets,
    validate_planner_no_recreate, validate_planner_output, validate_planner_tests_required,
};
use crate::roles::policy::RolePolicy;
use crate::roles::runner::{
    ProviderRoleRunner, RoleRequest, RoleRunOutput, RoleRunner, RoleToolContext,
};
use crate::telemetry::{NoopTelemetry, TelemetrySink};

use super::effect::DeliberationEffect;
use super::event::DeliberationEvent;

/// Maximum retry attempts after the first accepted plan violates structured
/// planner validation.
const MAX_PLAN_VALIDATION_RETRIES: usize = 2;

/// Maximum retry attempts after the first accepted work result contains no
/// artifact file changes.
const MAX_WORK_SEMANTIC_VALIDATION_RETRIES: usize = 2;

/// Executes `DeliberationEffect` values by delegating role execution to a
/// [`RoleRunner`].
///
/// Accumulates any [`ArtifactUpdate`] values produced by tool loops across
/// all role invocations. Retrieve the combined update with
/// [`take_artifact_update`](DeliberationHandler::take_artifact_update) after
/// the machine finishes.
pub struct DeliberationHandler<R> {
    runner: R,
    /// Artifact view made available to roles as file tool context.
    artifact_view: Option<ArtifactView>,
    /// Whether this deliberation is for a plan node or a work node.
    /// Forwarded to every Producer RoleRequest to select the correct policy field.
    node_kind: NodeKind,
    /// Whether Work+Producer accepted output must include artifact file changes.
    work_requires_artifact_update: bool,
    /// File changes accumulated across all tool loops run so far.
    accumulated_update: RefCell<Vec<FileChange>>,
    /// For plan nodes: optional structured validation applied to planner
    /// output before the plan is accepted.
    plan_validation_context: Option<PlanValidationContext>,
}

#[derive(Clone)]
struct PlanValidationContext {
    top_objective: String,
    existing_files: Vec<String>,
    requires_tests: bool,
}

struct ProducerSemanticValidationConfig {
    role: DeliberationRole,
    objective: String,
    target_files: Vec<String>,
    producer_content: Option<String>,
    critic_content: Option<String>,
    initial_feedback: Vec<RevisionFeedback>,
    max_retries: usize,
    accumulate_artifact_update_on_pass: bool,
}

enum ProducerSemanticValidationDecision {
    Valid,
    Retry(ValidationRetry),
}

struct ValidationRetry {
    feedback_reason: String,
    failure_kind: FailureKind,
    failure_reason: String,
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
            node_kind: NodeKind::Work,
            work_requires_artifact_update: false,
            accumulated_update: RefCell::new(Vec::new()),
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
            node_kind: NodeKind::Work,
            work_requires_artifact_update: false,
            accumulated_update: RefCell::new(Vec::new()),
            plan_validation_context: None,
        }
    }

    /// Wrap a provider in a handler with an artifact view for Work nodes, an
    /// explicit token budget forwarded to the role runner, the node kind
    /// used to select the matching plan/work system prompt from the policy,
    /// the role policy to inject into the runner, and an optional context used
    /// to reject planner tasks that violate structured plan rules.
    pub fn new_with_view(
        provider: P,
        artifact_view: Option<ArtifactView>,
        max_tokens: u32,
        node_kind: NodeKind,
        policy: RolePolicy,
        plan_validation_context: Option<(String, Vec<String>, bool)>,
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
            work_requires_artifact_update: node_kind == NodeKind::Work,
            node_kind,
            accumulated_update: RefCell::new(Vec::new()),
            plan_validation_context: plan_validation_context.map(
                |(top_objective, existing_files, requires_tests)| PlanValidationContext {
                    top_objective,
                    existing_files,
                    requires_tests,
                },
            ),
        }
    }
}

impl<R: RoleRunner> DeliberationHandler<R> {
    /// Execute one deliberation effect and return the resulting event.
    ///
    /// `ReturnComplete` and `ReturnFailed` are terminal effects: `run_machine`
    /// checks `output()` before dispatching effects, so reaching them here is
    /// a bug in the caller.
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
                target_files,
                producer_content,
                critic_content,
                feedback,
            } => {
                // Plan nodes must not have file tools — suppress tool context entirely.
                // For work nodes: Producer sees the committed base view; Critic and
                // Referee get a StagedArtifactView that layers the Producer's pending
                // writes on top, so they can read files before integration.
                let tool_context = if self.node_kind == NodeKind::Plan {
                    None
                } else {
                    match &self.artifact_view {
                        None => None,
                        Some(base) => {
                            let view: Box<dyn ArtifactRead> = match &role {
                                DeliberationRole::Producer => Box::new(base.clone()),
                                DeliberationRole::Critic | DeliberationRole::Referee => {
                                    let changes = self.accumulated_update.borrow().clone();
                                    let update = ArtifactUpdate { changes };
                                    Box::new(
                                        StagedArtifactView::from_update(base.clone(), &update)
                                            .expect("staged view construction must succeed for a valid accumulated update"),
                                    )
                                }
                            };
                            Some(RoleToolContext {
                                artifact_view: view,
                            })
                        }
                    }
                };

                // For Plan+Producer with structured validation context, run a retry
                // loop that sends feedback when the planner violates hard rules.
                if self.node_kind == NodeKind::Plan
                    && matches!(role, DeliberationRole::Producer)
                    && self.plan_validation_context.is_some()
                {
                    return self.run_plan_producer_with_validation(
                        ProducerSemanticValidationConfig {
                            role,
                            objective,
                            target_files,
                            producer_content,
                            critic_content,
                            initial_feedback: feedback,
                            max_retries: MAX_PLAN_VALIDATION_RETRIES,
                            accumulate_artifact_update_on_pass: false,
                        },
                        telemetry,
                    );
                }

                // For Work+Producer when file tools are available, enforce the
                // semantic invariant that accepted output must include at least
                // one artifact file change before routing to Critic/Referee.
                if self.node_kind == NodeKind::Work
                    && self.work_requires_artifact_update
                    && matches!(role, DeliberationRole::Producer)
                {
                    return self.run_work_producer_with_validation(
                        ProducerSemanticValidationConfig {
                            role,
                            objective,
                            target_files,
                            producer_content,
                            critic_content,
                            initial_feedback: feedback,
                            max_retries: MAX_WORK_SEMANTIC_VALIDATION_RETRIES,
                            accumulate_artifact_update_on_pass: true,
                        },
                        telemetry,
                    );
                }

                let request = RoleRequest {
                    role: role.clone(),
                    objective,
                    target_files,
                    producer_content,
                    critic_content,
                    feedback,
                    node_kind: self.node_kind.clone(),
                    tool_context,
                };
                let output = self.runner.run_role(request, telemetry);
                if let Some(update) = output.artifact_update {
                    self.accumulated_update.borrow_mut().extend(update.changes);
                }
                DeliberationEvent::RoleReturned {
                    role,
                    result: output.result,
                }
            }
            DeliberationEffect::ReturnComplete { .. } => {
                unreachable!(
                    "ReturnComplete is a terminal effect; \
                     run_machine returns before dispatching it"
                )
            }
            DeliberationEffect::ReturnFailed { .. } => {
                unreachable!(
                    "ReturnFailed is a terminal effect; \
                     run_machine returns before dispatching it"
                )
            }
        }
    }

    /// Run a Plan+Producer role invocation with structured validation and retry.
    ///
    /// After each accepted planner output, validates that no task targets an
    /// existing project file not mentioned in the run objective and that
    /// test-required code plans include test targets. On violation, sends
    /// structured revision feedback, up to `MAX_NO_RECREATE_RETRIES` additional
    /// attempts.
    fn run_plan_producer_with_validation(
        &self,
        config: ProducerSemanticValidationConfig,
        telemetry: &dyn TelemetrySink,
    ) -> DeliberationEvent {
        let context = self
            .plan_validation_context
            .as_ref()
            .expect("plan_validation_context must be Some when this method is called");

        self.run_producer_semantic_validation_loop(
            config,
            telemetry,
            || None,
            |output| {
                let RoleResult::Accepted { content } = &output.result else {
                    return ProducerSemanticValidationDecision::Valid;
                };
                let Some(planner_out) = parse_planner_content(content) else {
                    return ProducerSemanticValidationDecision::Retry(ValidationRetry {
                        feedback_reason: planner_parse_failure_feedback(),
                        failure_kind: FailureKind::PlannerValidationFailure,
                        failure_reason:
                            "planner validation failed: content is not valid PlannerOutput JSON"
                                .to_string(),
                    });
                };
                match validate_plan_output_for_context(&planner_out, context) {
                    Ok(()) => ProducerSemanticValidationDecision::Valid,
                    Err(e) => ProducerSemanticValidationDecision::Retry(ValidationRetry {
                        feedback_reason: planner_validation_feedback(&e),
                        failure_kind: FailureKind::PlannerValidationFailure,
                        failure_reason: format!("planner validation failed: {e}"),
                    }),
                }
            },
        )
    }

    /// Run a Work+Producer role invocation with semantic validation and retry.
    ///
    /// After each accepted work output, validates that at least one artifact
    /// file change was produced. On violation, sends revision feedback up to
    /// `MAX_WORK_SEMANTIC_VALIDATION_RETRIES` additional attempts. Critic and
    /// Referee are never invoked until validation succeeds.
    fn run_work_producer_with_validation(
        &self,
        config: ProducerSemanticValidationConfig,
        telemetry: &dyn TelemetrySink,
    ) -> DeliberationEvent {
        self.run_producer_semantic_validation_loop(
            config,
            telemetry,
            || {
                // Recreate tool context each iteration; Producer always sees the
                // committed base view, not any staged changes.
                self.artifact_view.as_ref().map(|base| RoleToolContext {
                    artifact_view: Box::new(base.clone()),
                })
            },
            |output| match validate_work_output(output.artifact_update.as_ref()) {
                Ok(()) => ProducerSemanticValidationDecision::Valid,
                Err(e) => ProducerSemanticValidationDecision::Retry(ValidationRetry {
                    feedback_reason: work_validation_feedback(&e),
                    failure_kind: FailureKind::WorkSemanticValidationFailure,
                    failure_reason: format!("work semantic validation failed: {e}"),
                }),
            },
        )
    }

    fn run_producer_semantic_validation_loop(
        &self,
        config: ProducerSemanticValidationConfig,
        telemetry: &dyn TelemetrySink,
        mut tool_context_for_attempt: impl FnMut() -> Option<RoleToolContext>,
        mut validate: impl FnMut(&RoleRunOutput) -> ProducerSemanticValidationDecision,
    ) -> DeliberationEvent {
        let mut feedback = config.initial_feedback;

        for attempt in 0..=config.max_retries {
            let request = RoleRequest {
                role: config.role.clone(),
                objective: config.objective.clone(),
                target_files: config.target_files.clone(),
                producer_content: config.producer_content.clone(),
                critic_content: config.critic_content.clone(),
                feedback: feedback.clone(),
                node_kind: self.node_kind.clone(),
                tool_context: tool_context_for_attempt(),
            };
            let output = self.runner.run_role(request, telemetry);

            if !matches!(output.result, RoleResult::Accepted { .. }) {
                // Failed or Rejected — pass through without semantic validation.
                return DeliberationEvent::RoleReturned {
                    role: config.role,
                    result: output.result,
                };
            }

            match validate(&output) {
                ProducerSemanticValidationDecision::Valid => {
                    if config.accumulate_artifact_update_on_pass
                        && let Some(update) = output.artifact_update
                    {
                        self.accumulated_update.borrow_mut().extend(update.changes);
                    }
                    return DeliberationEvent::RoleReturned {
                        role: config.role,
                        result: output.result,
                    };
                }
                ProducerSemanticValidationDecision::Retry(retry) => {
                    if attempt >= config.max_retries {
                        return DeliberationEvent::RoleReturned {
                            role: config.role,
                            result: RoleResult::Failed {
                                kind: retry.failure_kind,
                                reason: retry.failure_reason,
                            },
                        };
                    }
                    feedback = vec![RevisionFeedback {
                        reason: retry.feedback_reason,
                    }];
                }
            }
        }
        unreachable!("loop exits via return in all branches")
    }

    /// Returns and clears the artifact update accumulated by tool loops across
    /// all role invocations in this handler. Returns `None` when no tool calls
    /// produced file changes.
    pub fn take_artifact_update(&self) -> Option<ArtifactUpdate> {
        let changes: Vec<FileChange> = self.accumulated_update.borrow_mut().drain(..).collect();
        if changes.is_empty() {
            None
        } else {
            Some(ArtifactUpdate { changes })
        }
    }
}

fn validate_plan_output_for_context(
    planner_out: &crate::node_runner::planner::PlannerOutput,
    context: &PlanValidationContext,
) -> Result<(), crate::node_runner::planner::PlannerValidationError> {
    // Semantic invariants first — Critic/Referee must never see a structurally
    // broken plan (e.g. an empty task list).
    validate_planner_output(planner_out)?;
    validate_planner_explicit_targets(planner_out, &context.top_objective)?;
    validate_planner_no_recreate(planner_out, &context.top_objective, &context.existing_files)?;
    if context.requires_tests {
        validate_planner_tests_required(planner_out)?;
    }
    Ok(())
}

fn planner_validation_feedback(error: &PlannerValidationError) -> String {
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

fn planner_parse_failure_feedback() -> String {
    "Planner output must be valid PlannerOutput JSON with a top-level tasks array. \
     Return only the structured plan JSON, not prose or markdown."
        .to_string()
}

#[derive(Clone, Debug, PartialEq)]
enum WorkSemanticValidationError {
    MissingArtifactUpdate,
    EmptyArtifactUpdate,
}

impl std::fmt::Display for WorkSemanticValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkSemanticValidationError::MissingArtifactUpdate => {
                write!(f, "accepted work did not produce an artifact update")
            }
            WorkSemanticValidationError::EmptyArtifactUpdate => {
                write!(f, "accepted work produced an empty artifact update")
            }
        }
    }
}

fn validate_work_output(
    artifact_update: Option<&ArtifactUpdate>,
) -> Result<(), WorkSemanticValidationError> {
    match artifact_update {
        None => Err(WorkSemanticValidationError::MissingArtifactUpdate),
        Some(update) if update.changes.is_empty() => {
            Err(WorkSemanticValidationError::EmptyArtifactUpdate)
        }
        Some(_) => Ok(()),
    }
}

fn work_validation_feedback(error: &WorkSemanticValidationError) -> String {
    match error {
        WorkSemanticValidationError::MissingArtifactUpdate => {
            "Accepted Work results must modify the artifact. Use a file tool such as write_file, replace_text, or delete_file before returning accepted output.".to_string()
        }
        WorkSemanticValidationError::EmptyArtifactUpdate => {
            "Accepted Work results must include at least one file change. Produce a concrete artifact update before returning accepted output.".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::*;
    use crate::artifacts::{ArtifactUpdate, FileChange};
    use crate::engine::{Machine, Transition, run_machine, run_machine_with_telemetry};
    use crate::machines::deliberation::effect::DeliberationEffect;
    use crate::machines::deliberation::event::{DeliberationEvent, RoleResult};
    use crate::machines::deliberation::machine::DeliberationMachine;
    use crate::machines::deliberation::state::{
        DeliberationRequest, DeliberationRole, DeliberationState, DeliberationTerminalOutput,
        RevisionFeedback,
    };
    use crate::machines::scheduler::NodeKind;
    use crate::providers::types::{ProviderError, ProviderResponse};
    use crate::providers::{ProviderClient, ProviderRequest};
    use crate::roles::runner::{RoleRequest, RoleRunOutput, RoleRunner};
    use crate::telemetry::{NoopTelemetry, TelemetrySink};

    // --- fake RoleRunner for delegation tests ---

    struct ScriptedRoleRunner {
        outputs: RefCell<VecDeque<RoleRunOutput>>,
        requests: RefCell<Vec<RoleRequest>>,
    }

    impl ScriptedRoleRunner {
        fn new(results: Vec<RoleResult>) -> Self {
            Self {
                outputs: RefCell::new(
                    results
                        .into_iter()
                        .map(|result| RoleRunOutput {
                            result,
                            artifact_update: None,
                        })
                        .collect(),
                ),
                requests: RefCell::new(Vec::new()),
            }
        }

        fn with_outputs(outputs: Vec<RoleRunOutput>) -> Self {
            Self {
                outputs: RefCell::new(outputs.into()),
                requests: RefCell::new(Vec::new()),
            }
        }
    }

    impl RoleRunner for ScriptedRoleRunner {
        fn run_role(&self, request: RoleRequest, _telemetry: &dyn TelemetrySink) -> RoleRunOutput {
            self.requests.borrow_mut().push(request);
            self.outputs
                .borrow_mut()
                .pop_front()
                .expect("ScriptedRoleRunner: outputs exhausted")
        }
    }

    // --- ScriptedProvider for run_machine integration tests ---

    struct ScriptedProvider {
        responses: RefCell<VecDeque<String>>,
    }

    impl ScriptedProvider {
        fn from_strs(responses: &[&str]) -> Self {
            Self {
                responses: RefCell::new(responses.iter().map(|s| s.to_string()).collect()),
            }
        }
    }

    impl ProviderClient for ScriptedProvider {
        fn call(&self, _req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
            let content = self
                .responses
                .borrow_mut()
                .pop_front()
                .expect("ScriptedProvider: responses exhausted");
            Ok(ProviderResponse {
                content,
                finish_reason: None,
            })
        }
    }

    // --- Machine wrapper for run_machine tests ---

    struct ProvidedMachine<P: ProviderClient> {
        handler: ProviderBackedDeliberationHandler<P>,
    }

    impl<P: ProviderClient> Machine for ProvidedMachine<P> {
        type State = DeliberationState;
        type Event = DeliberationEvent;
        type Effect = DeliberationEffect;
        type Output = DeliberationTerminalOutput;

        fn start_event(&self) -> DeliberationEvent {
            DeliberationEvent::Start
        }

        fn transition(
            &self,
            state: DeliberationState,
            event: DeliberationEvent,
        ) -> Transition<DeliberationState, DeliberationEffect> {
            DeliberationMachine.transition(state, event)
        }

        fn handle_effect(&self, effect: DeliberationEffect) -> DeliberationEvent {
            self.handler.handle_effect(effect)
        }

        fn output(&self, state: &DeliberationState) -> Option<DeliberationTerminalOutput> {
            DeliberationMachine.output(state)
        }
    }

    // --- helpers ---

    fn run_role_effect(
        role: DeliberationRole,
        objective: &str,
        producer_content: Option<&str>,
        critic_content: Option<&str>,
        feedback: Vec<RevisionFeedback>,
    ) -> DeliberationEffect {
        DeliberationEffect::RunRole {
            role,
            objective: objective.to_string(),
            target_files: vec![],
            producer_content: producer_content.map(|s| s.to_string()),
            critic_content: critic_content.map(|s| s.to_string()),
            feedback,
        }
    }

    fn ready(objective: &str, max_revisions: usize) -> DeliberationState {
        DeliberationState::Ready {
            request: DeliberationRequest {
                objective: objective.to_string(),
                target_files: vec![],
                max_revisions,
            },
        }
    }

    fn role_output(result: RoleResult, artifact_update: Option<ArtifactUpdate>) -> RoleRunOutput {
        RoleRunOutput {
            result,
            artifact_update,
        }
    }

    fn accepted_output(content: &str, artifact_update: Option<ArtifactUpdate>) -> RoleRunOutput {
        role_output(
            RoleResult::Accepted {
                content: content.to_string(),
            },
            artifact_update,
        )
    }

    fn write_update(path: &str) -> ArtifactUpdate {
        ArtifactUpdate {
            changes: vec![FileChange::Write {
                path: path.to_string(),
                content: "changed\n".to_string(),
            }],
        }
    }

    fn empty_update() -> ArtifactUpdate {
        ArtifactUpdate { changes: vec![] }
    }

    // --- delegation test ---

    #[test]
    fn deliberation_handler_delegates_run_role_to_role_runner() {
        let runner = ScriptedRoleRunner::with_outputs(vec![accepted_output(
            "generated",
            Some(write_update("generated.txt")),
        )]);
        let handler = DeliberationHandler {
            runner,
            artifact_view: None,
            node_kind: NodeKind::Work,
            work_requires_artifact_update: false,
            accumulated_update: RefCell::new(Vec::new()),
            plan_validation_context: None,
        };

        let effect = run_role_effect(
            DeliberationRole::Producer,
            "write a poem",
            None,
            None,
            vec![],
        );
        let event = handler.handle_effect(effect);

        assert_eq!(
            handler.runner.requests.borrow().len(),
            1,
            "runner must have been called once"
        );
        let req = &handler.runner.requests.borrow()[0];
        assert_eq!(req.objective, "write a poem");
        assert!(
            matches!(
                event,
                DeliberationEvent::RoleReturned {
                    result: RoleResult::Accepted { ref content },
                    ..
                } if content == "generated"
            ),
            "expected RoleReturned with Accepted result, got {event:?}"
        );
    }

    #[test]
    fn structured_targets_flow_to_worker_role_request() {
        let runner = ScriptedRoleRunner::with_outputs(vec![accepted_output("generated", None)]);
        let handler = DeliberationHandler {
            runner,
            artifact_view: None,
            node_kind: NodeKind::Work,
            work_requires_artifact_update: false,
            accumulated_update: RefCell::new(Vec::new()),
            plan_validation_context: None,
        };

        let event = handler.handle_effect(DeliberationEffect::RunRole {
            role: DeliberationRole::Producer,
            objective: "write the implementation".to_string(),
            target_files: vec!["src/main.rs".to_string()],
            producer_content: None,
            critic_content: None,
            feedback: vec![],
        });

        assert!(matches!(
            event,
            DeliberationEvent::RoleReturned {
                result: RoleResult::Accepted { .. },
                ..
            }
        ));
        let req = &handler.runner.requests.borrow()[0];
        assert_eq!(req.objective, "write the implementation");
        assert_eq!(req.target_files, vec!["src/main.rs".to_string()]);
    }

    // --- run_machine integration tests ---

    #[test]
    fn run_machine_with_provider_handler_success() {
        let machine = ProvidedMachine {
            handler: ProviderBackedDeliberationHandler::new_with_view(
                ScriptedProvider::from_strs(&[
                    r#"{"tool":"write_file","path":"output.txt","content":"draft output\n"}"#,
                    r#"{"status":"accepted","content":"draft output"}"#,
                    r#"{"tool":"read_file","path":"output.txt"}"#,
                    r#"{"status":"accepted","content":"review done"}"#,
                    r#"{"tool":"read_file","path":"output.txt"}"#,
                    r#"{"status":"accepted","content":"approved"}"#,
                ]),
                Some(dummy_view()),
                1024,
                NodeKind::Work,
                crate::roles::policy::RolePolicy::default(),
                None,
            ),
        };
        let output = run_machine(machine, ready("write a poem", 0));
        match output {
            DeliberationTerminalOutput::Complete(out) => {
                assert_eq!(out.content, "draft output");
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn run_machine_with_provider_handler_revision() {
        let machine = ProvidedMachine {
            handler: ProviderBackedDeliberationHandler::new_with_view(
                ScriptedProvider::from_strs(&[
                    r#"{"tool":"write_file","path":"output.txt","content":"draft v1\n"}"#,
                    r#"{"status":"accepted","content":"draft v1"}"#,
                    r#"{"tool":"read_file","path":"output.txt"}"#,
                    r#"{"status":"accepted","content":"review done"}"#,
                    r#"{"tool":"read_file","path":"output.txt"}"#,
                    r#"{"status":"rejected","reason":"needs changes"}"#,
                    r#"{"tool":"write_file","path":"output.txt","content":"draft v2\n"}"#,
                    r#"{"status":"accepted","content":"draft v2"}"#,
                    r#"{"tool":"read_file","path":"output.txt"}"#,
                    r#"{"status":"accepted","content":"review ok"}"#,
                    r#"{"tool":"read_file","path":"output.txt"}"#,
                    r#"{"status":"accepted","content":"approved"}"#,
                ]),
                Some(dummy_view()),
                1024,
                NodeKind::Work,
                crate::roles::policy::RolePolicy::default(),
                None,
            ),
        };
        let output = run_machine(machine, ready("write a poem", 1));
        match output {
            DeliberationTerminalOutput::Complete(out) => {
                assert_eq!(out.content, "draft v2");
            }
            other => panic!("expected Complete with 'draft v2', got {other:?}"),
        }
    }

    // --- Step 1: planner tool suppression ---

    fn dummy_view() -> ArtifactView {
        use std::path::PathBuf;
        ArtifactView {
            repo_path: PathBuf::from("/nonexistent"),
            commit_sha: "deadbeef".to_string(),
        }
    }

    #[test]
    fn planner_handler_passes_no_tool_context_for_plan_nodes() {
        let runner = ScriptedRoleRunner::new(vec![RoleResult::Accepted {
            content: r#"{"tasks":[]}"#.to_string(),
        }]);
        let handler = DeliberationHandler {
            runner,
            artifact_view: Some(dummy_view()),
            node_kind: NodeKind::Plan,
            work_requires_artifact_update: false,
            accumulated_update: RefCell::new(Vec::new()),
            plan_validation_context: None,
        };

        let effect = run_role_effect(
            DeliberationRole::Producer,
            "plan the work",
            None,
            None,
            vec![],
        );
        handler.handle_effect(effect);

        let req = &handler.runner.requests.borrow()[0];
        assert!(
            req.tool_context.is_none(),
            "plan node must have no tool context even when artifact_view is set"
        );
    }

    #[test]
    fn worker_handler_passes_tool_context_when_view_available() {
        let runner = ScriptedRoleRunner::with_outputs(vec![accepted_output(
            "work done",
            Some(write_update("work.txt")),
        )]);
        let handler = DeliberationHandler {
            runner,
            artifact_view: Some(dummy_view()),
            node_kind: NodeKind::Work,
            work_requires_artifact_update: true,
            accumulated_update: RefCell::new(Vec::new()),
            plan_validation_context: None,
        };

        let effect = run_role_effect(
            DeliberationRole::Producer,
            "do the work",
            None,
            None,
            vec![],
        );
        handler.handle_effect(effect);

        let req = &handler.runner.requests.borrow()[0];
        assert!(
            req.tool_context.is_some(),
            "work node must have tool context when artifact_view is set"
        );
    }

    #[test]
    fn planner_handler_no_tool_context_without_view() {
        let runner = ScriptedRoleRunner::new(vec![RoleResult::Accepted {
            content: r#"{"tasks":[]}"#.to_string(),
        }]);
        let handler = DeliberationHandler {
            runner,
            artifact_view: None,
            node_kind: NodeKind::Plan,
            work_requires_artifact_update: false,
            accumulated_update: RefCell::new(Vec::new()),
            plan_validation_context: None,
        };

        let effect = run_role_effect(
            DeliberationRole::Producer,
            "plan the work",
            None,
            None,
            vec![],
        );
        handler.handle_effect(effect);

        let req = &handler.runner.requests.borrow()[0];
        assert!(
            req.tool_context.is_none(),
            "plan node must have no tool context regardless of whether artifact_view is set"
        );
    }

    // --- semantic validation regression tests ---

    const VALID_SINGLE_TASK: &str = r#"{"tasks":[{"id":"t1","objective":"do work","operation":"modify","targets":["foo.rs"],"depends_on":[]}]}"#;
    const EMPTY_PLAN: &str = r#"{"tasks":[]}"#;

    fn handler_with_validation(
        results: Vec<RoleResult>,
    ) -> DeliberationHandler<ScriptedRoleRunner> {
        DeliberationHandler {
            runner: ScriptedRoleRunner::new(results),
            artifact_view: None,
            node_kind: NodeKind::Plan,
            work_requires_artifact_update: false,
            accumulated_update: RefCell::new(Vec::new()),
            plan_validation_context: Some(PlanValidationContext {
                top_objective: "create foo.rs".to_string(),
                existing_files: vec![],
                requires_tests: false,
            }),
        }
    }

    fn handler_with_work_validation(
        outputs: Vec<RoleRunOutput>,
    ) -> DeliberationHandler<ScriptedRoleRunner> {
        DeliberationHandler {
            runner: ScriptedRoleRunner::with_outputs(outputs),
            artifact_view: Some(dummy_view()),
            node_kind: NodeKind::Work,
            work_requires_artifact_update: true,
            accumulated_update: RefCell::new(Vec::new()),
            plan_validation_context: None,
        }
    }

    struct ScriptedMachine {
        handler: DeliberationHandler<ScriptedRoleRunner>,
    }

    impl Machine for ScriptedMachine {
        type State = DeliberationState;
        type Event = DeliberationEvent;
        type Effect = DeliberationEffect;
        type Output = DeliberationTerminalOutput;

        fn start_event(&self) -> DeliberationEvent {
            DeliberationEvent::Start
        }

        fn transition(
            &self,
            state: DeliberationState,
            event: DeliberationEvent,
        ) -> Transition<DeliberationState, DeliberationEffect> {
            DeliberationMachine.transition(state, event)
        }

        fn handle_effect(&self, effect: DeliberationEffect) -> DeliberationEvent {
            self.handler.handle_effect(effect)
        }

        fn output(&self, state: &DeliberationState) -> Option<DeliberationTerminalOutput> {
            DeliberationMachine.output(state)
        }
    }

    #[test]
    fn valid_single_task_plan_passes_semantic_validation() {
        let handler = handler_with_validation(vec![RoleResult::Accepted {
            content: VALID_SINGLE_TASK.to_string(),
        }]);
        let effect = run_role_effect(
            DeliberationRole::Producer,
            "create foo.rs",
            None,
            None,
            vec![],
        );
        let event = handler.handle_effect(effect);
        assert_eq!(
            handler.runner.requests.borrow().len(),
            1,
            "valid plan must not trigger retry"
        );
        assert!(
            matches!(
                event,
                DeliberationEvent::RoleReturned {
                    result: RoleResult::Accepted { .. },
                    ..
                }
            ),
            "valid plan must produce Accepted; got {event:?}"
        );
    }

    #[test]
    fn empty_plan_triggers_revision_feedback() {
        let handler = handler_with_validation(vec![
            RoleResult::Accepted {
                content: EMPTY_PLAN.to_string(),
            },
            RoleResult::Accepted {
                content: VALID_SINGLE_TASK.to_string(),
            },
        ]);
        let effect = run_role_effect(
            DeliberationRole::Producer,
            "create foo.rs",
            None,
            None,
            vec![],
        );
        let event = handler.handle_effect(effect);

        assert_eq!(
            handler.runner.requests.borrow().len(),
            2,
            "empty plan must trigger exactly one retry"
        );
        let second_feedback = &handler.runner.requests.borrow()[1].feedback;
        assert!(
            !second_feedback.is_empty(),
            "retry request must carry revision feedback"
        );
        assert!(
            second_feedback[0].reason.contains("no tasks"),
            "feedback must mention missing tasks; got: {}",
            second_feedback[0].reason
        );
        assert!(
            matches!(
                event,
                DeliberationEvent::RoleReturned {
                    result: RoleResult::Accepted { .. },
                    ..
                }
            ),
            "valid retry must succeed; got {event:?}"
        );
    }

    #[test]
    fn unparseable_plan_triggers_revision_feedback() {
        let handler = handler_with_validation(vec![
            RoleResult::Accepted {
                content: "Just do the work in one step.".to_string(),
            },
            RoleResult::Accepted {
                content: VALID_SINGLE_TASK.to_string(),
            },
        ]);
        let effect = run_role_effect(
            DeliberationRole::Producer,
            "create foo.rs",
            None,
            None,
            vec![],
        );
        let event = handler.handle_effect(effect);

        assert_eq!(
            handler.runner.requests.borrow().len(),
            2,
            "unparseable planner content must trigger a producer retry"
        );
        let second_feedback = &handler.runner.requests.borrow()[1].feedback;
        assert!(
            second_feedback[0].reason.contains("PlannerOutput JSON"),
            "retry feedback must explain the parse requirement; got: {}",
            second_feedback[0].reason
        );
        assert!(
            matches!(
                event,
                DeliberationEvent::RoleReturned {
                    result: RoleResult::Accepted { .. },
                    ..
                }
            ),
            "valid retry must succeed; got {event:?}"
        );
    }

    #[test]
    fn repeated_unparseable_plans_exhaust_retries_before_review() {
        let machine = ScriptedMachine {
            handler: handler_with_validation(vec![
                RoleResult::Accepted {
                    content: "not json 1".to_string(),
                },
                RoleResult::Accepted {
                    content: "not json 2".to_string(),
                },
                RoleResult::Accepted {
                    content: "not json 3".to_string(),
                },
            ]),
        };
        let (output, machine) =
            run_machine_with_telemetry(machine, ready("create foo.rs", 1), &NoopTelemetry);

        assert!(
            matches!(
                output,
                DeliberationTerminalOutput::Failed {
                    kind: FailureKind::PlannerValidationFailure,
                    ..
                }
            ),
            "unparseable planner exhaustion must fail with typed kind; got {output:?}"
        );
        let roles: Vec<_> = machine
            .handler
            .runner
            .requests
            .borrow()
            .iter()
            .map(|request| request.role.clone())
            .collect();
        assert_eq!(
            roles,
            vec![
                DeliberationRole::Producer,
                DeliberationRole::Producer,
                DeliberationRole::Producer,
            ],
            "Critic/Referee must not run until planner content parses"
        );
    }

    #[test]
    fn repeated_empty_plans_exhaust_retries() {
        let handler = handler_with_validation(vec![
            RoleResult::Accepted {
                content: EMPTY_PLAN.to_string(),
            },
            RoleResult::Accepted {
                content: EMPTY_PLAN.to_string(),
            },
            RoleResult::Accepted {
                content: EMPTY_PLAN.to_string(),
            },
        ]);
        let effect = run_role_effect(
            DeliberationRole::Producer,
            "create foo.rs",
            None,
            None,
            vec![],
        );
        let event = handler.handle_effect(effect);

        assert_eq!(
            handler.runner.requests.borrow().len(),
            MAX_PLAN_VALIDATION_RETRIES + 1,
            "must attempt exactly {} producer calls before failing",
            MAX_PLAN_VALIDATION_RETRIES + 1
        );
        assert!(
            matches!(
                event,
                DeliberationEvent::RoleReturned {
                    result: RoleResult::Failed {
                        kind: FailureKind::PlannerValidationFailure,
                        ..
                    },
                    ..
                }
            ),
            "exhausted retries must produce PlannerValidationFailure; got {event:?}"
        );
    }

    #[test]
    fn empty_plan_revision_then_valid_plan_completes() {
        // Full run_machine integration: empty plan → revision → valid plan → Critic → Referee → Complete
        let machine = ScriptedMachine {
            handler: handler_with_validation(vec![
                RoleResult::Accepted {
                    content: EMPTY_PLAN.to_string(),
                },
                RoleResult::Accepted {
                    content: VALID_SINGLE_TASK.to_string(),
                },
                RoleResult::Accepted {
                    content: "looks good".to_string(),
                }, // Critic
                RoleResult::Accepted {
                    content: "approved".to_string(),
                }, // Referee
            ]),
        };
        let output = run_machine(machine, ready("create foo.rs", 1));
        assert!(
            matches!(output, DeliberationTerminalOutput::Complete(_)),
            "run must complete after one revision; got {output:?}"
        );
    }

    #[test]
    fn semantic_validation_failure_ends_run_before_critic_or_referee() {
        // Provide exactly MAX+1 Producer responses and no Critic/Referee responses.
        // If Critic or Referee were called, ScriptedRoleRunner would panic.
        let machine = ScriptedMachine {
            handler: handler_with_validation(vec![
                RoleResult::Accepted {
                    content: EMPTY_PLAN.to_string(),
                },
                RoleResult::Accepted {
                    content: EMPTY_PLAN.to_string(),
                },
                RoleResult::Accepted {
                    content: EMPTY_PLAN.to_string(),
                },
            ]),
        };
        let output = run_machine(machine, ready("create foo.rs", 1));
        assert!(
            matches!(
                output,
                DeliberationTerminalOutput::Failed {
                    kind: FailureKind::PlannerValidationFailure,
                    ..
                }
            ),
            "run must fail with PlannerValidationFailure; got {output:?}"
        );
    }

    #[test]
    fn accepted_work_with_one_file_change_passes_semantic_validation() {
        let handler = handler_with_work_validation(vec![accepted_output(
            "implemented change",
            Some(write_update("src/lib.rs")),
        )]);
        let event = handler.handle_effect(run_role_effect(
            DeliberationRole::Producer,
            "implement the change",
            None,
            None,
            vec![],
        ));

        assert_eq!(
            handler.runner.requests.borrow().len(),
            1,
            "valid work must not trigger retry"
        );
        assert!(
            matches!(
                event,
                DeliberationEvent::RoleReturned {
                    result: RoleResult::Accepted { .. },
                    ..
                }
            ),
            "valid work must produce Accepted; got {event:?}"
        );
        assert_eq!(
            handler
                .take_artifact_update()
                .expect("valid work update must be retained")
                .changes
                .len(),
            1
        );
    }

    #[test]
    fn accepted_work_with_no_artifact_update_triggers_revision_feedback() {
        let handler = handler_with_work_validation(vec![
            accepted_output("summary without changes", None),
            accepted_output("implemented change", Some(write_update("src/lib.rs"))),
        ]);
        let event = handler.handle_effect(run_role_effect(
            DeliberationRole::Producer,
            "implement the change",
            None,
            None,
            vec![],
        ));

        assert_eq!(
            handler.runner.requests.borrow().len(),
            2,
            "missing artifact update must trigger one retry"
        );
        let second_feedback = &handler.runner.requests.borrow()[1].feedback;
        assert!(
            !second_feedback.is_empty(),
            "retry request must carry revision feedback"
        );
        assert!(
            second_feedback[0]
                .reason
                .contains("must modify the artifact"),
            "feedback must explain the semantic invariant; got: {}",
            second_feedback[0].reason
        );
        assert!(
            matches!(
                event,
                DeliberationEvent::RoleReturned {
                    result: RoleResult::Accepted { .. },
                    ..
                }
            ),
            "valid retry must succeed; got {event:?}"
        );
    }

    #[test]
    #[should_panic(expected = "artifact-producing Work handlers require an ArtifactView")]
    fn artifact_work_constructor_requires_artifact_view() {
        let _handler = ProviderBackedDeliberationHandler::new_with_view(
            ScriptedProvider::from_strs(&[]),
            None,
            1024,
            NodeKind::Work,
            crate::roles::policy::RolePolicy::default(),
            None,
        );
    }

    #[test]
    fn explicit_non_artifact_work_does_not_use_artifact_semantic_validation() {
        let handler = ProviderBackedDeliberationHandler::new_non_artifact_work(
            ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"summary only"}"#]),
        );
        let event = handler.handle_effect(run_role_effect(
            DeliberationRole::Producer,
            "summarize something",
            None,
            None,
            vec![],
        ));

        assert!(
            matches!(
                event,
                DeliberationEvent::RoleReturned {
                    result: RoleResult::Accepted { ref content },
                    ..
                } if content == "summary only"
            ),
            "non-artifact work must accept summary-only Producer output; got {event:?}"
        );
        assert!(
            handler.take_artifact_update().is_none(),
            "non-artifact work must not synthesize an artifact update"
        );
    }

    #[test]
    fn repeated_empty_work_exhausts_semantic_validation_retries() {
        let handler = handler_with_work_validation(vec![
            accepted_output("empty work 1", Some(empty_update())),
            accepted_output("empty work 2", Some(empty_update())),
            accepted_output("empty work 3", Some(empty_update())),
        ]);
        let event = handler.handle_effect(run_role_effect(
            DeliberationRole::Producer,
            "implement the change",
            None,
            None,
            vec![],
        ));

        assert_eq!(
            handler.runner.requests.borrow().len(),
            MAX_WORK_SEMANTIC_VALIDATION_RETRIES + 1,
            "must attempt exactly {} producer calls before failing",
            MAX_WORK_SEMANTIC_VALIDATION_RETRIES + 1
        );
        assert!(
            matches!(
                event,
                DeliberationEvent::RoleReturned {
                    result: RoleResult::Failed {
                        kind: FailureKind::WorkSemanticValidationFailure,
                        ..
                    },
                    ..
                }
            ),
            "exhausted retries must produce WorkSemanticValidationFailure; got {event:?}"
        );
    }

    #[test]
    fn critic_and_referee_are_not_invoked_while_work_semantic_validation_fails() {
        let machine = ScriptedMachine {
            handler: handler_with_work_validation(vec![
                accepted_output("empty work 1", Some(empty_update())),
                accepted_output("empty work 2", Some(empty_update())),
                accepted_output("empty work 3", Some(empty_update())),
            ]),
        };
        let (output, machine) =
            run_machine_with_telemetry(machine, ready("implement the change", 1), &NoopTelemetry);

        assert!(
            matches!(
                output,
                DeliberationTerminalOutput::Failed {
                    kind: FailureKind::WorkSemanticValidationFailure,
                    ..
                }
            ),
            "semantic validation exhaustion must fail with typed kind; got {output:?}"
        );
        let roles: Vec<_> = machine
            .handler
            .runner
            .requests
            .borrow()
            .iter()
            .map(|request| request.role.clone())
            .collect();
        assert_eq!(
            roles,
            vec![
                DeliberationRole::Producer,
                DeliberationRole::Producer,
                DeliberationRole::Producer,
            ],
            "Critic/Referee must not run while Work semantic validation fails"
        );
    }

    #[test]
    fn valid_revised_work_proceeds_to_critic_and_referee() {
        let machine = ScriptedMachine {
            handler: handler_with_work_validation(vec![
                accepted_output("summary without changes", None),
                accepted_output("implemented change", Some(write_update("src/lib.rs"))),
                accepted_output("review passed", None),
                accepted_output("approved", None),
            ]),
        };
        let (output, machine) =
            run_machine_with_telemetry(machine, ready("implement the change", 1), &NoopTelemetry);

        assert!(
            matches!(output, DeliberationTerminalOutput::Complete(_)),
            "valid revised work must complete after Critic and Referee; got {output:?}"
        );
        let roles: Vec<_> = machine
            .handler
            .runner
            .requests
            .borrow()
            .iter()
            .map(|request| request.role.clone())
            .collect();
        assert_eq!(
            roles,
            vec![
                DeliberationRole::Producer,
                DeliberationRole::Producer,
                DeliberationRole::Critic,
                DeliberationRole::Referee,
            ],
            "valid revised work must proceed normally through review roles"
        );
    }

    // --- staged read view: reviewer visibility of producer writes ---

    /// A [`RoleRunner`] variant that can return per-invocation artifact updates.
    struct ScriptedRoleRunnerWithUpdates {
        responses: RefCell<VecDeque<(RoleResult, Option<ArtifactUpdate>)>>,
        requests: RefCell<Vec<RoleRequest>>,
    }

    impl ScriptedRoleRunnerWithUpdates {
        fn new(responses: Vec<(RoleResult, Option<ArtifactUpdate>)>) -> Self {
            Self {
                responses: RefCell::new(responses.into()),
                requests: RefCell::new(Vec::new()),
            }
        }
    }

    impl RoleRunner for ScriptedRoleRunnerWithUpdates {
        fn run_role(&self, request: RoleRequest, _telemetry: &dyn TelemetrySink) -> RoleRunOutput {
            self.requests.borrow_mut().push(request);
            let (result, artifact_update) = self
                .responses
                .borrow_mut()
                .pop_front()
                .expect("ScriptedRoleRunnerWithUpdates: responses exhausted");
            RoleRunOutput {
                result,
                artifact_update,
            }
        }
    }

    fn staged_handler(
        responses: Vec<(RoleResult, Option<ArtifactUpdate>)>,
    ) -> DeliberationHandler<ScriptedRoleRunnerWithUpdates> {
        DeliberationHandler {
            runner: ScriptedRoleRunnerWithUpdates::new(responses),
            artifact_view: Some(dummy_view()),
            node_kind: NodeKind::Work,
            work_requires_artifact_update: true,
            accumulated_update: RefCell::new(Vec::new()),
            plan_validation_context: None,
        }
    }

    #[test]
    fn critic_sees_producer_staged_write_before_integration() {
        let producer_update = ArtifactUpdate {
            changes: vec![FileChange::Write {
                path: "main.py".to_owned(),
                content: "def main():\n    pass\n".to_owned(),
            }],
        };
        let handler = staged_handler(vec![
            (
                RoleResult::Accepted {
                    content: "wrote main.py".to_owned(),
                },
                Some(producer_update),
            ),
            (
                RoleResult::Accepted {
                    content: "looks good".to_owned(),
                },
                None,
            ),
        ]);

        handler.handle_effect(run_role_effect(
            DeliberationRole::Producer,
            "create main.py",
            None,
            None,
            vec![],
        ));
        handler.handle_effect(run_role_effect(
            DeliberationRole::Critic,
            "create main.py",
            Some("wrote main.py"),
            None,
            vec![],
        ));

        let requests = handler.runner.requests.borrow();
        let critic_ctx = requests[1]
            .tool_context
            .as_ref()
            .expect("Critic must receive tool context");
        assert_eq!(
            critic_ctx.artifact_view.read_file("main.py").unwrap(),
            "def main():\n    pass\n",
            "Critic must see the Producer's staged write for main.py"
        );
    }

    #[test]
    fn referee_sees_producer_staged_write_before_integration() {
        let producer_update = ArtifactUpdate {
            changes: vec![FileChange::Write {
                path: "main.py".to_owned(),
                content: "def main():\n    pass\n".to_owned(),
            }],
        };
        let handler = staged_handler(vec![
            (
                RoleResult::Accepted {
                    content: "wrote main.py".to_owned(),
                },
                Some(producer_update),
            ),
            (
                RoleResult::Accepted {
                    content: "looks good".to_owned(),
                },
                None,
            ),
            (
                RoleResult::Accepted {
                    content: "approved".to_owned(),
                },
                None,
            ),
        ]);

        handler.handle_effect(run_role_effect(
            DeliberationRole::Producer,
            "create main.py",
            None,
            None,
            vec![],
        ));
        handler.handle_effect(run_role_effect(
            DeliberationRole::Critic,
            "create main.py",
            Some("wrote main.py"),
            None,
            vec![],
        ));
        handler.handle_effect(run_role_effect(
            DeliberationRole::Referee,
            "create main.py",
            Some("wrote main.py"),
            Some("looks good"),
            vec![],
        ));

        let requests = handler.runner.requests.borrow();
        let referee_ctx = requests[2]
            .tool_context
            .as_ref()
            .expect("Referee must receive tool context");
        assert_eq!(
            referee_ctx.artifact_view.read_file("main.py").unwrap(),
            "def main():\n    pass\n",
            "Referee must see the Producer's staged write for main.py"
        );
    }

    #[test]
    fn reviewer_staged_view_does_not_see_new_file_without_producer_write() {
        // Sanity check: without a Producer write, a new file is not visible.
        let handler = staged_handler(vec![(
            RoleResult::Accepted {
                content: "ok".to_owned(),
            },
            None,
        )]);

        handler.handle_effect(run_role_effect(
            DeliberationRole::Critic,
            "create main.py",
            Some("done"),
            None,
            vec![],
        ));

        let requests = handler.runner.requests.borrow();
        let critic_ctx = requests[0]
            .tool_context
            .as_ref()
            .expect("Critic must receive tool context");
        assert!(
            critic_ctx.artifact_view.read_file("main.py").is_err(),
            "Critic must not see main.py when Producer did not write it"
        );
    }

    // --- verify NoopTelemetry path still compiles ---

    #[test]
    fn handle_effect_without_telemetry_compiles() {
        let handler = ProviderBackedDeliberationHandler::new_with_view(
            ScriptedProvider::from_strs(&[
                r#"{"tool":"write_file","path":"output.txt","content":"completed\n"}"#,
                r#"{"status":"accepted","content":"completed"}"#,
            ]),
            Some(dummy_view()),
            1024,
            NodeKind::Work,
            crate::roles::policy::RolePolicy::default(),
            None,
        );
        let event = handler.handle_effect(run_role_effect(
            DeliberationRole::Producer,
            "test",
            None,
            None,
            vec![],
        ));
        assert!(matches!(
            event,
            DeliberationEvent::RoleReturned {
                result: RoleResult::Accepted { .. },
                ..
            }
        ));
    }
}
