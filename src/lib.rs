mod auth;
mod cli;
mod config;
mod debug;
mod fs;
mod gh;
mod git;
mod jj;
mod pr;

pub mod logging;

pub use cli::{Cli, Command};

/// Run the CLI based on the provided arguments
///
/// # Errors
///
/// Propagates the errors from each subcommand.
pub async fn dispatch() -> anyhow::Result<()> {
    use clap::Parser;

    let args = Cli::parse();
    let _logger = logging::init(args.global.resolve_log_level())?;
    match args.command {
        Command::Pr { action } => pr::dispatch(action).await?,
        Command::Debug { action } => debug::dispatch(action).await?,
    }
    Ok(())
}
