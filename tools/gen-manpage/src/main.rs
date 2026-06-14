use clap::{Command, CommandFactory};
use clap_mangen::{Man, roff::Roff};
use jj_gh::Cli;
use std::io::{self, Write};

fn main() -> io::Result<()> {
    render_manpage(&mut io::stdout().lock())
}

fn render_manpage(out: &mut dyn Write) -> io::Result<()> {
    let mut command = Cli::command().disable_help_subcommand(true);
    command.build();

    let man = Man::new(command.clone());
    man.render_title(out)?;
    man.render_name_section(out)?;
    man.render_synopsis_section(out)?;
    man.render_description_section(out)?;
    man.render_options_section(out)?;
    if has_extra(&command) {
        man.render_extra_section(out)?;
    }

    for subcommand in command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
    {
        render_command(out, subcommand)?;
    }

    man.render_version_section(out)
}

fn render_command(out: &mut dyn Write, command: &Command) -> io::Result<()> {
    let heading = command
        .get_bin_name()
        .unwrap_or_else(|| command.get_name())
        .to_uppercase();
    let mut roff = Roff::default();
    roff.control("SH", [heading.as_str()]);
    roff.to_writer(&mut *out)?;

    let man = Man::new(command.clone());
    render_subsection(out, |fragment| man.render_synopsis_section(fragment))?;
    if command.get_about().is_some() || command.get_long_about().is_some() {
        render_subsection(out, |fragment| man.render_description_section(fragment))?;
    }
    if command
        .get_arguments()
        .any(|argument| !argument.is_hide_set())
    {
        render_subsection(out, |fragment| man.render_options_section(fragment))?;
    }
    if has_extra(command) {
        render_subsection(out, |fragment| man.render_extra_section(fragment))?;
    }

    for subcommand in command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
    {
        render_command(out, subcommand)?;
    }

    Ok(())
}

fn has_extra(command: &Command) -> bool {
    command.get_after_help().is_some() || command.get_after_long_help().is_some()
}

fn render_subsection(
    out: &mut dyn Write,
    render: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    let mut fragment = Vec::new();
    render(&mut fragment)?;
    let fragment = String::from_utf8(fragment).expect("clap_mangen emits UTF-8");
    out.write_all(fragment.replace(".SH ", ".SS ").as_bytes())
}
