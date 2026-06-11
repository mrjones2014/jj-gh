use super::{ArgInfo, collect_subs, first_help_line};
use anyhow::Result;
use clap::Command;
use std::io::Write;

pub(super) fn emit<W: Write>(cmd: &Command, alias: &str, out: &mut W) -> Result<()> {
    let subs = collect_subs(cmd);
    writeln!(
        out,
        "# jj-gh: completion overlay for `jj {alias} <tab>` (do not edit)\n\
         # Source *after* `jj util completion fish`."
    )?;
    writeln!(out, "{}", helpers())?;

    for sub in &subs {
        let names: Vec<&str> = std::iter::once(sub.name)
            .chain(sub.aliases.iter().copied())
            .collect();
        for n in &names {
            let line = match &sub.about {
                Some(about) => format!(
                    "complete -c jj -n '__jj_gh_alias_no_subcommand {alias}' -f -a '{n}' -d '{}'",
                    escape(&first_help_line(about))
                ),
                None => {
                    format!("complete -c jj -n '__jj_gh_alias_no_subcommand {alias}' -f -a '{n}'")
                }
            };
            writeln!(out, "{line}")?;
        }
        let pred = sub_predicate(alias, &names);
        for arg in &sub.args {
            writeln!(out, "{}", arg_line(&pred, arg))?;
        }
    }
    Ok(())
}

fn helpers() -> &'static str {
    // Guarded so multiple sourcings (e.g. across nested fish shells, or
    // overlays for several aliases) are idempotent. `commandline -opc`
    // gives the tokens before the cursor; `$cmd[1]` is `jj`,
    // `$cmd[2]` should equal the alias name we were emitted for.
    "if not functions -q __jj_gh_alias_no_subcommand
    function __jj_gh_alias_no_subcommand -a alias
        set -l cmd (commandline -opc)
        test (count $cmd) -eq 2
        and test \"$cmd[2]\" = $alias
    end
    function __jj_gh_alias_at_subcommand
        set -l alias $argv[1]
        set -l names $argv[2..]
        set -l cmd (commandline -opc)
        test (count $cmd) -ge 3
        and test \"$cmd[2]\" = $alias
        and contains -- \"$cmd[3]\" $names
    end
end"
}

fn sub_predicate(alias: &str, names: &[&str]) -> String {
    let names_str = names.join(" ");
    format!("__jj_gh_alias_at_subcommand {alias} {names_str}")
}

fn arg_line(predicate: &str, arg: &ArgInfo) -> String {
    let mut parts = vec![format!("complete -c jj -n '{predicate}'")];
    if let Some(short) = arg.short {
        parts.push(format!("-s {short}"));
    }
    if let Some(long) = arg.long {
        parts.push(format!("-l {long}"));
    }
    if arg.takes_value {
        parts.push("-r".into());
    } else {
        parts.push("-f".into());
    }
    if let Some(about) = &arg.about {
        parts.push(format!("-d '{}'", escape(&first_help_line(about))));
    }
    parts.join(" ")
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::completions::fake_pr_command;

    #[test]
    fn registers_against_jj_and_gates_on_alias() {
        let mut out: Vec<u8> = Vec::new();
        emit(&fake_pr_command(), "pr", &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();

        assert!(s.contains("function __jj_gh_alias_no_subcommand"));
        assert!(s.contains("complete -c jj -n '__jj_gh_alias_no_subcommand pr' -f -a 'create'"));
        assert!(s.contains("complete -c jj -n '__jj_gh_alias_no_subcommand pr' -f -a 'c'"));
        assert!(s.contains("complete -c jj -n '__jj_gh_alias_no_subcommand pr' -f -a 'fetch'"));
        assert!(s.contains("__jj_gh_alias_at_subcommand pr create c"));
        assert!(s.contains("-l draft"));
        assert!(s.contains("-l base"));

        let base_line = s
            .lines()
            .find(|l| l.contains("-l base"))
            .expect("base line");
        assert!(base_line.contains(" -r"));
        let draft_line = s
            .lines()
            .find(|l| l.contains("-l draft") && !l.contains("no-draft"))
            .expect("draft line");
        assert!(draft_line.contains(" -f"));
    }
}
