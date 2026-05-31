//! CLI arg parser

use crate::{completions::SubcommandStr, pr::PrAction};
use clap::{Parser, Subcommand};
use clap_complete::Shell;
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

#[derive(Debug, clap::Args, Serialize)]
pub struct GlobalOpts {
    /// Increase log verbosity (repeat for more, e.g. `-vv`).
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count, global = true)]
    #[serde(skip)]
    pub verbose: u8,

    /// Drop log level to `ERROR`.
    #[arg(short = 'q', long = "quiet", global = true, conflicts_with = "verbose")]
    #[serde(skip)]
    pub quiet: bool,

    /// Set log level explicitly, overrides `-v` and `-q`.
    #[arg(long = "log-level", value_name = "LEVEL", global = true)]
    #[serde(skip)]
    pub log_level: Option<LevelFilter>,

    /// Git remote used for the user's own pushes and PR head lookups.
    /// Overrides config `default_remote` (default: `origin`).
    #[arg(long = "remote", value_name = "NAME", global = true)]
    #[serde(rename = "default_remote", skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,

    /// Git remote used as the PR target in fork workflows. Overrides config
    /// `upstream_remote` (default: `upstream`).
    #[arg(long = "upstream-remote", value_name = "NAME", global = true)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_remote: Option<String>,

    #[command(flatten)]
    #[serde(flatten)]
    pub auth: AuthArgs,
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
    /// Generate completions (on stdout) for the specified shell.
    ///
    /// Without flags, emits a standalone completion script for the `jj-gh`
    /// binary. With `--jj-alias <NAME> --subcommand <NAME>` (both required
    /// together), emits an overlay that adds completions for
    /// `jj <jj-alias> <tab>` on top of jj's own completion script (source
    /// the overlay *after* `jj util completion <shell>`).
    Completions {
        shell: Shell,
        /// Emit an overlay for `jj <NAME> <tab>` instead of the standalone
        /// `jj-gh` script. Pass the jj alias name (e.g. `pr`). Must be
        /// paired with `--subcommand`.
        #[arg(long = "jj-alias", value_name = "NAME", requires = "jj_gh_subcommand")]
        jj_alias: Option<String>,
        /// jj-gh top-level subcommand whose tree the overlay describes
        /// (e.g. `pr`). Must be paired with `--jj-alias`.
        #[arg(long = "subcommand", value_name = "NAME", requires = "jj_alias")]
        jj_gh_subcommand: Option<SubcommandStr>,
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
