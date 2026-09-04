use crate::commands::completions::{ArgInfo, collect_subs, first_help_line};
use anyhow::Result;
use clap::Command;
use std::io::Write;

pub(super) fn emit<W: Write>(cmd: &Command, alias: &str, out: &mut W) -> Result<()> {
    writeln!(
        out,
        "# jj-gh: completion overlay for `jj {alias} <tab>` (do not edit)"
    )?;
    writeln!(out, "module completions {{")?;
    writeln!(out)?;

    for sub in collect_subs(cmd) {
        for name in std::iter::once(sub.name).chain(sub.aliases) {
            if let Some(about) = &sub.about {
                writeln!(out, "  # {}", first_help_line(about))?;
            }
            writeln!(out, r#"  export extern "jj {alias} {name}" ["#)?;
            for arg in &sub.args {
                writeln!(out, "    {}", arg_spec(arg))?;
            }
            writeln!(out, "  ]")?;
            writeln!(out)?;
        }
    }
    writeln!(out, "}}")?;
    writeln!(out)?;
    writeln!(out, "export use completions *")?;
    Ok(())
}

fn arg_spec(arg: &ArgInfo) -> String {
    let mut spec = match (arg.short, arg.long) {
        (Some(short), Some(long)) => format!("--{long}(-{short})"),
        (Some(short), None) => format!("-{short}"),
        (None, Some(long)) => format!("--{long}"),
        (None, None) => return String::new(),
    };
    if arg.takes_value {
        spec.push_str(": string");
    }
    if let Some(about) = &arg.about {
        spec.push_str(" # ");
        spec.push_str(&first_help_line(about));
    }
    spec
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::completions::fake_pr_command;

    #[test]
    fn emits_compound_externs_for_alias() {
        let mut out = Vec::new();
        emit(&fake_pr_command(), "pr", &mut out).unwrap();
        let output = String::from_utf8(out).unwrap();

        assert!(output.contains(r#"export extern "jj pr create" ["#));
        assert!(output.contains(r#"export extern "jj pr c" ["#));
        assert!(output.contains("--draft"));
        assert!(output.contains("--base: string"));
    }
}
