//! Resolves a [`ProviderConfig`] into ready-to-use provider instances.
//!
//! Bundles metadata resolution (for run manifests), managed llama.cpp server
//! startup, and provider construction into a single build step so `run` and
//! `resume` share one code path instead of duplicating it.

use std::error::Error;

use crate::config::{
    ManagedLlamaCppModelConfig, ManagedProviderConfig, ProviderConfig, ProviderTierConfig,
};
use crate::providers::{LlamaCppProvider, RetryingProvider};
use crate::runtime::managed_provider::{
    ManagedLlamaCppRuntimeConfig, ManagedProviderServer, resolve_llama_cpp_config,
};
use crate::runtime::{ManagedProviderServerMetadata, ProviderRunMetadata, ProviderTierMetadata};

/// The fully resolved provider stack for a run: metadata for the manifest,
/// live provider handles for the node runner, and any managed server
/// processes kept alive for the lifetime of the stack.
pub struct ResolvedProviderStack {
    pub metadata: ProviderRunMetadata,
    pub cheap: RetryingProvider<LlamaCppProvider>,
    pub strong: RetryingProvider<LlamaCppProvider>,
    pub cheap_tokens: u32,
    pub strong_tokens: u32,
    _servers: Vec<ManagedProviderServer>,
}

impl ResolvedProviderStack {
    /// Resolve `provider` into metadata, start any managed servers it
    /// requires, and build the cheap/strong provider handles.
    pub fn build(provider: &ProviderConfig) -> Result<Self, Box<dyn Error>> {
        let metadata = make_provider_run_metadata(provider)?;
        let servers = start_managed_provider_servers(provider)?;

        let cheap_llama =
            LlamaCppProvider::new(&metadata.cheap.base_url, metadata.cheap.timeout_seconds);
        let cheap = RetryingProvider::new(cheap_llama, 3);

        let strong_llama =
            LlamaCppProvider::new(&metadata.strong.base_url, metadata.strong.timeout_seconds);
        let strong = RetryingProvider::new(strong_llama, 3);

        let cheap_tokens = metadata.cheap.n_predict as u32;
        let strong_tokens = metadata.strong.n_predict as u32;

        Ok(Self {
            metadata,
            cheap,
            strong,
            cheap_tokens,
            strong_tokens,
            _servers: servers,
        })
    }
}

fn make_provider_run_metadata(
    provider: &ProviderConfig,
) -> Result<ProviderRunMetadata, Box<dyn Error>> {
    let cheap = resolve_provider_tier(&provider.cheap, provider.timeout_seconds);
    let strong = match &provider.strong {
        Some(strong) => resolve_provider_tier(
            strong,
            provider
                .strong_timeout_seconds
                .unwrap_or(provider.timeout_seconds),
        ),
        None => {
            let mut strong = cheap.clone();
            strong.timeout_seconds = provider
                .strong_timeout_seconds
                .unwrap_or(provider.timeout_seconds);
            strong
        }
    };
    Ok(ProviderRunMetadata { cheap, strong })
}

fn resolve_provider_tier(tier: &ProviderTierConfig, timeout_seconds: u64) -> ProviderTierMetadata {
    match tier {
        ProviderTierConfig::Unmanaged(config) => ProviderTierMetadata {
            base_url: config.base_url.clone(),
            model: config.model.clone(),
            n_predict: config.n_predict,
            timeout_seconds,
            managed: false,
            managed_server: None,
        },
        ProviderTierConfig::Managed(managed) => {
            let config = resolve_managed_llama_cpp(managed);
            ProviderTierMetadata {
                base_url: config.base_url.clone(),
                model: config.model.identity().to_string(),
                n_predict: managed.llama_cpp.n_predict,
                timeout_seconds,
                managed: true,
                managed_server: Some(managed_server_metadata(&config)),
            }
        }
    }
}

fn resolve_managed_llama_cpp(managed: &ManagedProviderConfig) -> ManagedLlamaCppRuntimeConfig {
    resolve_llama_cpp_config(&managed.llama_cpp)
}

fn managed_server_metadata(config: &ManagedLlamaCppRuntimeConfig) -> ManagedProviderServerMetadata {
    ManagedProviderServerMetadata {
        kind: "llama_cpp".to_string(),
        command: config.command.clone(),
        port: config.port,
        context_size: config.context_size,
        startup_timeout_seconds: config.startup_timeout_seconds,
    }
}

fn start_managed_provider_servers(
    provider: &ProviderConfig,
) -> Result<Vec<ManagedProviderServer>, Box<dyn Error>> {
    let mut servers = Vec::new();
    if let ProviderTierConfig::Managed(managed) = &provider.cheap {
        let config = resolve_managed_llama_cpp(managed);
        servers.push(ManagedProviderServer::start_llama_cpp(&config)?);
        log_managed_provider_started("cheap", &config);
    }
    if let Some(ProviderTierConfig::Managed(managed)) = &provider.strong {
        let config = resolve_managed_llama_cpp(managed);
        servers.push(ManagedProviderServer::start_llama_cpp(&config)?);
        log_managed_provider_started("strong", &config);
    }
    Ok(servers)
}

fn log_managed_provider_started(tier: &str, config: &ManagedLlamaCppRuntimeConfig) {
    eprintln!(
        "[provider] {:<7} {:<20} {}:{}",
        tier,
        managed_model_display_name(&config.model),
        config.host,
        config.port
    );
}

