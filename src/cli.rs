//! CLI arg parser

use crate::commands::{
    completions::{CompletionShell, SubcommandStr},
    pr::PrAction,
};
use clap::{
    Parser, Subcommand,
    builder::{Styles, styling::AnsiColor},
};
use jj_gh_config_derive::subcommand_args;
use log::LevelFilter;
use std::io::IsTerminal;

const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Yellow.on_default().bold())
    .usage(AnsiColor::Yellow.on_default().bold())
    .literal(AnsiColor::Green.on_default().bold())
    .placeholder(AnsiColor::Cyan.on_default())
    .error(AnsiColor::Red.on_default().bold())
    .valid(AnsiColor::Green.on_default().bold())
    .invalid(AnsiColor::Red.on_default().bold());

#[derive(Debug, Parser)]
#[command(name = "jj-gh", version, about, styles = STYLES)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalOptsInput,

    #[command(subcommand)]
    pub command: Command,
}

const GLOBAL_OPTIONS_HEADING: &str = "Global Options";

subcommand_args! {
    #[no_globals]
    pub struct GlobalOpts {
        /// Increase log verbosity (repeat for more, e.g. `-vv`).
        #[arg(short = 'v', long, action = clap::ArgAction::Count, global = true, help_heading = GLOBAL_OPTIONS_HEADING)]
        pub verbose: u8,

        /// Drop log level to `ERROR`.
        #[arg(short = 'q', long, global = true, conflicts_with = "verbose", help_heading = GLOBAL_OPTIONS_HEADING)]
        pub quiet: bool,

        /// Set log level explicitly, overrides `-v` and `-q`.
        #[arg(long, value_name = "LEVEL", global = true, help_heading = GLOBAL_OPTIONS_HEADING)]
        pub log_level: Option<LevelFilter>,

        /// Git remote used for the user's own pushes and PR head lookups.
        /// Precedence: this flag, then git's auto-detected default push remote,
        /// then `default_remote` in config.
        #[arg(long, value_name = "NAME", global = true, help_heading = GLOBAL_OPTIONS_HEADING)]
        #[config(fallback = "default_remote")]
        pub remote: Option<String>,

        /// Git remote used as the PR target in fork workflows. Precedence: this
        /// flag, then `upstream_remote` in config, else
        /// [`crate::gh::remote::DEFAULT_UPSTREAM_REMOTE`].
        #[arg(long, value_name = "NAME", global = true, help_heading = GLOBAL_OPTIONS_HEADING)]
        #[config(fallback = "upstream_remote")]
        pub upstream_remote: Option<String>,

        /// Askpass helper command that prints a GitHub token on stdout;
        /// e.g. `--gh-askpass "op read op://Vault/gh/token"`.
        /// Highest-priority token source; outranks `$GH_ASKPASS`, the token env
        /// vars, and `gh_askpass` in config.
        #[arg(long, value_name = "CMD", value_parser = crate::util::parse_shell_command, global = true, help_heading = GLOBAL_OPTIONS_HEADING)]
        pub gh_askpass: Option<crate::util::ShellCommand>,

        /// Timeout in seconds for the askpass helper. Default: 20.
        #[arg(long = "askpass-timeout", value_name = "SECS", global = true, help_heading = GLOBAL_OPTIONS_HEADING)]
        #[config]
        pub askpass_timeout_secs: u64,
    }
}

impl GlobalOptsInput {
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
        shell: CompletionShell,
        /// Emit an overlay for `jj <NAME> <tab>` instead of the standalone
        /// `jj-gh` script. Pass the jj alias name (e.g. `pr`). Must be
        /// paired with `--subcommand`.
        #[arg(long, value_name = "NAME", requires = "jj_gh_subcommand")]
        jj_alias: Option<String>,
        /// jj-gh top-level subcommand whose tree the overlay describes
        /// (e.g. `pr`). Must be paired with `--jj-alias`.
        #[arg(long = "subcommand", value_name = "NAME", requires = "jj_alias")]
        jj_gh_subcommand: Option<SubcommandStr>,
    },
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
