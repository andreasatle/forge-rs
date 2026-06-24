//! Project-level adapter seam.
//!
//! [`ProjectAdapter`] is the hook through which project-specific configuration
//! is injected into the runtime. The initial seam exposes only role prompt
//! policy; future variants can add export config, validation config, or
//! integration movement without changing the runtime wiring.

pub mod default;

pub use default::DefaultProjectAdapter;

use crate::roles::RolePolicy;

/// Provides project-specific configuration to the Forge runtime.
///
/// Implement this trait to customise the role prompt policy (and, in future,
/// other project-level knobs) without touching the runtime directly.
pub trait ProjectAdapter {
    /// Return the per-role system prompt policy for this project.
    fn role_policy(&self) -> RolePolicy;
}
