//! Forge CLI entry point.
//!
//! Usage:
//!   forge run <config.yaml>   — run a forge job described by a YAML config file
//!
//! For smoke-test scenarios, use the examples:
//!   cargo run --example scheduler_deliberation_demo
//!   cargo run --example deliberation_demo

use forge_rs::config::ForgeConfig;
use forge_rs::runtime::ForgeRuntime;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    match args.as_slice() {
        [_, cmd, file] if cmd == "run" => {
            let config = ForgeConfig::from_file(file).unwrap_or_else(|e| {
                eprintln!("Error loading config: {e}");
                std::process::exit(1);
            });
            ForgeRuntime::run(config).unwrap_or_else(|e| {
                eprintln!("Run failed: {e}");
                std::process::exit(1);
            });
        }
        _ => {
            eprintln!("Usage: forge run <config.yaml>");
            std::process::exit(1);
        }
    }
}