/// Strips the repo/directory prefix from a managed model identifier for display.
fn managed_model_display_name(model: &ManagedLlamaCppModelConfig) -> &str {
    let identity = model.identity();
    identity.rsplit('/').next().unwrap_or(identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UnmanagedProviderConfig;

    fn unmanaged_provider(base_url: &str, model: &str, n_predict: usize) -> ProviderConfig {
        ProviderConfig {
            cheap: ProviderTierConfig::Unmanaged(UnmanagedProviderConfig {
                base_url: base_url.to_string(),
                model: model.to_string(),
                n_predict,
            }),
            strong: None,
            timeout_seconds: 120,
            strong_timeout_seconds: None,
        }
    }

    fn managed_tier(
        command: &str,
        model: &str,
        host: &str,
        port: u16,
        n_predict: usize,
        context_size: Option<usize>,
        startup_timeout_seconds: u64,
    ) -> ProviderTierConfig {
        ProviderTierConfig::Managed(ManagedProviderConfig {
            llama_cpp: crate::config::ManagedLlamaCppConfig {
                command: command.to_string(),
                model: ManagedLlamaCppModelConfig::Path(model.to_string()),
                host: host.to_string(),
                port,
                context_size,
                startup_timeout_seconds,
                n_predict,
            },
        })
    }

    #[test]
    fn strong_tier_falls_back_when_no_strong_provider_configured() {
        let config = unmanaged_provider("http://localhost:8080", "cheap-model", 512);

        let metadata = make_provider_run_metadata(&config).unwrap();

        // When strong fields are absent, both tiers must resolve to the cheap values.
        assert_eq!(metadata.strong, metadata.cheap);
        assert_eq!(metadata.strong.base_url, "http://localhost:8080");
    }

    #[test]
    fn provider_run_metadata_uses_effective_tier_identities() {
        let config = ProviderConfig {
            cheap: ProviderTierConfig::Unmanaged(UnmanagedProviderConfig {
                base_url: "http://localhost:8080".to_string(),
                model: "cheap-model".to_string(),
                n_predict: 512,
            }),
            strong: Some(ProviderTierConfig::Unmanaged(UnmanagedProviderConfig {
                base_url: "http://localhost:8081".to_string(),
                model: "strong-model".to_string(),
                n_predict: 1024,
            })),
            timeout_seconds: 120,
            strong_timeout_seconds: Some(180),
        };

        let metadata = make_provider_run_metadata(&config).unwrap();

        assert_eq!(
            metadata,
            ProviderRunMetadata {
                cheap: ProviderTierMetadata {
                    base_url: "http://localhost:8080".to_string(),
                    model: "cheap-model".to_string(),
                    n_predict: 512,
                    timeout_seconds: 120,
                    managed: false,
                    managed_server: None,
                },
                strong: ProviderTierMetadata {
                    base_url: "http://localhost:8081".to_string(),
                    model: "strong-model".to_string(),
                    n_predict: 1024,
                    timeout_seconds: 180,
                    managed: false,
                    managed_server: None,
                },
            }
        );
    }

    #[test]
    fn provider_run_metadata_records_managed_llama_cpp_server() {
        let config = ProviderConfig {
            cheap: managed_tier(
                "llama-server",
                "models/cheap.gguf",
                "127.0.0.1",
                18080,
                512,
                Some(8192),
                45,
            ),
            strong: None,
            timeout_seconds: 120,
            strong_timeout_seconds: None,
        };

        let metadata = make_provider_run_metadata(&config).unwrap();

        let cheap_tier = ProviderTierMetadata {
            base_url: "http://127.0.0.1:18080".to_string(),
            model: "models/cheap.gguf".to_string(),
            n_predict: 512,
            timeout_seconds: 120,
            managed: true,
            managed_server: Some(ManagedProviderServerMetadata {
                kind: "llama_cpp".to_string(),
                command: "llama-server".to_string(),
                port: 18080,
                context_size: Some(8192),
                startup_timeout_seconds: 45,
            }),
        };
        assert_eq!(
            metadata,
            ProviderRunMetadata {
                // Strong tier records the shared managed server when it falls back to cheap.
                strong: cheap_tier.clone(),
                cheap: cheap_tier,
            }
        );
    }

    #[test]
    fn provider_run_metadata_records_separate_strong_managed_server() {
        let config = ProviderConfig {
            cheap: ProviderTierConfig::Unmanaged(UnmanagedProviderConfig {
                base_url: "http://localhost:8080".to_string(),
                model: "models/cheap.gguf".to_string(),
                n_predict: 512,
            }),
            strong: Some(managed_tier(
                "/opt/llama-server",
                "models/strong.gguf",
                "127.0.0.1",
                28080,
                1024,
                None,
                60,
            )),
            timeout_seconds: 120,
            strong_timeout_seconds: Some(180),
        };

        let metadata = make_provider_run_metadata(&config).unwrap();

        assert_eq!(
            metadata,
            ProviderRunMetadata {
                cheap: ProviderTierMetadata {
                    base_url: "http://localhost:8080".to_string(),
                    model: "models/cheap.gguf".to_string(),
                    n_predict: 512,
                    timeout_seconds: 120,
                    managed: false,
                    managed_server: None,
                },
                strong: ProviderTierMetadata {
                    base_url: "http://127.0.0.1:28080".to_string(),
                    model: "models/strong.gguf".to_string(),
                    n_predict: 1024,
                    timeout_seconds: 180,
                    managed: true,
                    managed_server: Some(ManagedProviderServerMetadata {
                        kind: "llama_cpp".to_string(),
                        command: "/opt/llama-server".to_string(),
                        port: 28080,
                        context_size: None,
                        startup_timeout_seconds: 60,
                    }),
                },
            }
        );
    }
}
