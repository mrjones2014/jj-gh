//! CLI arg parser

use clap::{Parser, Subcommand};
use log::LevelFilter;

#[derive(Debug, Parser)]
#[command(name = "jj-gh", version, about)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalOpts,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, clap::Args)]
pub struct GlobalOpts {
    /// Increase log verbosity (repeat for more, e.g. `-vv`).
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Drop log level to `ERROR`.
    #[arg(short = 'q', long = "quiet", global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Set log level explicitly, verrides `-v` and `-q`.
    #[arg(long = "log-level", value_name = "LEVEL", global = true)]
    pub log_level: Option<LevelFilter>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Commands to work with PRs.
    Pr {
        #[command(subcommand)]
        action: PrAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum PrAction {
    /// Create a pull request from a revision. This supports stacked PRs; by deafult, the base
    /// branch will be set to the closest ancestor bookmark if one exists, otherwise `trunk()`.
    #[command(visible_alias = "c")]
    Create(CreateArgs),
}

#[derive(Debug, clap::Args)]
pub struct CreateArgs {
    /// Revision to create the PR from.
    #[arg(value_name = "REV")]
    pub rev: String,
}
