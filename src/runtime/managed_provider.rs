//! Managed local provider server process ownership.

use std::error::Error;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::{ManagedLlamaCppConfig, ManagedLlamaCppModelConfig};

/// Number of trailing log lines included in startup-failure error messages.
const LOG_TAIL_LINES: usize = 200;

/// Resolved managed llama.cpp server launch settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedLlamaCppRuntimeConfig {
    /// Executable path or command name.
    pub command: String,
    /// Model source passed to `llama-server`.
    pub model: ManagedLlamaCppModelConfig,
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
    /// Number of concurrent slots passed to `--parallel`.
    pub parallel: usize,
}

/// Owned managed provider server process.
pub struct ManagedProviderServer {
    child: Child,
    base_url: String,
    log_path: PathBuf,
}

impl ManagedProviderServer {
    /// Spawn a managed llama.cpp server and wait until its health endpoint is reachable.
    ///
    /// The server's stdout/stderr are redirected to `log_path` (created, including
    /// parent directories, if it doesn't exist; appended to otherwise) so request
    /// logging, slot allocation, and crash output survive for diagnosis instead of
    /// being discarded.
    pub fn start_llama_cpp(
        config: &ManagedLlamaCppRuntimeConfig,
        log_path: &Path,
    ) -> Result<Self, Box<dyn Error>> {
        if endpoint_ready(&config.base_url, Duration::from_millis(200)) {
            return Err(format!(
                "managed provider endpoint {} is already reachable; refusing to attach to an unrelated process",
                config.base_url
            )
            .into());
        }

        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .map_err(|e| {
                format!(
                    "failed to open managed server log '{}': {e}",
                    log_path.display()
                )
            })?;
        writeln!(
            log_file,
            "\n=== starting {} (pid pending) ===",
            config.command
        )?;
        let stdout_handle = log_file.try_clone()?;
        let stderr_handle = log_file.try_clone()?;

        let mut command = Command::new(&config.command);
        append_model_args(&mut command, &config.model);
        command
            .arg("--host")
            .arg(&config.host)
            .arg("--port")
            .arg(config.port.to_string())
            .arg("--parallel")
            .arg(config.parallel.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout_handle))
            .stderr(Stdio::from(stderr_handle));
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
            log_path: log_path.to_path_buf(),
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
                    "managed provider server exited before becoming ready: {status}\n--- {} (last {LOG_TAIL_LINES} lines) ---\n{}",
                    self.log_path.display(),
                    log_tail(&self.log_path, LOG_TAIL_LINES)
                )
                .into());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "managed provider server at {} did not become ready within {} seconds\n--- {} (last {LOG_TAIL_LINES} lines) ---\n{}",
                    self.base_url,
                    timeout.as_secs(),
                    self.log_path.display(),
                    log_tail(&self.log_path, LOG_TAIL_LINES)
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
pub fn resolve_llama_cpp_config(config: &ManagedLlamaCppConfig) -> ManagedLlamaCppRuntimeConfig {
    let host = config.host.trim().to_string();
    let base_url = format!("http://{}:{}", host, config.port);
    ManagedLlamaCppRuntimeConfig {
        command: config.command.clone(),
        model: config.model.clone(),
        base_url,
        host,
        port: config.port,
        context_size: config.context_size,
        startup_timeout_seconds: config.startup_timeout_seconds,
        parallel: config.parallel,
    }
}

fn append_model_args(command: &mut Command, model: &ManagedLlamaCppModelConfig) {
    match model {
        ManagedLlamaCppModelConfig::Path(path) => {
            command.arg("--model").arg(path);
        }
        ManagedLlamaCppModelConfig::HuggingFace(hf) => {
            command.arg("-hf").arg(hf);
        }
    }
}

fn endpoint_ready(base_url: &str, timeout: Duration) -> bool {
    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    let url = format!("{}/health", normalize_base_url(base_url));
    agent.get(&url).call().is_ok()
}

fn normalize_base_url(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_string()
}

