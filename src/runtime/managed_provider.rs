//! Managed local provider server process ownership.

use std::error::Error;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::ManagedLlamaCppConfig;

/// Resolved managed llama.cpp server launch settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedLlamaCppRuntimeConfig {
    /// Executable path or command name.
    pub command: String,
    /// Model path/identifier passed to `llama-server --model`.
    pub model: String,
    /// HTTP base URL used by Forge provider clients.
    pub base_url: String,
    /// Host passed to `llama-server --host`.
    pub host: String,
    /// Port passed to `llama-server --port`.
    pub port: u16,
    /// Optional context size passed to `--ctx-size`.
    pub context_size: Option<usize>,
    /// Readiness timeout after process spawn.
    pub startup_timeout_seconds: u64,
}

/// Owned managed provider server process.
pub struct ManagedProviderServer {
    child: Child,
    base_url: String,
}

impl ManagedProviderServer {
    /// Spawn a managed llama.cpp server and wait until its health endpoint is reachable.
    pub fn start_llama_cpp(config: &ManagedLlamaCppRuntimeConfig) -> Result<Self, Box<dyn Error>> {
        if endpoint_ready(&config.base_url, Duration::from_millis(200)) {
            return Err(format!(
                "managed provider endpoint {} is already reachable; refusing to attach to an unrelated process",
                config.base_url
            )
            .into());
        }

        let mut command = Command::new(&config.command);
        command
            .arg("--model")
            .arg(&config.model)
            .arg("--host")
            .arg(&config.host)
            .arg("--port")
            .arg(config.port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(context_size) = config.context_size {
            command.arg("--ctx-size").arg(context_size.to_string());
        }

        let mut server = Self {
            child: command.spawn().map_err(|e| {
                format!(
                    "failed to start managed llama.cpp server command '{}': {e}",
                    config.command
                )
            })?,
            base_url: config.base_url.clone(),
        };

        server.wait_until_ready(Duration::from_secs(config.startup_timeout_seconds))?;
        Ok(server)
    }

    fn wait_until_ready(&mut self, timeout: Duration) -> Result<(), Box<dyn Error>> {
        let deadline = Instant::now() + timeout;
        loop {
            if endpoint_ready(&self.base_url, Duration::from_millis(500)) {
                return Ok(());
            }
            if let Some(status) = self.child.try_wait()? {
                return Err(format!(
                    "managed provider server exited before becoming ready: {status}"
                )
                .into());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "managed provider server at {} did not become ready within {} seconds",
                    self.base_url,
                    timeout.as_secs()
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for ManagedProviderServer {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

/// Resolve managed llama.cpp config into concrete launch settings.
pub fn resolve_llama_cpp_config(
    config: &ManagedLlamaCppConfig,
    model: &str,
) -> Result<ManagedLlamaCppRuntimeConfig, Box<dyn Error>> {
    let base_url = match (&config.base_url, config.port) {
        (Some(base_url), _) => normalize_base_url(base_url),
        (None, Some(port)) => format!("http://127.0.0.1:{port}"),
        (None, None) => {
            return Err("managed llama.cpp requires port or base_url".into());
        }
    };
    let endpoint = parse_http_endpoint(&base_url)?;
    if let Some(port) = config.port
        && port != endpoint.port
    {
        return Err(format!(
            "managed llama.cpp port ({port}) must match base_url port ({}) when both are set",
            endpoint.port
        )
        .into());
    }
    Ok(ManagedLlamaCppRuntimeConfig {
        command: config.command.clone(),
        model: model.to_string(),
        base_url,
        host: endpoint.host,
        port: endpoint.port,
        context_size: config.context_size,
        startup_timeout_seconds: config.startup_timeout_seconds,
    })
}

fn endpoint_ready(base_url: &str, timeout: Duration) -> bool {
    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    let url = format!("{}/health", normalize_base_url(base_url));
    agent.get(&url).call().is_ok()
}

fn normalize_base_url(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpEndpoint {
    host: String,
    port: u16,
}

fn parse_http_endpoint(base_url: &str) -> Result<HttpEndpoint, Box<dyn Error>> {
    let rest = base_url
        .strip_prefix("http://")
        .or_else(|| base_url.strip_prefix("https://"))
        .ok_or("managed llama.cpp base_url must start with http:// or https://")?;
    let authority = rest.split('/').next().unwrap_or(rest);
    let (host, port_str) = authority
        .rsplit_once(':')
        .ok_or("managed llama.cpp base_url must include an explicit port")?;
    if host.is_empty() {
        return Err("managed llama.cpp base_url host must be non-empty".into());
    }
    let port = port_str
        .parse::<u16>()
        .map_err(|e| format!("managed llama.cpp base_url port is invalid: {e}"))?;
    Ok(HttpEndpoint {
        host: host.to_string(),
        port,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn managed_config() -> ManagedLlamaCppConfig {
        ManagedLlamaCppConfig {
            command: "llama-server".to_string(),
            port: Some(18080),
            base_url: None,
            context_size: Some(4096),
            startup_timeout_seconds: 10,
        }
    }

    #[test]
    fn resolves_port_to_local_base_url() {
        let resolved = resolve_llama_cpp_config(&managed_config(), "model.gguf").unwrap();
        assert_eq!(resolved.command, "llama-server");
        assert_eq!(resolved.model, "model.gguf");
        assert_eq!(resolved.base_url, "http://127.0.0.1:18080");
        assert_eq!(resolved.host, "127.0.0.1");
        assert_eq!(resolved.port, 18080);
        assert_eq!(resolved.context_size, Some(4096));
    }

    #[test]
    fn base_url_overrides_derived_base_url() {
        let mut config = managed_config();
        config.base_url = Some("http://localhost:18080/".to_string());
        let resolved = resolve_llama_cpp_config(&config, "model.gguf").unwrap();
        assert_eq!(resolved.base_url, "http://localhost:18080");
        assert_eq!(resolved.host, "localhost");
        assert_eq!(resolved.port, 18080);
    }

    #[test]
    fn base_url_and_port_must_match_when_both_are_set() {
        let mut config = managed_config();
        config.base_url = Some("http://localhost:28080/".to_string());
        let err = resolve_llama_cpp_config(&config, "model.gguf").unwrap_err();
        assert!(err.to_string().contains("must match"));
    }

    #[test]
    fn base_url_port_used_when_port_absent() {
        let mut config = managed_config();
        config.port = None;
        config.base_url = Some("http://localhost:28080".to_string());
        let resolved = resolve_llama_cpp_config(&config, "model.gguf").unwrap();
        assert_eq!(resolved.base_url, "http://localhost:28080");
        assert_eq!(resolved.host, "localhost");
        assert_eq!(resolved.port, 28080);
    }

    #[test]
    fn missing_port_and_base_url_fails() {
        let mut config = managed_config();
        config.port = None;
        config.base_url = None;
        let err = resolve_llama_cpp_config(&config, "model.gguf").unwrap_err();
        assert!(err.to_string().contains("port or base_url"));
    }
}
