use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::Arc;

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
use crate::machines::scheduler::{FailureKind, NodeKind};
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

fn handler_with_validation(results: Vec<RoleResult>) -> DeliberationHandler<ScriptedRoleRunner> {
    DeliberationHandler {
        runner: ScriptedRoleRunner::new(results),
        artifact_view: None,
        node_kind: NodeKind::Plan,
        work_requires_artifact_update: false,
        accumulated_update: RefCell::new(Vec::new()),
        plan_validation_context: Some(PlanValidationContext {
            top_objective: "create foo.rs".to_string(),
            existing_files: vec![],
            required_test_targets_fn: Arc::new(|_| vec![]),
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
    let handler =
        ProviderBackedDeliberationHandler::new_non_artifact_work(ScriptedProvider::from_strs(&[
            r#"{"status":"accepted","content":"summary only"}"#,
        ]));
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
