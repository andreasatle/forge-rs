//! Effect handler for `DeliberationMachine`.
//!
//! `DeliberationHandler` is a thin adapter: it unpacks a `RunRole` effect,
//! delegates to a [`RoleRunner`], and wraps the result back into a
//! `RoleReturned` event. All prompt rendering, provider calls, JSON parsing,
//! protocol retries, and file tool loops live in the runner layer.

use std::cell::RefCell;

use crate::artifacts::{ArtifactUpdate, ArtifactView, FileChange};
use crate::machines::scheduler::NodeKind;
use crate::roles::runner::{ProviderRoleRunner, RoleRequest, RoleRunner, RoleToolContext};
use crate::telemetry::{NoopTelemetry, TelemetrySink};

use super::effect::DeliberationEffect;
use super::event::DeliberationEvent;

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
        }
    }

    /// Wrap a provider in a handler with an optional artifact view, an
    /// explicit token budget forwarded to the role runner, and the node kind
    /// used to select `planner_system` vs `worker_system` from the policy.
    pub fn new_with_view(
        provider: P,
        artifact_view: Option<ArtifactView>,
        max_tokens: u32,
        node_kind: NodeKind,
    ) -> Self {
        Self {
            runner: ProviderRoleRunner::new_with_max_tokens(provider, max_tokens),
            artifact_view,
            node_kind,
            accumulated_update: RefCell::new(Vec::new()),
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
                let tool_context = self
                    .artifact_view
                    .clone()
                    .map(|v| RoleToolContext { artifact_view: v });
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
                r#"{"status":"accepted","content":"draft"}"#,
                r#"{"status":"accepted","content":"review"}"#,
                r#"{"status":"accepted","content":"approved"}"#,
            ])),
        };
        let output = run_machine(machine, ready("write a poem", 0));
        match output {
            DeliberationTerminalOutput::Complete(out) => {
                assert_eq!(out.content, "draft");
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn run_machine_with_provider_handler_revision() {
        let machine = ProvidedMachine {
            handler: ProviderBackedDeliberationHandler::new(ScriptedProvider::from_strs(&[
                r#"{"status":"accepted","content":"draft v1"}"#,
                r#"{"status":"accepted","content":"review"}"#,
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

    // --- verify NoopTelemetry path still compiles ---

    #[test]
    fn handle_effect_without_telemetry_compiles() {
        let handler = ProviderBackedDeliberationHandler::new(ScriptedProvider::from_strs(&[
            r#"{"status":"accepted","content":"ok"}"#,
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
