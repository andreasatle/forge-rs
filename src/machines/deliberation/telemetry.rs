use crate::roles::runner::RoleRunner;
use crate::telemetry::NoopTelemetry;

use super::effect::DeliberationEffect;
use super::event::DeliberationEvent;
use super::handler::DeliberationHandler;

impl<R: RoleRunner> DeliberationHandler<R> {
    /// Execute one deliberation effect and return the resulting event.
    ///
    /// Terminal deliberation outcomes are represented by terminal state plus
    /// `output()`, so this only dispatches non-terminal effects.
    pub fn handle_effect(&self, effect: DeliberationEffect) -> DeliberationEvent {
        self.handle_effect_with_telemetry(effect, &NoopTelemetry)
    }
}
