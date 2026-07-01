//! Forge CLI entry point.
//!
//! Usage:
//!   forge run     <config.yaml>            — run a forge job from the current artifact state
//!   forge run     <config.yaml> --resume   — resume an interrupted run
//!   forge show    <config.yaml>            — display the current artifact contents
//!   forge history <config.yaml>            — print commit history (newest first)
//!   forge reset   <config.yaml>            — delete and recreate the artifact repository
//!   forge trace   <config.yaml>                — node/attempt-grouped view of the latest run
//!   forge trace   <config.yaml> --run <run-id>  — trace a specific past run
//!   forge trace   <config.yaml> --summary       — print the old flat chronological event list
//!   forge trace   <config.yaml> --prompts       — print full role-prompt-rendered events
//!   forge trace   <config.yaml> --failures      — print full failure-related events
//!
//! For smoke-test scenarios, use the examples:
//!   cargo run --example scheduler_deliberation_demo
//!   cargo run --example deliberation_demo

use clap::{Parser, Subcommand};

use forge_rs::config::ForgeConfig;
use forge_rs::runtime::{ForgeRuntime, TraceFilter, run_history, run_reset, run_show, run_trace};

#[derive(Parser)]
#[command(name = "forge", about = "Drive forge runs from a config file")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a forge job from the current artifact state.
    Run {
        /// Path to the forge config YAML file.
        config: String,
        /// Resume a previously interrupted run instead of starting a new one.
        #[arg(long)]
        resume: bool,
    },
    /// Display the current artifact contents.
    Show {
        /// Path to the forge config YAML file.
        config: String,
    },
    /// Print commit history (newest first).
    History {
        /// Path to the forge config YAML file.
        config: String,
    },
    /// Delete and recreate the artifact repository.
    Reset {
        /// Path to the forge config YAML file.
        config: String,
    },
    /// View telemetry output for a run.
    Trace {
        /// Path to the forge config YAML file.
        config: String,
        /// Run id to trace instead of the latest run.
        #[arg(long)]
        run: Option<String>,
        /// Print the old flat chronological one-line-per-event view instead
        /// of the default node/attempt-grouped view.
        #[arg(long, conflicts_with_all = ["prompts", "failures"])]
        summary: bool,
        /// Show only role-prompt-rendered events, with the full prompt body.
        #[arg(long, conflicts_with_all = ["failures", "summary"])]
        prompts: bool,
        /// Show only failure-related events, with their full content
        /// (raw JSON payloads rendered as YAML for readability).
        #[arg(long, conflicts_with = "summary")]
        failures: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Run { config, resume } => run_config_command(&config, |config| {
            if resume {
                ForgeRuntime::resume(config)
            } else {
                ForgeRuntime::run(config)
            }
        }),
        Command::Show { config } => run_config_command(&config, run_show),
        Command::History { config } => run_config_command(&config, run_history),
        Command::Reset { config } => run_config_command(&config, run_reset),
        Command::Trace {
            config,
            run,
            summary,
            prompts,
            failures,
        } => {
            let filter = if prompts {
                TraceFilter::Prompts
            } else if failures {
                TraceFilter::Failures
            } else if summary {
                TraceFilter::Summary
            } else {
                TraceFilter::Default
            };
            run_config_command(&config, move |config| {
                run_trace(&config.telemetry.directory, run.as_deref(), filter)
            });
        }
    }
}

/// Load the config at `path`, then run `f` with it, exiting on either failure.
fn run_config_command(
    path: &str,
    f: impl FnOnce(ForgeConfig) -> Result<(), Box<dyn std::error::Error>>,
) {
    let config = ForgeConfig::from_file(path).unwrap_or_else(|e| {
        eprintln!("Error loading config: {e}");
        std::process::exit(1);
    });
    handle_result(f(config));
}

fn handle_result(result: Result<(), Box<dyn std::error::Error>>) {
    result.unwrap_or_else(|e| {
        eprintln!("Error: {e}");
        std::process::exit(1);
    });
}
