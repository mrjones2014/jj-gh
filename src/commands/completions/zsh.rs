use super::{ArgInfo, SubInfo, collect_subs, first_help_line};
use anyhow::Result;
use clap::Command;
use core::fmt::Write as _;
use std::io::Write;

pub(super) fn emit<W: Write>(cmd: &Command, alias: &str, out: &mut W) -> Result<()> {
    let subs = collect_subs(cmd);
    writeln!(
        out,
        "# jj-gh: completion overlay for `jj {alias} <tab>` (do not edit)\n\
         # Safe to source any time after `compinit`; the overlay forces an\n\
         # autoload of `_jj` if compinit hasn't registered it yet."
    )?;
    write!(out, "{}", wrapper(alias, &subs))?;
    Ok(())
}

fn wrapper(alias: &str, subs: &[SubInfo]) -> String {
    let mut sub_lines = Vec::<String>::new();
    for sub in subs {
        let desc = sub
            .about
            .as_deref()
            .map(first_help_line)
            .unwrap_or_default();
        for n in std::iter::once(sub.name).chain(sub.aliases.iter().copied()) {
            sub_lines.push(format!("            '{n}:{}'", escape(&desc)));
        }
    }
    let sub_block = sub_lines.join(" \\\n");
    let per_sub_cases = build_cases(subs);

    format!(
        r#"if (( ! ${{+functions[_jj_gh_alias_{alias}]}} )); then
    _jj_gh_alias_{alias}_inner() {{
        if (( CURRENT <= 3 )); then
            _values "subcommand" \
{sub_block}
            return
        fi
        local sub="${{words[3]}}"
        case "$sub" in
{per_sub_cases}        esac
    }}

    # nixpkgs `jujutsu` drops `_jj` into `$fpath` via
    # share/zsh/site-functions; compinit registers `_comps[jj]=_jj` from
    # the `#compdef jj` header. If compinit hasn't run yet (rare, but
    # possible if the user sources this from a non-interactive context),
    # force-resolve `_jj` so the snapshot below picks it up.
    if (( ! ${{+_comps[jj]}} )); then
        autoload -Uz +X _jj 2>/dev/null
    fi

    # Capture the prior `jj` completer (jj's own, or another alias overlay
    # sourced earlier) so we can delegate when not handling our alias.
    if (( ${{+_comps[jj]}} )); then
        functions[_jj_gh_alias_prior_{alias}]=$functions[${{_comps[jj]}}]
    fi

    _jj_gh_alias_{alias}() {{
        if [[ "${{words[2]:-}}" == "{alias}" ]]; then
            _jj_gh_alias_{alias}_inner
            return
        fi
        if (( ${{+functions[_jj_gh_alias_prior_{alias}]}} )); then
            _jj_gh_alias_prior_{alias}
        fi
    }}
    compdef _jj_gh_alias_{alias} jj
fi
"#
    )
}

fn build_cases(subs: &[SubInfo]) -> String {
    let mut out = String::new();
    for sub in subs {
        let names = std::iter::once(sub.name)
            .chain(sub.aliases.iter().copied())
            .collect::<Vec<&str>>();
        let pattern = names.join("|");
        let args = arg_spec_lines(&sub.args);
        if args.is_empty() {
            #[expect(clippy::needless_raw_string_hashes)]
            writeln!(
                out,
                r#"            ({pattern})
                _arguments
                ;;"#
            )
            .expect("writing to String never fails");
        } else {
            #[expect(clippy::needless_raw_string_hashes)]
            writeln!(
                out,
                r#"            ({pattern})
                _arguments \
{args}
                ;;"#
            )
            .expect("writing to String never fails");
        }
    }
    out
}

fn arg_spec_lines(args: &[ArgInfo]) -> String {
    let mut lines = Vec::<String>::new();
    for a in args {
        let desc = a.about.as_deref().map(first_help_line).unwrap_or_default();
        let value_suffix = if a.takes_value { "=" } else { "" };
        // `_arguments` syntax:
        //   long-only/short-only: `'--flag[desc]'`
        //   both:                 `'(-s --long)'{-s,--long}'[desc]'`
        // (the brace expansion is unquoted; each surrounding piece carries
        // its own quoting.)
        let spec = match (a.short, a.long) {
            (Some(s), Some(l)) => format!(
                "'(-{s} --{l})'{{-{s},--{l}{value_suffix}}}'[{}]'",
                escape(&desc)
            ),
            (Some(s), None) => format!("'-{s}{value_suffix}[{}]'", escape(&desc)),
            (None, Some(l)) => format!("'--{l}{value_suffix}[{}]'", escape(&desc)),
            (None, None) => continue,
        };
        lines.push(format!("                    {spec}"));
    }
    lines.join(" \\\n")
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::completions::fake_pr_command;

    #[test]
    fn compdef_and_chains() {
        let mut out = Vec::<u8>::new();
        emit(&fake_pr_command(), "pr", &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();

        assert!(s.contains("_jj_gh_alias_pr"));
        assert!(s.contains("compdef _jj_gh_alias_pr jj"));
        // clap strips the trailing period from doc-comment short help.
        assert!(s.contains("'create:Create a thing'"));
        assert!(s.contains("'c:Create a thing'"));
        assert!(s.contains("'fetch:Fetch a thing'"));
        assert!(s.contains("(create|c)"));
        assert!(s.contains("--draft"));
        assert!(s.contains("--base="));
        assert!(s.contains("_jj_gh_alias_prior_pr"));
        assert!(s.contains("autoload -Uz +X _jj"));
    }
}
