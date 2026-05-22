use anyhow::Result;
use clap::Parser;

mod cli;
mod error;
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
    }
    Ok(())
}
