use clap::Parser;
use cn_swarms::{config::Config, pipeline};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let result = pipeline::run(Config::parse()).await?;
    println!(
        "Saved {} pages, {} findings, {} reports to {}",
        result.summary.fetched,
        result.summary.findings,
        result.summary.reports,
        result.directory.display()
    );
    if result.summary.interrupted {
        std::process::exit(130);
    }
    if result.summary.failed > 0 {
        eprintln!(
            "{} targets have failures or incomplete judgments; see results.csv",
            result.summary.failed
        );
        std::process::exit(2);
    }
    Ok(())
}
