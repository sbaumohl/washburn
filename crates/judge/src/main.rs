use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use washburn_common::{WashburnConfig, config_path_from_env_or_arg};
use washburn_judge::{AppState, build_app, init_tracing, load_rubric, shutdown_signal};

#[derive(Parser)]
#[command(
    version,
    about = "HTTP judge service wrapping the OpenRouter review model"
)]
struct Cli {
    /// Path to washburn.toml (overridden by WASHBURN_CONFIG)
    #[arg(long)]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    let path = config_path_from_env_or_arg(cli.config);
    let config = WashburnConfig::load(&path)?;
    let rubric = load_rubric()?;
    let state = AppState::new(&config, rubric)?;
    let listener = tokio::net::TcpListener::bind(&config.judge.bind)
        .await
        .with_context(|| format!("bind judge listener on {}", config.judge.bind))?;
    tracing::info!(bind = %config.judge.bind, "washburn-judge listening");
    axum::serve(listener, build_app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve judge HTTP")?;
    tracing::info!("washburn-judge stopped");
    Ok(())
}
