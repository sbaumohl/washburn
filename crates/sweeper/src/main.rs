//! `washburn-sweeper`: the crawl and screening daemon. See PROJECTS.md 3.4.
//!
//! Exit codes: 0 normal or SIGTERM; 3 with `--once` when no host was free;
//! 130 on Ctrl-C; 1 on error. Logs are JSON when `WASHBURN_LOG_JSON=1`.

use std::{path::PathBuf, process::ExitCode};

use anyhow::Result;
use clap::{ArgAction, Parser};
use tracing_subscriber::EnvFilter;
use washburn_common::{WashburnConfig, config_path_from_env_or_arg};
use washburn_sweeper::{RunMode, RunOutcome, run};

#[derive(Parser)]
#[command(name = "washburn-sweeper", about = "Lease hosts, crawl, screen with Jev, review with the judge")]
struct Cli {
    /// Path to washburn.toml (`WASHBURN_CONFIG` takes precedence).
    #[arg(long)]
    config: Option<PathBuf>,
    /// Lease and drain one host, then exit (exit 3 if none was free).
    #[arg(long)]
    once: bool,
    /// Store pages without Jev or the judge.
    #[arg(long)]
    crawl_only: bool,
    /// Run migrations at startup. On by default; `--migrate=false` skips.
    #[arg(
        long,
        default_value_t = true,
        num_args = 0..=1,
        default_missing_value = "true",
        action = ArgAction::Set
    )]
    migrate: bool,
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    init_tracing();
    let cli = Cli::parse();
    let config = WashburnConfig::load(&config_path_from_env_or_arg(cli.config))?;
    let mode = RunMode {
        once: cli.once,
        crawl_only: cli.crawl_only,
        migrate: cli.migrate,
    };
    Ok(match run(config, mode).await? {
        RunOutcome::Drained => ExitCode::SUCCESS,
        RunOutcome::NoHost => ExitCode::from(3),
        RunOutcome::Shutdown { sigint: true } => ExitCode::from(130),
        RunOutcome::Shutdown { sigint: false } => ExitCode::SUCCESS,
    })
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    if std::env::var("WASHBURN_LOG_JSON").is_ok_and(|value| value == "1") {
        builder.json().init();
    } else {
        builder.init();
    }
}
