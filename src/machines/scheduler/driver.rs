//! Bridges the pure [`SchedulerMachine`] and the impure [`SchedulerHandler`]
//! into something [`crate::engine::run_machine`] can drive.
//!
//! Mirrors the role `DeliberatingMachine` plays for the deliberation machine
//! (in `node_runner::deliberating`): neither `SchedulerMachine` nor
//! `SchedulerHandler` implements the generic [`Machine`](crate::engine::Machine)
//! trait itself, so a small owning wrapper composes them for the engine's
//! runner loop.

use std::cell::RefCell;

use crate::engine::{Machine, Transition, run_machine, run_machine_with_telemetry};
use crate::telemetry::{TelemetryEvent, TelemetryRecord, TelemetrySink};

use super::effect::SchedulerEffect;
use super::event::SchedulerEvent;
use super::handler::SchedulerHandler;
use super::machine::{SchedulerMachine, SchedulerTerminalOutput};
use super::state::SchedulerState;
use crate::node_runner::NodeRunner;

struct SchedulerDriver<'a, R> {
    handler: SchedulerHandler<R>,
    /// The effect emitted by the most recent `transition` call, if any.
    ///
    /// Captured here so [`EffectContextTelemetry`] can attach `node_id` and
    /// `attempt` to the `EffectEmitted` record the generic engine loop emits
    /// immediately afterwards, without the domain-blind engine needing to
    /// know about either field.
    pending_effect: &'a RefCell<Option<SchedulerEffect>>,
}

impl<'a, R: NodeRunner> Machine for SchedulerDriver<'a, R> {
    type State = SchedulerState;
    type Event = SchedulerEvent;
    type Effect = SchedulerEffect;
    type Output = SchedulerTerminalOutput;

    fn name(&self) -> String {
        "SchedulerMachine".to_string()
    }

    fn start_event(&self) -> SchedulerEvent {
        SchedulerMachine.start_event()
    }

    fn transition(
        &self,
        state: SchedulerState,
        event: SchedulerEvent,
    ) -> Transition<SchedulerState, SchedulerEffect> {
        let transition = self.handler.transition(state, event);
        *self.pending_effect.borrow_mut() = transition.effects.first().cloned();
        transition
    }

    fn handle_effect(&self, effect: SchedulerEffect) -> SchedulerEvent {
        self.handler.handle_effect(effect)
    }

    fn output(&self, state: &SchedulerState) -> Option<SchedulerTerminalOutput> {
        SchedulerMachine.output(state)
    }
}

/// Drive a scheduler run to completion, discarding telemetry.
pub fn run_scheduler<R: NodeRunner>(
    handler: SchedulerHandler<R>,
    state: SchedulerState,
) -> SchedulerTerminalOutput {
    let pending_effect = RefCell::new(None);
    run_machine(
        SchedulerDriver {
            handler,
            pending_effect: &pending_effect,
        },
        state,
    )
}

/// Drive a scheduler run to completion, recording telemetry at each step.
///
/// `StateEntered` and `EventReceived` carry no `node_id`/`attempt`: scheduler
/// state is the whole run graph, not a single node. `EffectEmitted` gets both
/// fields when the effect is `RunNode` or `IntegrateWork`.
pub fn run_scheduler_with_telemetry<R: NodeRunner>(
    handler: SchedulerHandler<R>,
    state: SchedulerState,
    telemetry: &dyn TelemetrySink,
) -> (SchedulerTerminalOutput, SchedulerHandler<R>) {
    let pending_effect = RefCell::new(None);
    let node_context = EffectContextTelemetry::new(telemetry, &pending_effect);
    let (output, driver) = run_machine_with_telemetry(
        SchedulerDriver {
            handler,
            pending_effect: &pending_effect,
        },
        state,
        &node_context,
    );
    (output, driver.handler)
}

/// Stamps `node_id` and `attempt` onto an `EffectEmitted` record when the
/// effect it describes is a `RunNode` or `IntegrateWork` dispatch.
///
/// Reads the effect captured by [`SchedulerDriver::transition`] just before
/// the generic engine loop records `EffectEmitted`, so the enrichment happens
/// without changing the domain-blind engine itself.
struct EffectContextTelemetry<'a> {
    inner: &'a dyn TelemetrySink,
    pending_effect: &'a RefCell<Option<SchedulerEffect>>,
}

impl<'a> EffectContextTelemetry<'a> {
    fn new(
        inner: &'a dyn TelemetrySink,
        pending_effect: &'a RefCell<Option<SchedulerEffect>>,
    ) -> Self {
        Self {
            inner,
            pending_effect,
        }
    }
}

impl<'a> TelemetrySink for EffectContextTelemetry<'a> {
    fn record(&self, mut record: TelemetryRecord) {
        if matches!(record.event, TelemetryEvent::EffectEmitted { .. })
            && let Some(effect) = self.pending_effect.borrow_mut().take()
            && let Some((node_id, attempt)) = effect_node_context(&effect)
        {
            record.node_id = Some(node_id);
            record.attempt = Some(attempt);
        }
        self.inner.record(record);
    }
}

/// Extracts `(node_id, attempt)` from the effect variants that carry them.
fn effect_node_context(effect: &SchedulerEffect) -> Option<(String, u32)> {
    match effect {
        SchedulerEffect::RunNode {
            node_id, attempt, ..
        } => Some((node_id.0.clone(), *attempt)),
        SchedulerEffect::IntegrateWork {
            node_id, attempt, ..
        } => Some((node_id.0.clone(), *attempt)),
    }
}
