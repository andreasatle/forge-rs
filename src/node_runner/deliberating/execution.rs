//! Provider execution for one deliberation-backed node run.

use crate::engine::run_machine_with_telemetry;
use crate::providers::ProviderClient;
use crate::roles::RolePolicy;
use crate::telemetry::TelemetrySink;

use crate::node_runner::types::{NodeRunRequest, NodeRunResult};

use super::machine::DeliberatingMachine;
use super::output::map_output;
use super::request::prepare_deliberation;

pub(crate) fn run_with_provider<P: ProviderClient>(
    provider: &P,
    request: NodeRunRequest,
    max_tokens: u32,
    policy: &RolePolicy,
    requires_tests: bool,
    context_file_names: &[String],
    telemetry: &dyn TelemetrySink,
) -> NodeRunResult {
    let prepared = prepare_deliberation(
        provider,
        &request,
        max_tokens,
        policy,
        requires_tests,
        context_file_names,
    );
    let machine = DeliberatingMachine {
        handler: prepared.handler,
        telemetry,
    };
    let (output, machine) = run_machine_with_telemetry(machine, prepared.initial_state, telemetry);
    let tool_artifact_update = machine.take_artifact_update();
    map_output(output, request.kind, tool_artifact_update, telemetry)
}
