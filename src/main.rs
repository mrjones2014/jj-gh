use anyhow::Result;
use clap::Parser;

mod auth;
mod cli;
mod config;
mod debug;
mod gh;
mod git;
mod jj;
mod logging;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = cli::Cli::parse();
    let _logger = logging::init(&args.global)?;

    match args.command {
        cli::Command::Pr { action } => match action {
            cli::PrAction::Create(create) => {
                log::info!("pr create not yet implemented (rev = {})", create.rev);
            }
        },
        cli::Command::Debug { action } => debug::dispatch(action).await?,
    }
    Ok(())
}
