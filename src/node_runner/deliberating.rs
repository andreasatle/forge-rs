//! NodeRunner backed by DeliberationMachine.

use crate::engine::{Machine, Transition, run_machine};
use crate::machines::deliberation::{
    DeliberationEffect, DeliberationEvent, DeliberationMachine, DeliberationRequest,
    DeliberationState, DeliberationTerminalOutput, ProviderBackedDeliberationHandler,
};
use crate::machines::scheduler::{
    NodeFailure, NodeId, NodeKind, NodeRequest, PlanOutput, RecoveryAction, WorkOutput,
};
use crate::providers::ProviderClient;

use super::runner::NodeRunner;
use super::types::{NodeRunRequest, NodeRunResult};

/// Runs a node by driving a [`DeliberationMachine`] with a real provider.
///
/// The final producer content is mapped to [`NodeRunResult`] by kind: plan nodes
/// produce one child work node whose objective is the producer content; work nodes
/// return the producer content as their summary. No JSON interpretation happens
/// here — that boundary belongs to the deliberation role handler.
pub struct DeliberatingNodeRunner<P> {
    provider: P,
}

impl<P> DeliberatingNodeRunner<P> {
    /// Wrap a provider in a new runner.
    pub fn new(provider: P) -> Self {
        Self { provider }
    }
}

struct DeliberatingMachine<'a, P: ProviderClient> {
    handler: ProviderBackedDeliberationHandler<&'a P>,
}

impl<'a, P: ProviderClient> Machine for DeliberatingMachine<'a, P> {
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

impl<P: ProviderClient> NodeRunner for DeliberatingNodeRunner<P> {
    fn run_node(&self, request: NodeRunRequest) -> NodeRunResult {
        let delib_request = DeliberationRequest {
            objective: request.objective.clone(),
            max_revisions: 1,
        };
        let initial_state = DeliberationState::Ready {
            request: delib_request,
        };
        let machine = DeliberatingMachine {
            handler: ProviderBackedDeliberationHandler::new(&self.provider),
        };
        map_output(run_machine(machine, initial_state), request.kind)
    }
}

fn map_output(output: DeliberationTerminalOutput, kind: NodeKind) -> NodeRunResult {
    match output {
        DeliberationTerminalOutput::Complete(out) => match kind {
            NodeKind::Plan => NodeRunResult::PlanAccepted(PlanOutput {
                children: vec![NodeRequest {
                    id: NodeId("child-1".to_string()),
                    kind: NodeKind::Work,
                    objective: out.content,
                    dependencies: vec![],
                }],
            }),
            NodeKind::Work => NodeRunResult::WorkAccepted(WorkOutput {
                summary: out.content,
            }),
        },
        DeliberationTerminalOutput::Failed { reason } => NodeRunResult::Failed(NodeFailure {
            reason,
            recovery: RecoveryAction::Terminal {
                message: "deliberation failed".to_string(),
            },
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::*;
    use crate::machines::scheduler::ModelTier;
    use crate::providers::{ProviderError, ProviderErrorKind, ProviderRequest, ProviderResponse};

    struct ScriptedProvider {
        responses: RefCell<VecDeque<Result<String, ProviderError>>>,
    }

    impl ScriptedProvider {
        fn from_strs(responses: &[&str]) -> Self {
            Self {
                responses: RefCell::new(responses.iter().map(|s| Ok(s.to_string())).collect()),
            }
        }

        fn failing(kind: ProviderErrorKind, message: &str) -> Self {
            Self {
                responses: RefCell::new(VecDeque::from([Err(ProviderError {
                    kind,
                    message: message.to_string(),
                })])),
            }
        }
    }

    impl ProviderClient for ScriptedProvider {
        fn call(&self, _req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
            self.responses
                .borrow_mut()
                .pop_front()
                .expect("ScriptedProvider: responses exhausted")
                .map(|content| ProviderResponse { content })
        }
    }

    fn plan_request(objective: &str) -> NodeRunRequest {
        NodeRunRequest {
            kind: NodeKind::Plan,
            objective: objective.to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        }
    }

    fn work_request(objective: &str) -> NodeRunRequest {
        NodeRunRequest {
            kind: NodeKind::Work,
            objective: objective.to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        }
    }

    #[test]
    fn deliberating_runner_plan_returns_plan_output() {
        let provider = ScriptedProvider::from_strs(&[
            r#"{"status":"accepted","content":"draft"}"#,
            r#"{"status":"accepted","content":"looks good"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(provider);
        let result = runner.run_node(plan_request("plan the work"));
        let NodeRunResult::PlanAccepted(plan) = result else {
            panic!("expected PlanAccepted");
        };
        assert_eq!(plan.children.len(), 1);
        assert_eq!(plan.children[0].kind, NodeKind::Work);
        assert_eq!(plan.children[0].objective, "draft");
    }

    #[test]
    fn deliberating_runner_work_returns_work_output() {
        let provider = ScriptedProvider::from_strs(&[
            r#"{"status":"accepted","content":"finished the task"}"#,
            r#"{"status":"accepted","content":"review ok"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(provider);
        let result = runner.run_node(work_request("write some code"));
        let NodeRunResult::WorkAccepted(work) = result else {
            panic!("expected WorkAccepted");
        };
        assert_eq!(work.summary, "finished the task");
    }

    #[test]
    fn deliberating_runner_provider_failure_returns_failed() {
        let provider = ScriptedProvider::failing(ProviderErrorKind::Retryable, "timeout");
        let runner = DeliberatingNodeRunner::new(provider);
        let result = runner.run_node(work_request("do something"));
        let NodeRunResult::Failed(failure) = result else {
            panic!("expected Failed");
        };
        assert!(matches!(failure.recovery, RecoveryAction::Terminal { .. }));
    }

    #[test]
    fn deliberating_runner_revision_uses_latest_producer_content() {
        let provider = ScriptedProvider::from_strs(&[
            r#"{"status":"accepted","content":"draft v1"}"#,
            r#"{"status":"accepted","content":"review"}"#,
            r#"{"status":"rejected","reason":"needs work"}"#,
            r#"{"status":"accepted","content":"draft v2"}"#,
            r#"{"status":"accepted","content":"review ok"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(provider);
        let result = runner.run_node(work_request("refine the plan"));
        let NodeRunResult::WorkAccepted(work) = result else {
            panic!("expected WorkAccepted");
        };
        assert_eq!(work.summary, "draft v2");
    }

    #[test]
    fn deliberating_runner_preserves_deliberation_failure() {
        let provider = ScriptedProvider::from_strs(&["not valid json at all"]);
        let runner = DeliberatingNodeRunner::new(provider);
        let result = runner.run_node(work_request("do something"));
        let NodeRunResult::Failed(failure) = result else {
            panic!("expected Failed");
        };
        assert!(matches!(failure.recovery, RecoveryAction::Terminal { .. }));
    }
}
