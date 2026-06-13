mod auth;
mod cli;
mod commands;
pub mod config;
mod editor;
mod frontmatter;
mod fs;
mod gh;
mod git;
mod jj;
mod macro_support;
mod model;
mod proc;
mod template;
mod ui;
mod util;

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
        Command::Pr { action } => commands::pr::dispatch(global, action).await?,
        Command::Debug { action } => commands::debug::dispatch(global, action).await?,
        Command::Completions {
            shell,
            jj_alias,
            jj_gh_subcommand,
        } => commands::completions::run(bin_name, shell, jj_alias, jj_gh_subcommand)?,
    }
    Ok(())
}
