use super::{ArgInfo, SubInfo, collect_subs};
use anyhow::Result;
use clap::Command;
use core::fmt::Write as _;
use std::io::Write;

pub(super) fn emit<W: Write>(cmd: &Command, alias: &str, out: &mut W) -> Result<()> {
    let subs = collect_subs(cmd);
    writeln!(
        out,
        "# jj-gh: completion overlay for `jj {alias} <tab>` (do not edit)\n\
         # Safe to source any time after bash-completion is initialised; the\n\
         # overlay triggers jj's own lazy completion load before snapshotting."
    )?;
    write!(out, "{}", wrapper(alias, &subs))?;
    Ok(())
}

fn wrapper(alias: &str, subs: &[SubInfo]) -> String {
    let sub_names = subs
        .iter()
        .flat_map(|s| {
            std::iter::once(s.name.to_string()).chain(s.aliases.iter().map(ToString::to_string))
        })
        .collect::<Vec<String>>();
    let sub_list = sub_names.join(" ");
    let per_sub_cases = build_cases(subs);

    format!(
        r#"if ! declare -F _jj_gh_alias_wrapper_{alias} >/dev/null 2>&1; then
    _jj_gh_alias_inner_{alias}() {{
        local cur sub
        cur="${{COMP_WORDS[COMP_CWORD]}}"
        sub="${{COMP_WORDS[2]:-}}"
        if [[ "$COMP_CWORD" -le 2 ]]; then
            COMPREPLY=( $(compgen -W "{sub_list}" -- "$cur") )
            return 0
        fi
        case "$sub" in
{per_sub_cases}        esac
        COMPREPLY=()
        return 0
    }}

    # nixpkgs `jujutsu` ships jj's bash completion at
    # share/bash-completion/completions/jj, which bash-completion only
    # sources on the first `jj <tab>`. Force-load it now so the snapshot
    # below finds a real handler instead of an empty `complete -p jj`.
    if ! complete -p jj >/dev/null 2>&1; then
        if declare -F _comp_load >/dev/null 2>&1; then
            _comp_load jj 2>/dev/null || true
        elif declare -F _completion_loader >/dev/null 2>&1; then
            _completion_loader jj 2>/dev/null || true
        fi
    fi

    # Capture the prior `jj` completer (jj's own, or another alias overlay
    # sourced earlier) so we can delegate when the user is not invoking
    # `jj {alias}`. Parse with awk so we only pick up the function name
    # after `-F`; any other `complete` form (`-W`, `-C`, ...) leaves the
    # variable empty and the delegate path is skipped.
    declare -g _jj_gh_alias_prior_{alias}
    _jj_gh_alias_prior_{alias}=$(complete -p jj 2>/dev/null \
        | awk '{{ for (i = 1; i < NF; i++) if ($i == "-F") {{ print $(i + 1); exit }} }}')

    _jj_gh_alias_wrapper_{alias}() {{
        if [[ "${{COMP_WORDS[1]:-}}" == "{alias}" ]]; then
            _jj_gh_alias_inner_{alias} "$@"
            return $?
        fi
        if [[ -n "$_jj_gh_alias_prior_{alias}" ]] \
            && [[ "$_jj_gh_alias_prior_{alias}" != "_jj_gh_alias_wrapper_{alias}" ]] \
            && declare -F "$_jj_gh_alias_prior_{alias}" >/dev/null 2>&1; then
            "$_jj_gh_alias_prior_{alias}" "$@"
            return $?
        fi
        COMPREPLY=()
    }}

    complete -F _jj_gh_alias_wrapper_{alias} jj
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
        let flags = flag_list(&sub.args);
        writeln!(
            out,
            r#"            {pattern})
                COMPREPLY=( $(compgen -W "{flags}" -- "$cur") )
                return 0
                ;;"#
        )
        .expect("writing to String never fails");
    }
    out
}

fn flag_list(args: &[ArgInfo]) -> String {
    let mut flags = Vec::<String>::new();
    for a in args {
        if let Some(long) = a.long {
            flags.push(format!("--{long}"));
        }
        if let Some(short) = a.short {
            flags.push(format!("-{short}"));
        }
    }
    flags.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::completions::fake_pr_command;

    #[test]
    fn wraps_and_chains() {
        let mut out = Vec::<u8>::new();
        emit(&fake_pr_command(), "pr", &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();

        assert!(s.contains("_jj_gh_alias_wrapper_pr"));
        assert!(s.contains("_jj_gh_alias_inner_pr"));
        assert!(s.contains("complete -F _jj_gh_alias_wrapper_pr jj"));
        assert!(s.contains("compgen -W \"create c fetch\""));
        assert!(s.contains("create|c)"));
        assert!(s.contains("--draft"));
        assert!(s.contains("--base"));
        assert!(s.contains("\"${COMP_WORDS[1]:-}\" == \"pr\""));
        assert!(s.contains("_jj_gh_alias_prior_pr"));
        assert!(s.contains("declare -g _jj_gh_alias_prior_pr"));
        assert!(s.contains("awk"));
        assert!(s.contains("_comp_load jj"));
        assert!(s.contains("_completion_loader jj"));
    }
}
