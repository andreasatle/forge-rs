//! Provider execution for one deliberation-backed node run.

use crate::engine::run_machine_with_telemetry;
use crate::providers::ProviderClient;
use crate::roles::RolePolicy;
use crate::telemetry::{TelemetryEvent, TelemetryRecord, TelemetrySink};

use crate::node_runner::types::{NodeRunRequest, NodeRunResult};

use super::context::DeliberationContextConfig;
use super::machine::DeliberatingMachine;
use super::output::map_output;
use super::request::prepare_deliberation;

pub(crate) fn run_with_provider<P: ProviderClient>(
    provider: &P,
    request: NodeRunRequest,
    max_tokens: u32,
    policy: &RolePolicy,
    context_config: &DeliberationContextConfig,
    telemetry: &dyn TelemetrySink,
) -> NodeRunResult {
    let prepared = prepare_deliberation(provider, &request, max_tokens, policy, context_config);
    let node_context =
        NodeContextTelemetry::new(telemetry, request.node_id.0.clone(), request.attempt);
    let machine = DeliberatingMachine {
        handler: prepared.handler,
        telemetry: &node_context,
    };
    let (output, _) = run_machine_with_telemetry(machine, prepared.initial_state, &node_context);
    map_output(output, request, &policy.worker_role_descriptions, telemetry)
}

/// Stamps `node_id` and `attempt` onto `StateEntered`, `EventReceived`, and
/// `EffectEmitted` records produced while driving one deliberation run.
///
/// Every event observed here belongs to the same node run, so the context is
/// constant for the lifetime of this sink rather than extracted per-event.
struct NodeContextTelemetry<'a> {
    inner: &'a dyn TelemetrySink,
    node_id: String,
    attempt: u32,
}

impl<'a> NodeContextTelemetry<'a> {
    fn new(inner: &'a dyn TelemetrySink, node_id: String, attempt: u32) -> Self {
        Self {
            inner,
            node_id,
            attempt,
        }
    }
}

impl<'a> TelemetrySink for NodeContextTelemetry<'a> {
    fn record(&self, mut record: TelemetryRecord) {
        if matches!(
            record.event,
            TelemetryEvent::StateEntered { .. }
                | TelemetryEvent::EventReceived { .. }
                | TelemetryEvent::EffectEmitted { .. }
        ) {
            record.node_id = Some(self.node_id.clone());
            record.attempt = Some(self.attempt);
        }
        self.inner.record(record);
    }
}
