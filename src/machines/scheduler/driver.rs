//! Drives the scheduler machine to completion, dispatching `RunNode` effects
//! concurrently.
//!
//! Unlike `DeliberatingMachine` (a single, purely sequential consumer of
//! `engine::run_machine`), a scheduler `Start` tick can emit more than one
//! `RunNode` effect at once (`RunConfig::dispatch_cap`). Those dispatches are
//! independent LLM/tool-call round trips, so this module runs its own
//! driving loop instead of routing through the domain-blind
//! `engine::run_machine`: every `RunNode` effect is spawned on its own
//! scoped thread, and results are funneled back through an `mpsc` channel
//! and fed into `SchedulerMachine::transition` one at a time on the main
//! thread — `transition` itself always stays single-threaded and pure.
//! `IntegrateWork`/`IntegratePlannerTasks` effects still run synchronously
//! on the main thread immediately after the event that produced them, since
//! artifact integration must be serialized against the shared artifact/git
//! state.
//!
//! Dispatch is opportunistic, not wave-gated: as soon as any in-flight node
//! resolves and frees a slot below `dispatch_cap`, `SchedulerState::resuming`
//! reports `Active` and this loop emits a fresh `Start` immediately — it does
//! not wait for the rest of the original dispatch batch to drain first. The
//! `state` variant returned by `SchedulerMachine::transition` is the sole
//! signal for whether to re-scan (`Active`) or block for the next completion
//! (`Waiting`); no separate in-flight counter is kept here.
//!
//! `std::thread::scope` is used (rather than `std::thread::spawn`) so
//! dispatch threads can borrow `&SchedulerHandler<R>` directly: every
//! spawned thread is guaranteed to finish before this module's functions
//! return, so no `'static` bound is required on `R`.

use std::any::Any;
use std::collections::VecDeque;
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc;
use std::thread;

use crate::node_runner::NodeRunner;
use crate::telemetry::{NoopTelemetry, TelemetryEvent, TelemetryRecord, TelemetrySink};

use super::effect::SchedulerEffect;
use super::event::SchedulerEvent;
use super::failure::FailureKind;
use super::handler::SchedulerHandler;
use super::machine::{SchedulerMachine, SchedulerTerminalOutput};
use super::state::SchedulerState;
use super::types::{NodeFailure, RecoveryAction};

const MACHINE_NAME: &str = "SchedulerMachine";

/// Drive a scheduler run to completion, discarding telemetry.
pub fn run_scheduler<R: NodeRunner + Sync>(
    handler: SchedulerHandler<R>,
    state: SchedulerState,
) -> SchedulerTerminalOutput {
    run_scheduler_with_telemetry(handler, state, &NoopTelemetry).0
}

