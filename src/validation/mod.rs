//! Workspace validation gate between artifact update apply and integration commit.
//!
//! Validation runs after an [`ArtifactUpdate`](crate::artifacts::ArtifactUpdate)
//! has been applied to a mutable workspace but before that workspace is
//! committed. A failing validator blocks the commit and returns
//! `IntegrationReturned::Failed` without changing artifact history.

mod plan;
mod validator;

pub use plan::{ValidationPlan, ValidationStage, ValidationStep};
pub use validator::{
    AlwaysPassValidator, CommandSpec, CommandValidator, ValidationCommandFailure, ValidationResult,
    Validator,
};
