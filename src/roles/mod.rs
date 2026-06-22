//! Role execution layer — sits between [`DeliberationMachine`] and [`ProviderClient`].
//!
//! A [`RoleRunner`] owns one complete role round-trip: render prompt, call the
//! provider, parse the JSON response, and retry on protocol failure. The
//! deliberation layer above sees only [`RoleRequest`] in and [`RoleResult`] out.
//!
//! [`DeliberationMachine`]: crate::machines::deliberation::DeliberationMachine
//! [`ProviderClient`]: crate::providers::ProviderClient
//! [`RoleResult`]: crate::machines::deliberation::RoleResult

pub mod runner;

pub use runner::{ProviderRoleRunner, RoleRequest, RoleRunner};
