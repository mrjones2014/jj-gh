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

    /// Set log level explicitly, overrides `-v` and `-q`.
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
    /// Diagnostic subcommands. Useful for inspecting the resolved config and pre-flight checks.
    Debug {
        #[command(subcommand)]
        action: DebugAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum PrAction {
    /// Create a pull request from a revision. This supports stacked PRs; by default the base
    /// branch is set to the closest ancestor bookmark if one exists, otherwise `trunk()`.
    #[command(visible_alias = "c")]
    Create(CreateArgs),
    /// Fetch a pull request by number into a local bookmark. Requires a colocated
    /// git repository; the special `refs/pull/123/head` ref is fetched via `git`
    /// because `jj` cannot yet fetch arbitrary refs.
    #[command(visible_alias = "f")]
    Fetch(FetchArgs),
}

#[derive(Debug, clap::Args)]
pub struct AuthArgs {
    /// Askpass helper command that prints a GitHub token on stdout;
    /// shell-words split, e.g. `--gh-askpass "op read op://Vault/gh/token"`.
    /// Default: `gh_askpass` in config, then `$GH_ASKPASS`.
    #[arg(long = "gh-askpass", value_name = "CMD", value_parser = shell_words::split)]
    pub gh_askpass: Option<Vec<String>>,

    /// Timeout in seconds for the askpass helper. Default: 20.
    #[arg(long = "askpass-timeout", value_name = "SECS")]
    pub askpass_timeout_secs: Option<u64>,
}

#[derive(Debug, clap::Args)]
pub struct CreateArgs {
    /// Revision to create the PR from.
    #[arg(value_name = "REV")]
    pub rev: String,

    /// Override the base bookmark. Default: closest ancestor bookmark on the
    /// stack, falling back to the remote's `main` / `master` / configured
    /// `default_base_branch`.
    #[arg(long, value_name = "BRANCH")]
    pub base: Option<String>,

    /// Force the PR to be a draft. Overrides config (default: `draft = false`).
    #[arg(long)]
    pub draft: bool,

    /// Force the PR to be non-draft. Overrides config.
    #[arg(long = "no-draft", conflicts_with = "draft")]
    pub no_draft: bool,

    /// Template path or name under `.github/PULL_REQUEST_TEMPLATE/`. Default:
    /// `template_path` in config, else auto-detect
    /// `.github/PULL_REQUEST_TEMPLATE.md`.
    #[arg(long, value_name = "PATH_OR_NAME")]
    pub template: Option<String>,

    /// Skip template selection entirely.
    #[arg(long = "no-template", conflicts_with = "template")]
    pub no_template: bool,

    /// Editor command; shell-words split, e.g. `--editor "nvim +7"`. Default:
    /// `editor` in config, then `$VISUAL`, then `$EDITOR`.
    #[arg(short = 'e', long, value_name = "CMD", value_parser = shell_words::split)]
    pub editor: Option<Vec<String>>,

    #[command(flatten)]
    pub auth: AuthArgs,
}

#[derive(Debug, clap::Args)]
pub struct FetchArgs {
    /// PR number to fetch.
    #[arg(value_name = "PR_NUM")]
    pub pr: u64,

    /// Override the bookmark template. Default: `pr_fetch_bookmark_template`
    /// in config, else `pr-{number}/{branch}`. Placeholders: `{number}`,
    /// `{branch}` (head.ref), `{user}`
    /// (head.user.login), `{repo}` (head.repo.name). `{{` / `}}` are literal
    /// braces.
    #[arg(short = 't', long, value_name = "STR")]
    pub template: Option<String>,

    /// Replace an existing local bookmark of the same name.
    #[arg(short = 'f', long)]
    pub force: bool,

    #[command(flatten)]
    pub auth: AuthArgs,
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
