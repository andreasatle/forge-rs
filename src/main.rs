//! Forge CLI entry point.
//!
//! Usage:
//!   forge run     <config.yaml>            — run a forge job from the current artifact state
//!   forge run     <config.yaml> --resume   — resume an interrupted run
//!   forge show    <config.yaml>            — display the current artifact contents
//!   forge history <config.yaml>            — print commit history (newest first)
//!   forge reset   <config.yaml>            — delete and recreate the artifact repository
//!
//! For smoke-test scenarios, use the examples:
//!   cargo run --example scheduler_deliberation_demo
//!   cargo run --example deliberation_demo

use forge_rs::config::ForgeConfig;
use forge_rs::runtime::{ForgeRuntime, run_history, run_reset, run_show};

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let (cmd, file, resume) = match args.as_slice() {
        [_, cmd, file] => (cmd.as_str(), file.as_str(), false),
        [_, cmd, file, flag] if flag == "--resume" => (cmd.as_str(), file.as_str(), true),
        _ => {
            eprintln!("Usage: forge <run|show|history|reset> <config.yaml> [--resume]");
            std::process::exit(1);
        }
    };

    let config = ForgeConfig::from_file(file).unwrap_or_else(|e| {
        eprintln!("Error loading config: {e}");
        std::process::exit(1);
    });

    let result = match (cmd, resume) {
        ("run", false) => ForgeRuntime::run(config),
        ("run", true) => ForgeRuntime::resume(config),
        ("show", _) => run_show(config),
        ("history", _) => run_history(config),
        ("reset", _) => run_reset(config),
        (other, _) => {
            eprintln!(
                "Unknown command '{other}'. Usage: forge <run|show|history|reset> <config.yaml>"
            );
            std::process::exit(1);
        }
    };

    result.unwrap_or_else(|e| {
        eprintln!("Error: {e}");
        std::process::exit(1);
    });
}
