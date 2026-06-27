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
use crate::roles::runner::{ProviderRoleRunner, RoleRequest, RoleRunner, RoleToolContext};
use crate::telemetry::{NoopTelemetry, TelemetrySink};

use super::effect::DeliberationEffect;
use super::event::DeliberationEvent;

/// Maximum retry attempts after the first accepted plan violates structured
/// planner validation.
const MAX_PLAN_VALIDATION_RETRIES: usize = 2;

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

/// Compatibility alias: a [`DeliberationHandler`] backed by a
/// [`ProviderRoleRunner`].
pub type ProviderBackedDeliberationHandler<P> = DeliberationHandler<ProviderRoleRunner<P>>;

impl<P> DeliberationHandler<ProviderRoleRunner<P>> {
    /// Wrap a provider in a handler with no file tool context.
    /// Defaults to `NodeKind::Work` for policy selection.
    pub fn new(provider: P) -> Self {
        Self {
            runner: ProviderRoleRunner::new(provider),
            artifact_view: None,
            node_kind: NodeKind::Work,
            accumulated_update: RefCell::new(Vec::new()),
            plan_validation_context: None,
        }
    }

    /// Wrap a provider in a handler with an optional artifact view, an
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
        Self {
            runner: ProviderRoleRunner::new_with_max_tokens(provider, max_tokens)
                .with_policy(policy),
            artifact_view,
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
                        role,
                        objective,
                        producer_content,
                        critic_content,
                        feedback,
                        telemetry,
                    );
                }

                let request = RoleRequest {
                    role: role.clone(),
                    objective,
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
        role: DeliberationRole,
        objective: String,
        producer_content: Option<String>,
        critic_content: Option<String>,
        initial_feedback: Vec<RevisionFeedback>,
        telemetry: &dyn TelemetrySink,
    ) -> DeliberationEvent {
        let context = self
            .plan_validation_context
            .as_ref()
            .expect("plan_validation_context must be Some when this method is called");

        let mut feedback = initial_feedback;

        for attempt in 0..=MAX_PLAN_VALIDATION_RETRIES {
            let request = RoleRequest {
                role: role.clone(),
                objective: objective.clone(),
                producer_content: producer_content.clone(),
                critic_content: critic_content.clone(),
                feedback: feedback.clone(),
                node_kind: self.node_kind.clone(),
                // Plan nodes never get tool context.
                tool_context: None,
            };
            let output = self.runner.run_role(request, telemetry);

            match output.result {
                RoleResult::Accepted { ref content } => {
                    if let Some(planner_out) = parse_planner_content(content) {
                        match validate_plan_output_for_context(&planner_out, context) {
                            Ok(()) => {
                                return DeliberationEvent::RoleReturned {
                                    role,
                                    result: output.result,
                                };
                            }
                            Err(e) => {
                                if attempt >= MAX_PLAN_VALIDATION_RETRIES {
                                    return DeliberationEvent::RoleReturned {
                                        role,
                                        result: RoleResult::Failed {
                                            kind: FailureKind::PlannerValidationFailure,
                                            reason: format!("planner validation failed: {e}"),
                                        },
                                    };
                                }
                                feedback = vec![RevisionFeedback {
                                    reason: planner_validation_feedback(&e),
                                }];
                            }
                        }
                    } else {
                        // Content did not parse as PlannerOutput — pass through as-is;
                        // map_plan_output will handle the failure downstream.
                        return DeliberationEvent::RoleReturned {
                            role,
                            result: output.result,
                        };
                    }
                }
                _ => {
                    // Failed or Rejected — pass through without validation.
                    return DeliberationEvent::RoleReturned {
                        role,
                        result: output.result,
                    };
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::*;
    use crate::engine::{Machine, Transition, run_machine};
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
    use crate::telemetry::TelemetrySink;

    // --- fake RoleRunner for delegation tests ---

    struct ScriptedRoleRunner {
        results: RefCell<VecDeque<RoleResult>>,
        requests: RefCell<Vec<RoleRequest>>,
    }

    impl ScriptedRoleRunner {
        fn new(results: Vec<RoleResult>) -> Self {
            Self {
                results: RefCell::new(results.into()),
                requests: RefCell::new(Vec::new()),
            }
        }
    }

    impl RoleRunner for ScriptedRoleRunner {
        fn run_role(&self, request: RoleRequest, _telemetry: &dyn TelemetrySink) -> RoleRunOutput {
            self.requests.borrow_mut().push(request);
            let result = self
                .results
                .borrow_mut()
                .pop_front()
                .expect("ScriptedRoleRunner: results exhausted");
            RoleRunOutput {
                result,
                artifact_update: None,
            }
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
            producer_content: producer_content.map(|s| s.to_string()),
            critic_content: critic_content.map(|s| s.to_string()),
            feedback,
        }
    }

    fn ready(objective: &str, max_revisions: usize) -> DeliberationState {
        DeliberationState::Ready {
            request: DeliberationRequest {
                objective: objective.to_string(),
                max_revisions,
            },
        }
    }

    // --- delegation test ---

    #[test]
    fn deliberation_handler_delegates_run_role_to_role_runner() {
        let runner = ScriptedRoleRunner::new(vec![RoleResult::Accepted {
            content: "generated".to_string(),
        }]);
        let handler = DeliberationHandler {
            runner,
            artifact_view: None,
            node_kind: NodeKind::Work,
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

    // --- run_machine integration tests ---

    #[test]
    fn run_machine_with_provider_handler_success() {
        let machine = ProvidedMachine {
            handler: ProviderBackedDeliberationHandler::new(ScriptedProvider::from_strs(&[
                r#"{"status":"accepted","content":"draft output"}"#,
                r#"{"status":"accepted","content":"review done"}"#,
                r#"{"status":"accepted","content":"approved"}"#,
            ])),
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
            handler: ProviderBackedDeliberationHandler::new(ScriptedProvider::from_strs(&[
                r#"{"status":"accepted","content":"draft v1"}"#,
                r#"{"status":"accepted","content":"review done"}"#,
                r#"{"status":"rejected","reason":"needs changes"}"#,
                r#"{"status":"accepted","content":"draft v2"}"#,
                r#"{"status":"accepted","content":"review ok"}"#,
                r#"{"status":"accepted","content":"approved"}"#,
            ])),
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
        let runner = ScriptedRoleRunner::new(vec![RoleResult::Accepted {
            content: "work done".to_string(),
        }]);
        let handler = DeliberationHandler {
            runner,
            artifact_view: Some(dummy_view()),
            node_kind: NodeKind::Work,
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
            accumulated_update: RefCell::new(Vec::new()),
            plan_validation_context: Some(PlanValidationContext {
                top_objective: "create foo.rs".to_string(),
                existing_files: vec![],
                requires_tests: false,
            }),
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

    // --- verify NoopTelemetry path still compiles ---

    #[test]
    fn handle_effect_without_telemetry_compiles() {
        let handler = ProviderBackedDeliberationHandler::new(ScriptedProvider::from_strs(&[
            r#"{"status":"accepted","content":"completed"}"#,
        ]));
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
