use clap::CommandFactory as _;

mod auth;
mod cli;
mod completions;
pub mod config;
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
pub async fn dispatch(bin_name: &str) -> anyhow::Result<()> {
    use clap::Parser;

    let args = Cli::parse();
    let _logger = logging::init(args.global.resolve_log_level())?;
    let global = args.global;
    match args.command {
        Command::Pr { action } => pr::dispatch(&global, action).await?,
        Command::Debug { action } => debug::dispatch(&global, action).await?,
        Command::Completions {
            shell,
            jj_alias,
            jj_gh_subcommand,
        } => match (jj_alias, jj_gh_subcommand) {
            (Some(alias), Some(subcommand)) => {
                completions::run(shell.into(), &alias, subcommand, &mut std::io::stdout())?;
            }
            _ => {
                clap_complete::generate(
                    shell,
                    &mut Cli::command(),
                    bin_name,
                    &mut std::io::stdout(),
                );
            }
        },
    }
    Ok(())
}
