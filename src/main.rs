//! WinKit binary: MCP server over stdio, with CLI subcommands.
//!
//! Usage:
//! ```text
//! winkit [--config <path>] [--help] [--version]
//! winkit doctor [--json] [--config <path>]
//! winkit init --client <opencode|claude-code|codex|generic> [--write] [--force]
//! winkit install [--yes] [--list] [--json]
//! winkit configure [--dry-run] [--write] [--set KEY=VALUE]...
//! ```
//!
//! With no subcommand, WinKit runs as an MCP stdio server: all protocol
//! traffic flows over stdin/stdout as newline-delimited JSON-RPC and
//! diagnostics go to stderr, so stdout stays protocol-clean. When the first
//! non-flag argument is a subcommand (`doctor`, `init`, `install`,
//! `configure`), the CLI path runs instead and owns stdout for its
//! human/machine output.

use std::path::PathBuf;
use std::process::ExitCode;

use winkit::cli;
use winkit::config;
use winkit::server::AppState;
use winkit::utils::log::{self, Level};
use winkit::{log_error, log_info};

const SUBCOMMANDS: [&str; 4] = ["doctor", "init", "install", "configure"];

const USAGE: &str = "\
WinKit — local Windows observability and diagnostics for AI agents (MCP server)

USAGE:
    winkit [OPTIONS] [SUBCOMMAND]

SUBCOMMANDS:
    doctor               Check the installation and report pass/fail per check
    init                 Print an MCP client configuration for WinKit
    install              Register WinKit as an MCP server in every installed AI agent
    configure            Read, validate, and update the configuration

OPTIONS:
    --config <PATH>      Path to a winkit.toml configuration file
    --version            Print the version and exit
    --help               Print this help and exit

WinKit speaks MCP over stdio: launch it from an MCP client such as OpenCode
or Claude Code, or run `winkit doctor` to verify an installation.
";

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut config_path: Option<PathBuf> = None;
    let mut i = 0;
    while i < raw.len() {
        let arg = &raw[i];
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--version" | "-V" => {
                println!("winkit {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "--config" => {
                i += 1;
                match raw.get(i) {
                    Some(path) => config_path = Some(PathBuf::from(path)),
                    None => {
                        eprintln!("error: --config requires a path argument\n\n{USAGE}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            other if other.starts_with("--config=") => {
                config_path = Some(PathBuf::from(&other["--config=".len()..]));
            }
            other if SUBCOMMANDS.contains(&other) => {
                return cli::run(other, &raw[i + 1..], config_path);
            }
            other => {
                eprintln!("error: unknown argument '{other}'\n\n{USAGE}");
                return ExitCode::FAILURE;
            }
        }
        i += 1;
    }

    let t0 = std::time::Instant::now();
    let cfg = match config::loader::load(config_path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let t_cfg = t0.elapsed();

    let level = log::Level::parse(&cfg.server.log_level).unwrap_or(Level::Info);
    log::set_level(level);

    let state = match AppState::build(cfg) {
        Ok(state) => state,
        Err(e) => {
            log_error!("startup failed after {:?}: {}", t_cfg, e.message);
            return ExitCode::FAILURE;
        }
    };
    let t_state = t0.elapsed();

    log_info!(
        "WinKit {} starting (MCP over stdio, permission mode '{}', cfg {:?} + state {:?})",
        env!("CARGO_PKG_VERSION"),
        state.permissions.mode.as_str(),
        t_cfg,
        t_state - t_cfg
    );

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            log_error!(
                "failed to start async runtime after {:?}: {e}",
                t0.elapsed()
            );
            return ExitCode::FAILURE;
        }
    };

    match runtime.block_on(winkit::server::transport::run(&state)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log_error!("fatal: {}", e.message);
            ExitCode::FAILURE
        }
    }
}
