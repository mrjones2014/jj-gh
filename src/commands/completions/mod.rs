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
mod nushell;
mod zsh;

use crate::{Cli, commands::pr::PrAction};
use anyhow::{Result, bail};
use clap::{Arg, Command, CommandFactory, Subcommand};
use std::{fmt::Display, io::Write};

pub fn run(
    bin_name: &str,
    shell: CompletionShell,
    jj_alias: Option<String>,
    jj_gh_subcommand: Option<SubcommandStr>,
) -> Result<()> {
    if let (Some(alias), Some(subcommand)) = (jj_alias, jj_gh_subcommand) {
        alias_completions(shell, &alias, subcommand, &mut std::io::stdout())?;
    } else {
        let mut cmd = Cli::command();
        cmd.set_bin_name(bin_name);
        cmd.build();
        shell.generator().generate(&cmd, &mut std::io::stdout());
    }

    Ok(())
}

/// Shell completion generators supported by jj-gh.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, clap::ValueEnum)]
pub enum CompletionShell {
    /// Bash.
    Bash,
    /// Elvish.
    Elvish,
    /// Fish.
    Fish,
    /// Nushell.
    Nushell,
    /// PowerShell.
    #[value(name = "powershell")]
    PowerShell,
    /// Zsh.
    Zsh,
}

impl CompletionShell {
    fn generator(&self) -> &dyn clap_complete::Generator {
        match self {
            Self::Bash => &clap_complete::Shell::Bash,
            Self::Elvish => &clap_complete::Shell::Elvish,
            Self::Fish => &clap_complete::Shell::Fish,
            Self::Nushell => &clap_complete_nushell::Nushell,
            Self::PowerShell => &clap_complete::Shell::PowerShell,
            Self::Zsh => &clap_complete::Shell::Zsh,
        }
    }
}

impl Display for CompletionShell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Bash => "bash",
            Self::Elvish => "elvish",
            Self::Fish => "fish",
            Self::Nushell => "nushell",
            Self::PowerShell => "powershell",
            Self::Zsh => "zsh",
        })
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
        subcmd @ (crate::Command::Config { .. }
        | crate::Command::Debug { .. }
        | crate::Command::Completions { .. }) => {
            unreachable!("{subcmd:?} is not supported in this position");
        }
    };
}

/// Emit a completion overlay for `jj <alias> <tab>` to `out`.
fn alias_completions<W: Write>(
    shell: CompletionShell,
    alias: &str,
    subcommand: SubcommandStr,
    out: &mut W,
) -> Result<()> {
    let cmd = match subcommand {
        SubcommandStr::Pr => PrAction::augment_subcommands(Command::new("pr")),
    };
    match shell {
        CompletionShell::Fish => fish::emit(&cmd, alias, out)?,
        CompletionShell::Bash => bash::emit(&cmd, alias, out)?,
        CompletionShell::Nushell => nushell::emit(&cmd, alias, out)?,
        CompletionShell::Zsh => zsh::emit(&cmd, alias, out)?,
        shell @ (CompletionShell::Elvish | CompletionShell::PowerShell) => {
            bail!("--jj-alias overlay not supported for shell `{shell}`");
        }
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

    fn emit_string(shell: CompletionShell) -> String {
        let mut buf = Vec::<u8>::new();
        alias_completions(shell, "pr", SubcommandStr::Pr, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn real_pr_action_covers_all_visible_subcommands() {
        // Catches regressions when adding/renaming subcommands or visible
        // aliases on `PrAction`; fake_pr_command is too narrow to notice.
        let bash = emit_string(CompletionShell::Bash);
        for name in ["create", "c", "fetch", "f", "auto-merge", "am", "log", "l"] {
            assert!(bash.contains(name), "bash overlay missing `{name}`");
        }
        assert!(bash.contains("complete -F _jj_gh_alias_wrapper_pr jj"));

        let zsh = emit_string(CompletionShell::Zsh);
        for name in ["create", "c", "fetch", "f", "auto-merge", "am", "log", "l"] {
            assert!(zsh.contains(name), "zsh overlay missing `{name}`");
        }
        assert!(zsh.contains("compdef _jj_gh_alias_pr jj"));

        let fish = emit_string(CompletionShell::Fish);
        for name in ["create", "c", "fetch", "f", "auto-merge", "am", "log", "l"] {
            assert!(
                fish.contains(&format!("-a '{name}'")),
                "fish overlay missing `-a '{name}'`"
            );
        }
        assert!(fish.contains("__jj_gh_alias_no_subcommand pr"));

        let nushell = emit_string(CompletionShell::Nushell);
        for name in ["create", "c", "fetch", "f", "auto-merge", "am", "log", "l"] {
            assert!(
                nushell.contains(&format!(r#"export extern "jj pr {name}""#)),
                "nushell overlay missing `{name}`"
            );
        }
    }

    #[test]
    fn unsupported_shell_errors_with_name() {
        let mut buf = Vec::<u8>::new();
        let err = alias_completions(
            CompletionShell::PowerShell,
            "pr",
            SubcommandStr::Pr,
            &mut buf,
        )
        .unwrap_err();
        assert!(err.to_string().contains("powershell"));
    }
}
