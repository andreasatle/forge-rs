//! The NodeRunner service boundary.
//!
//! Translates a scheduler [`RunNode`](crate::machines::scheduler::SchedulerEffect) effect
//! into a typed [`NodeRunResult`], which dispatch converts back into a
//! direct [`SchedulerEvent`](crate::machines::scheduler::SchedulerEvent).
//!
//! # Module layout
//!
//! - `types.rs` — [`NodeRunRequest`] and [`NodeRunResult`]
//! - `runner.rs` — [`NodeRunner`] trait and [`StaticNodeRunner`] fake implementation
//! - `deliberating.rs` — [`DeliberatingNodeRunner`] backed by [`DeliberationMachine`](crate::machines::deliberation::DeliberationMachine)
//! - `project_setup.rs` — `ProjectRuntimeSetup`, which wires a project adapter into the pieces a runner needs

pub mod classify;
pub mod deliberating;
pub mod planner;
pub(crate) mod project_setup;
pub mod runner;
pub mod types;

pub use deliberating::DeliberatingNodeRunner;
pub(crate) use project_setup::ProjectRuntimeSetup;
pub use runner::{NodeRunner, StaticNodeRunner};
pub use types::{NodeRunRequest, NodeRunResult, NodeRunWorkResult, WorkAttempt};

/// Shared type for the adapter-provided test-target derivation function.
///
/// Called with the source targets in a plan; returns the test-file paths the
/// project adapter requires. An empty return means no tests are required.
pub(crate) type TestTargetsFn = dyn Fn(&[String]) -> Vec<String> + Send + Sync;

/// Shared type for the language-plugin-provided per-role validation plan
/// lookup.
///
/// Called with a node's assigned worker role (`None` when unassigned) and its
/// target files, which select the matching language plugin by extension;
/// returns the validation plan for that role, falling back to the selected
/// plugin's default plan when the role has no override.
pub(crate) type ValidationPlanForRoleFn =
    dyn Fn(Option<&str>, &[String]) -> Option<crate::validation::ValidationPlan> + Send + Sync;