/// Drive a scheduler run to completion, recording telemetry at each step.
///
/// `StateEntered` and `EventReceived` carry no `node_id`/`attempt`: scheduler
/// state is the whole run graph, not a single node. `EffectEmitted` gets
/// both fields when the effect is `RunNode` or `IntegrateWork`.
///
/// `R: Sync` because every in-flight `RunNode` dispatch shares `&handler`
/// (and therefore `&R`) with every other concurrently-running dispatch
/// thread.
pub fn run_scheduler_with_telemetry<R: NodeRunner + Sync>(
    handler: SchedulerHandler<R>,
    mut state: SchedulerState,
    telemetry: &dyn TelemetrySink,
) -> (SchedulerTerminalOutput, SchedulerHandler<R>) {
    telemetry.record(TelemetryRecord::new(
        MACHINE_NAME,
        TelemetryEvent::MachineStarted {
            machine: MACHINE_NAME.to_string(),
        },
    ));

    let mut pending_effects: VecDeque<SchedulerEffect> = VecDeque::new();
    let (tx, rx) = mpsc::channel::<SchedulerEvent>();
    let mut event = SchedulerMachine.start_event();

    let output = thread::scope(|scope| {
        loop {
            telemetry.record(TelemetryRecord::new(
                MACHINE_NAME,
                TelemetryEvent::StateEntered {
                    machine: MACHINE_NAME.to_string(),
                    state: format!("{state:#?}"),
                },
            ));
            telemetry.record(TelemetryRecord::new(
                MACHINE_NAME,
                TelemetryEvent::EventReceived {
                    machine: MACHINE_NAME.to_string(),
                    event: format!("{event:#?}"),
                },
            ));

            let transition = handler.transition(state, event);
            state = transition.state;

            if let Some(output) = SchedulerMachine.output(&state) {
                return output;
            }

            pending_effects.extend(transition.effects);

            event = loop {
                let Some(effect) = pending_effects.pop_front() else {
                    // `Active` means a dispatch slot is free and the machine
                    // wants to re-scan for ready work now — emit `Start`
                    // immediately rather than waiting for every other
                    // in-flight node to drain first, so a freed slot gets
                    // opportunistically back-filled. `Waiting` means the cap
                    // is saturated (or nothing is ready yet): block for the
                    // next completion instead.
                    if matches!(state, SchedulerState::Active { .. }) {
                        break SchedulerMachine.start_event();
                    }
                    break rx.recv().expect(
                        "a spawned node-dispatch thread dropped its sender without a result",
                    );
                };

                record_effect_emitted(telemetry, &effect);

                if matches!(effect, SchedulerEffect::RunNode { .. }) {
                    let tx = tx.clone();
                    let handler_ref = &handler;
                    scope.spawn(move || {
                        let event = dispatch_catching_panics(handler_ref, effect);
                        let _ = tx.send(event);
                    });
                    continue;
                }

                break handler.handle_effect(effect);
            };
        }
    });

    (output, handler)
}

/// Stamps `node_id`/`attempt` onto an `EffectEmitted` record when the effect
/// is a `RunNode` or `IntegrateWork` dispatch.
fn record_effect_emitted(telemetry: &dyn TelemetrySink, effect: &SchedulerEffect) {
    let mut record = TelemetryRecord::new(
        MACHINE_NAME,
        TelemetryEvent::EffectEmitted {
            machine: MACHINE_NAME.to_string(),
            effect: format!("{effect:#?}"),
        },
    );
    if let Some((node_id, attempt)) = effect_node_context(effect) {
        record.node_id = Some(node_id);
        record.attempt = Some(attempt);
    }
    telemetry.record(record);
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
        SchedulerEffect::IntegratePlannerTasks { .. } => None,
    }
}

/// Executes a `RunNode` effect, converting a panic into a `NodeFailed` event
/// instead of unwinding across the dispatch thread — a bug in one node's
/// dispatch must not silently strand the scheduler waiting for a result that
/// will never arrive, nor take the rest of the run down with it.
fn dispatch_catching_panics<R: NodeRunner>(
    handler: &SchedulerHandler<R>,
    effect: SchedulerEffect,
) -> SchedulerEvent {
    let node_id = match &effect {
        SchedulerEffect::RunNode { node_id, .. } => node_id.clone(),
        _ => unreachable!("dispatch_catching_panics is only called for RunNode effects"),
    };
    match panic::catch_unwind(AssertUnwindSafe(|| handler.handle_effect(effect))) {
        Ok(event) => event,
        Err(payload) => {
            let message = format!(
                "node dispatch thread panicked: {}",
                panic_message(payload.as_ref())
            );
            SchedulerEvent::NodeFailed {
                node_id,
                failure: NodeFailure {
                    kind: FailureKind::DispatchPanic,
                    message: message.clone(),
                    recovery: RecoveryAction::Terminal { message },
                },
            }
        }
    }
}

/// Best-effort extraction of a human-readable message from a panic payload.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

#[cfg(test)]
#[path = "driver_tests.rs"]
mod tests;
