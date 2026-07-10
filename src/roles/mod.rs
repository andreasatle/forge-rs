//! Role execution layer — sits between [`DeliberationMachine`] and [`ProviderClient`].
//!
//! A [`RoleRunner`] owns one complete role round-trip: render prompt, call the
//! provider, parse the JSON response, and retry on protocol failure. The
//! deliberation layer above sees only [`RoleRequest`] in and [`RoleResult`] out.
//!
//! [`DeliberationMachine`]: crate::machines::deliberation::DeliberationMachine
//! [`ProviderClient`]: crate::providers::ProviderClient
//! [`RoleResult`]: crate::roles::runner::RoleResult

mod parser;
pub mod policy;
mod preview;
mod prompt;
mod protocol_state;
pub mod runner;
pub mod target_view;
mod tooling;

pub use policy::RolePolicy;
pub use preview::render_prompt_preview;
pub use runner::{ProviderRoleRunner, RoleRequest, RoleRunOutput, RoleRunner, RoleToolContext};
pub use target_view::{TargetView, TargetViewKind};