/// Returns the last `max_lines` lines of the log file at `path`, for embedding
/// in startup-failure error messages.
fn log_tail(path: &Path, max_lines: usize) -> String {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(max_lines);
            lines[start..].join("\n")
        }
        Err(e) => format!("<failed to read log file {}: {e}>", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn managed_config() -> ManagedLlamaCppConfig {
        ManagedLlamaCppConfig {
            command: "llama-server".to_string(),
            model: ManagedLlamaCppModelConfig::Path("model.gguf".to_string()),
            host: "127.0.0.1".to_string(),
            port: 18080,
            context_size: Some(4096),
            startup_timeout_seconds: 10,
            n_predict: 512,
            parallel: 2,
        }
    }

    #[test]
    fn resolves_host_to_base_url() {
        let cases = [
            ("127.0.0.1", "http://127.0.0.1:18080"),
            ("localhost", "http://localhost:18080"),
        ];
        for (host, expected_base_url) in cases {
            let mut config = managed_config();
            config.host = host.to_string();
            let resolved = resolve_llama_cpp_config(&config);
            assert_eq!(resolved.command, "llama-server");
            assert_eq!(
                resolved.model,
                ManagedLlamaCppModelConfig::Path("model.gguf".to_string())
            );
            assert_eq!(resolved.base_url, expected_base_url, "host: {host}");
            assert_eq!(resolved.host, host);
            assert_eq!(resolved.port, 18080);
            assert_eq!(resolved.context_size, Some(4096));
            assert_eq!(resolved.parallel, 2);
        }
    }

    fn temp_workspace(label: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "forge-managed-provider-test-{label}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("failed to create test workspace");
        dir
    }

    /// A managed server whose process never becomes reachable must still leave
    /// its real stdout/stderr behind in the log file, and the resulting
    /// readiness error must surface that captured output rather than only an
    /// exit status.
    #[test]
    fn start_llama_cpp_captures_process_output_in_log_file() {
        let workspace = temp_workspace("log-capture");
        let script_path = workspace.join("fake-llama-server.sh");
        std::fs::write(
            &script_path,
            "#!/bin/sh\necho REAL_OUTPUT_MARKER\necho REAL_ERROR_MARKER >&2\nexit 1\n",
        )
        .expect("failed to write fake server script");
        Command::new("chmod")
            .arg("+x")
            .arg(&script_path)
            .status()
            .expect("failed to chmod fake server script");

        let log_path = workspace.join("llama-server.log");
        let config = ManagedLlamaCppRuntimeConfig {
            command: script_path.to_str().unwrap().to_string(),
            model: ManagedLlamaCppModelConfig::Path("unused.gguf".to_string()),
            base_url: "http://127.0.0.1:1".to_string(),
            host: "127.0.0.1".to_string(),
            port: 1,
            context_size: None,
            startup_timeout_seconds: 2,
            parallel: 1,
        };

        let err = match ManagedProviderServer::start_llama_cpp(&config, &log_path) {
            Ok(_) => {
                panic!("fake script exits immediately and can never serve the health endpoint")
            }
            Err(e) => e,
        };
        let log_contents =
            std::fs::read_to_string(&log_path).expect("log file must be created on disk");
        assert!(
            log_contents.contains("REAL_OUTPUT_MARKER"),
            "log must capture real stdout from the spawned process: {log_contents}"
        );
        assert!(
            log_contents.contains("REAL_ERROR_MARKER"),
            "log must capture real stderr from the spawned process: {log_contents}"
        );

        let err_message = err.to_string();
        assert!(
            err_message.contains("REAL_ERROR_MARKER"),
            "startup-failure error must include captured log content, not just exit status: {err_message}"
        );
    }

    #[test]
    fn model_variant_selects_launch_argument() {
        let cases = [
            (
                ManagedLlamaCppModelConfig::Path("models/local.gguf".to_string()),
                vec!["--model", "models/local.gguf"],
            ),
            (
                ManagedLlamaCppModelConfig::HuggingFace(
                    "lm-kit/qwen-3-8b-instruct-gguf:Q4_K_M".to_string(),
                ),
                vec!["-hf", "lm-kit/qwen-3-8b-instruct-gguf:Q4_K_M"],
            ),
        ];
        for (model, expected_args) in cases {
            let mut command = Command::new("llama-server");
            append_model_args(&mut command, &model);
            let args: Vec<_> = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            assert_eq!(args, expected_args, "model: {model:?}");
        }
    }
}
