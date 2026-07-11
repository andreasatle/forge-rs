//! Configuration types for a forge run.

mod team_triggers;
mod types;
pub use types::{
    ArtifactConfig, ForgeConfig, ManagedLlamaCppConfig, ManagedLlamaCppModelConfig,
    ManagedProviderConfig, ProviderBackend, ProviderConfig, ProviderTierConfig, TeamConfig,
    TelemetryConfig, Trigger, UnmanagedProviderConfig, ValidationConfig,
};
