//! Shell completion overlay for `jj <alias> <tab>`.
//!
//! jj's static completion script (`jj util completion <shell>`) does not
//! know about user-defined aliases. When a user aliases `pr` to
//! `["util", "exec", "--", "jj-gh", "pr"]`, typing `jj pr <tab>` produces
//! nothing because jj's script sees `pr` as an unknown subcommand.
//!
//! This module emits a *supplementary* completion fragment that registers
//! against the `jj` binary but is predicated on the alias token being
//! present in the command line. Sourced after jj's own script, it adds
//! completion for the aliased subcommand tree. Multiple overlays (for
//! different aliases) chain: each captures the prior `jj` completer at
//! source time and delegates to it when the alias does not match.
//!
//! Inventory (subcommands, flags) is read from clap's `Command`
//! introspection at runtime rather than duplicated by hand.

mod bash;
mod fish;
mod zsh;

use crate::{Cli, commands::pr::PrAction};
use anyhow::{Result, bail};
use clap::{Arg, Command, CommandFactory, Subcommand};
use std::{fmt::Display, io::Write};

pub fn run(
    bin_name: &str,
    shell: clap_complete::Shell,
    jj_alias: Option<String>,
    jj_gh_subcommand: Option<SubcommandStr>,
) -> Result<()> {
    match (jj_alias, jj_gh_subcommand) {
        (Some(alias), Some(subcommand)) => {
            alias_completions(shell.into(), &alias, subcommand, &mut std::io::stdout())?;
        }
        _ => {
            clap_complete::generate(shell, &mut Cli::command(), bin_name, &mut std::io::stdout());
        }
    }

    Ok(())
}

/// Shells for which an overlay can be emitted. Sibling to
/// `clap_complete::Shell`, but narrower: only shells whose programmable
/// completion model supports the layering we need (predicate-gated rules
/// or wrapper functions) get a dedicated variant. Anything else lands in
/// `Other` so the dispatch can produce a clear error referencing the name
/// the user actually passed.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Shell {
    /// Bash.
    Bash,
    /// Fish.
    Fish,
    /// Zsh.
    Zsh,
    /// Any shell name we don't have an overlay implementation for. The
    /// inner string is whatever the user passed (e.g. `"powershell"`,
    /// `"elvish"`).
    Other(String),
}

impl From<clap_complete::Shell> for Shell {
    fn from(shell: clap_complete::Shell) -> Self {
        match shell {
            clap_complete::Shell::Bash => Self::Bash,
            clap_complete::Shell::Fish => Self::Fish,
            clap_complete::Shell::Zsh => Self::Zsh,
            other => Self::Other(other.to_string()),
        }
    }
}

#[derive(Debug, clap::ValueEnum, Clone, Copy)]
pub enum SubcommandStr {
    Pr,
}

impl Display for SubcommandStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SubcommandStr::Pr => "pr",
        })
    }
}

// this is never actually used, but it gives us compiler errors when new subcommands are added
#[cfg(debug_assertions)]
#[doc(hidden)]
fn _ensure_subcmds_handled(cmd: crate::cli::Command) {
    let _ = match cmd {
        crate::Command::Pr { .. } => SubcommandStr::Pr,
        subcmd @ (crate::Command::Debug { .. } | crate::Command::Completions { .. }) => {
            unreachable!("{subcmd:?} is not supported in this position");
        }
    };
}

/// Emit a completion overlay for `jj <alias> <tab>` to `out`.
fn alias_completions<W: Write>(
    shell: Shell,
    alias: &str,
    subcommand: SubcommandStr,
    out: &mut W,
) -> Result<()> {
    let cmd = match subcommand {
        SubcommandStr::Pr => PrAction::augment_subcommands(Command::new("pr")),
    };
    match shell {
        Shell::Fish => fish::emit(&cmd, alias, out)?,
        Shell::Bash => bash::emit(&cmd, alias, out)?,
        Shell::Zsh => zsh::emit(&cmd, alias, out)?,
        Shell::Other(name) => bail!("--jj-alias overlay not supported for shell `{name}`"),
    }
    Ok(())
}

