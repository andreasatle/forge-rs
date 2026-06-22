//! Forge runtime — drives a single run from a [`crate::config::ForgeConfig`].

mod run;
pub use run::{ForgeRuntime, load_or_create_artifact};
