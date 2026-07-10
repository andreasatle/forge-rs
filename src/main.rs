//! Forge CLI entry point.
//!
//! Usage:
//!   forge start   <config.yaml>            — run a forge job from the current artifact state
//!   forge start   <config.yaml> --resume   — resume an interrupted run
//!   forge show    <config.yaml>            — display the current artifact contents
//!   forge history <config.yaml>            — print commit history (newest first)
//!   forge reset   <config.yaml>            — delete and recreate the artifact repository
//!   forge trace   <config.yaml>                — node/attempt-grouped view of the latest run
//!   forge trace   <config.yaml> --run <run-id>  — trace a specific past run
//!   forge trace   <config.yaml> --summary       — print the old flat chronological event list
//!   forge trace   <config.yaml> --prompts       — print full role-prompt-rendered events
//!   forge trace   <config.yaml> --failures      — print full failure-related events
//!
//!   forge vast search --min-ram <gb> --max-price <usd/hr>  — list GPU offers
//!   forge vast rent <offer_id> [--disk <gb>]                — rent an instance
//!   forge vast list                                         — list current instances
//!   forge vast destroy <instance_id>                        — destroy an instance
//!
//!   forge prompt-preview <config.yaml> --node <plan|work> --role <producer|critic|referee> [--worker <name>]
//!       — render the static prompt template for a role/node-kind combination
//!
//! For smoke-test scenarios, use the examples:
//!   cargo run --example scheduler_deliberation_demo
//!   cargo run --example deliberation_demo

use clap::{Parser, Subcommand, ValueEnum};

use forge_rs::config::ForgeConfig;
use forge_rs::machines::deliberation::DeliberationRole;
use forge_rs::machines::scheduler::NodeKind;
use forge_rs::project::ProjectAdapter;
use forge_rs::roles::render_prompt_preview;
use forge_rs::runtime::{ForgeRuntime, TraceFilter, run_history, run_reset, run_show, run_trace};
use forge_rs::vast::VastClient;

#[derive(Parser)]
#[command(name = "forge", about = "Drive forge runs from a config file")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a forge job from the current artifact state.
    Start {
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
    /// Manage Vast.ai GPU rental instances.
    Vast {
        #[command(subcommand)]
        action: VastCommand,
    },
    /// Render the static prompt template for a role/node-kind combination.
    PromptPreview {
        /// Path to the forge config YAML file.
        config: String,
        /// Node kind whose prompt template to render.
        #[arg(long, value_enum)]
        node: NodeKindArg,
        /// Role whose prompt template to render.
        #[arg(long, value_enum)]
        role: RoleArg,
        /// Worker role name (for `--node work` only); selects that role's
        /// prompts in place of the adapter's shared worker prompts.
        #[arg(long)]
        worker: Option<String>,
    },
}

/// CLI-facing mirror of [`NodeKind`], since the domain type has no
/// `clap::ValueEnum` impl of its own.
#[derive(Clone, Copy, ValueEnum)]
enum NodeKindArg {
    /// A planning node that assigns worker roles and file operations.
    Plan,
    /// An execution node.
    Work,
}

impl From<NodeKindArg> for NodeKind {
    fn from(value: NodeKindArg) -> Self {
        match value {
            NodeKindArg::Plan => NodeKind::Plan,
            NodeKindArg::Work => NodeKind::Work,
        }
    }
}

/// CLI-facing mirror of [`DeliberationRole`], since the domain type has no
/// `clap::ValueEnum` impl of its own.
#[derive(Clone, Copy, ValueEnum)]
enum RoleArg {
    /// Generates the initial content for the objective.
    Producer,
    /// Evaluates the producer's content and accepts or rejects it.
    Critic,
    /// Makes the final acceptance decision after the critic has weighed in.
    Referee,
}

impl From<RoleArg> for DeliberationRole {
    fn from(value: RoleArg) -> Self {
        match value {
            RoleArg::Producer => DeliberationRole::Producer,
            RoleArg::Critic => DeliberationRole::Critic,
            RoleArg::Referee => DeliberationRole::Referee,
        }
    }
}

#[derive(Subcommand)]
enum VastCommand {
    /// List available GPU offers, sorted by price.
    Search {
        /// Minimum GPU memory in GB.
        #[arg(long)]
        min_ram: f64,
        /// Maximum price per hour in USD.
        #[arg(long)]
        max_price: f64,
    },
    /// Rent an instance from an offer.
    Rent {
        /// Offer id to rent.
        offer_id: u64,
        /// Local disk size in GB.
        #[arg(long, default_value_t = 16.0)]
        disk: f64,
    },
    /// List current instances with SSH connection info.
    List,
    /// Destroy an instance.
    Destroy {
        /// Instance id to destroy.
        instance_id: u64,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Start { config, resume } => run_config_command(&config, |config| {
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
        Command::Vast { action } => handle_result(run_vast_command(action)),
        Command::PromptPreview {
            config,
            node,
            role,
            worker,
        } => run_config_command(&config, move |config| {
            run_prompt_preview(&config, node.into(), role.into(), worker)
        }),
    }
}

/// Load the adapter `config` points at and print the rendered prompt
/// template for `node_kind`/`role` (and, for [`NodeKind::Work`], `worker`).
fn run_prompt_preview(
    config: &ForgeConfig,
    node_kind: NodeKind,
    role: DeliberationRole,
    worker: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let adapter = forge_rs::project::load_adapter(std::path::Path::new(&config.adapter))?;
    let policy = adapter.role_policy();

    if let Some(worker_role) = &worker {
        let known = policy
            .worker_role_descriptions
            .iter()
            .any(|(name, _)| name == worker_role);
        if !known {
            let available: Vec<&str> = policy
                .worker_role_descriptions
                .iter()
                .map(|(name, _)| name.as_str())
                .collect();
            return Err(format!(
                "unknown worker role '{worker_role}'; available roles: {}",
                available.join(", ")
            )
            .into());
        }
    }

    println!(
        "{}",
        render_prompt_preview(&policy, node_kind, role, worker)
    );
    Ok(())
}

/// Run a `vast` subcommand against the Vast.ai API.
fn run_vast_command(action: VastCommand) -> Result<(), Box<dyn std::error::Error>> {
    let client = VastClient::new()?;
    match action {
        VastCommand::Search { min_ram, max_price } => {
            let mut offers = client.search_offers(min_ram, max_price)?;
            offers.sort_by(|a, b| a.price_per_hour.total_cmp(&b.price_per_hour));
            for offer in offers {
                println!(
                    "{:<10} {:<20} {:>6.1} GB  ${:.3}/hr  x{}",
                    offer.id,
                    offer.gpu_name,
                    offer.gpu_ram_gb,
                    offer.price_per_hour,
                    offer.num_gpus
                );
            }
        }
        VastCommand::Rent { offer_id, disk } => {
            let instance = client.create_instance(offer_id, disk)?;
            println!(
                "Rented instance {} ({}), status: {}",
                instance.id, instance.gpu_name, instance.status
            );
        }
        VastCommand::List => {
            for instance in client.list_instances()? {
                let ssh = match (&instance.ssh_host, instance.ssh_port) {
                    (Some(host), Some(port)) => format!("{host}:{port}"),
                    _ => "not ready".to_string(),
                };
                println!(
                    "{:<10} {:<10} {:<20} {}",
                    instance.id, instance.status, instance.gpu_name, ssh
                );
            }
        }
        VastCommand::Destroy { instance_id } => {
            client.destroy_instance(instance_id)?;
            println!("Destroyed instance {instance_id}");
        }
    }
    Ok(())
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
