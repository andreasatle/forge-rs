use crate::roles::runner::RoleRunner;
use crate::telemetry::NoopTelemetry;

use super::effect::DeliberationEffect;
use super::event::DeliberationEvent;
use super::handler::DeliberationHandler;

impl<R: RoleRunner> DeliberationHandler<R> {
    /// Execute one deliberation effect and return the resulting event.
    ///
    /// `ReturnComplete` and `ReturnFailed` are terminal effects: `run_machine`
    /// checks `output()` before dispatching effects, so reaching them here is
    /// a bug in the caller.
    pub fn handle_effect(&self, effect: DeliberationEffect) -> DeliberationEvent {
        self.handle_effect_with_telemetry(effect, &NoopTelemetry)
    }
}
