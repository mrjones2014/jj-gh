//! CLI arg parser

use crate::pr::PrAction;
use clap::{Parser, Subcommand};
use log::LevelFilter;
use serde::Serialize;
use std::io::IsTerminal;

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

    /// Set log level explicitly, overrides `-v` and `-q`.
    #[arg(long = "log-level", value_name = "LEVEL", global = true)]
    pub log_level: Option<LevelFilter>,
}

impl GlobalOpts {
    pub fn resolve_log_level(&self) -> LevelFilter {
        if let Some(level) = self.log_level {
            return level;
        }

        if self.quiet {
            return LevelFilter::Error;
        }

        let base = if std::io::stdout().is_terminal() {
            LevelFilter::Info
        } else {
            LevelFilter::Error
        };

        match self.verbose {
            0 => base,
            1 => LevelFilter::Debug,
            _ => LevelFilter::Trace,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Commands to work with PRs.
    Pr {
        #[command(subcommand)]
        action: PrAction,
    },
    /// Diagnostic subcommands. Useful for inspecting the resolved config and pre-flight checks.
    Debug {
        #[command(subcommand)]
        action: DebugAction,
    },
}

#[derive(Debug, clap::Args, Serialize)]
pub struct AuthArgs {
    /// Askpass helper command that prints a GitHub token on stdout;
    /// shell-words split, e.g. `--gh-askpass "op read op://Vault/gh/token"`.
    /// Default: `gh_askpass` in config, then `$GH_ASKPASS`.
    #[arg(long = "gh-askpass", value_name = "CMD", value_parser = shell_words::split)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gh_askpass: Option<Vec<String>>,

    /// Timeout in seconds for the askpass helper. Default: 20.
    #[arg(long = "askpass-timeout", value_name = "SECS")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub askpass_timeout_secs: Option<u64>,
}

#[derive(Debug, Subcommand)]
pub enum DebugAction {
    /// Print the merged config with the token rendered as `***`.
    Config,
    /// Resolve the GitHub token and report success or failure. Never prints the token itself.
    Auth,
    /// Resolve a revision and print commit info, ancestor bookmark, remote URLs,
    /// and the detected default branch.
    Rev {
        #[arg(value_name = "REV")]
        rev: String,
    },
    /// Pre-flight lookup for a PR: resolve the target, check if a PR is already
    /// open for the head, and confirm the base branch exists on the remote.
    PrLookup {
        #[arg(value_name = "REV")]
        rev: String,
    },
}
