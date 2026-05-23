use anyhow::Result;
use clap::Parser;

mod auth;
mod cli;
mod config;
mod debug;
mod fs;
mod gh;
mod git;
mod jj;
mod logging;
mod pr;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = cli::Cli::parse();
    let _logger = logging::init(&args.global)?;

    match args.command {
        cli::Command::Pr { action } => pr::dispatch(action).await?,
        cli::Command::Debug { action } => debug::dispatch(action).await?,
    }
    Ok(())
}
