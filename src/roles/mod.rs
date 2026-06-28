//! Role execution layer — sits between [`DeliberationMachine`] and [`ProviderClient`].
//!
//! A [`RoleRunner`] owns one complete role round-trip: render prompt, call the
//! provider, parse the JSON response, and retry on protocol failure. The
//! deliberation layer above sees only [`RoleRequest`] in and [`RoleResult`] out.
//!
//! [`DeliberationMachine`]: crate::machines::deliberation::DeliberationMachine
//! [`ProviderClient`]: crate::providers::ProviderClient
//! [`RoleResult`]: crate::machines::deliberation::RoleResult

pub mod policy;
pub mod runner;
pub mod target_view;

pub use policy::RolePolicy;
pub use runner::{ProviderRoleRunner, RoleRequest, RoleRunOutput, RoleRunner, RoleToolContext};
pub use target_view::{TargetView, TargetViewKind};