pub(super) struct SubInfo<'a> {
    pub(super) name: &'a str,
    pub(super) aliases: Vec<&'a str>,
    pub(super) about: Option<String>,
    pub(super) args: Vec<ArgInfo<'a>>,
}

pub(super) struct ArgInfo<'a> {
    pub(super) long: Option<&'a str>,
    pub(super) short: Option<char>,
    pub(super) about: Option<String>,
    pub(super) takes_value: bool,
}

pub(super) fn collect_subs(cmd: &Command) -> Vec<SubInfo<'_>> {
    cmd.get_subcommands()
        .filter(|s| !s.is_hide_set())
        .map(|s| SubInfo {
            name: s.get_name(),
            aliases: s.get_visible_aliases().collect(),
            about: s.get_about().map(ToString::to_string),
            args: collect_args(s),
        })
        .collect()
}

fn collect_args(cmd: &Command) -> Vec<ArgInfo<'_>> {
    cmd.get_arguments()
        .filter(|a| !a.is_hide_set() && !a.is_positional())
        .map(|a: &Arg| ArgInfo {
            long: a.get_long(),
            short: a.get_short(),
            about: a.get_help().map(ToString::to_string),
            takes_value: arg_takes_value(a),
        })
        .collect()
}

fn arg_takes_value(arg: &Arg) -> bool {
    // Explicit `num_args` (e.g. `num_args = 0` on `Option<bool>` flags) wins
    // over the action's default, since the action may report "Set" for an
    // Option-typed field that's used as a presence flag.
    arg.get_num_args()
        .map_or_else(|| arg.get_action().takes_values(), |r| r.takes_values())
}

pub(super) fn first_help_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
}

#[cfg(test)]
pub(super) fn fake_pr_command() -> Command {
    #[derive(Debug, clap::Args)]
    struct FakeCreateArgs {
        /// Open as draft.
        #[arg(long)]
        draft: bool,
        /// Base bookmark.
        #[arg(long, value_name = "BRANCH")]
        base: Option<String>,
    }

    #[derive(Debug, Subcommand)]
    enum FakeAction {
        /// Create a thing.
        #[command(visible_alias = "c")]
        Create(FakeCreateArgs),
        /// Fetch a thing.
        Fetch,
    }

    let cmd = Command::new("pr");
    FakeAction::augment_subcommands(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emit_string(shell: Shell) -> String {
        let mut buf: Vec<u8> = Vec::new();
        alias_completions(shell, "pr", SubcommandStr::Pr, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn real_pr_action_covers_all_visible_subcommands() {
        // Catches regressions when adding/renaming subcommands or visible
        // aliases on `PrAction`; fake_pr_command is too narrow to notice.
        let bash = emit_string(Shell::Bash);
        for name in ["create", "c", "fetch", "f", "auto-merge", "am", "log", "l"] {
            assert!(bash.contains(name), "bash overlay missing `{name}`");
        }
        assert!(bash.contains("complete -F _jj_gh_alias_wrapper_pr jj"));

        let zsh = emit_string(Shell::Zsh);
        for name in ["create", "c", "fetch", "f", "auto-merge", "am", "log", "l"] {
            assert!(zsh.contains(name), "zsh overlay missing `{name}`");
        }
        assert!(zsh.contains("compdef _jj_gh_alias_pr jj"));

        let fish = emit_string(Shell::Fish);
        for name in ["create", "c", "fetch", "f", "auto-merge", "am", "log", "l"] {
            assert!(
                fish.contains(&format!("-a '{name}'")),
                "fish overlay missing `-a '{name}'`"
            );
        }
        assert!(fish.contains("__jj_gh_alias_no_subcommand pr"));
    }

    #[test]
    fn unsupported_shell_errors_with_name() {
        let mut buf: Vec<u8> = Vec::new();
        let err = alias_completions(
            Shell::Other("powershell".into()),
            "pr",
            SubcommandStr::Pr,
            &mut buf,
        )
        .unwrap_err();
        assert!(err.to_string().contains("powershell"));
    }
}
