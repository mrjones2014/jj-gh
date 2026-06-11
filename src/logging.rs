//! CLI Logging

use anstyle::{AnsiColor, Color, Style};
use flexi_logger::{
    AdaptiveFormat, DeferredNow, FlexiLoggerError, LogSpecification, Logger, LoggerHandle,
};
use log::Record;
use std::{
    fmt::Display,
    io::{IsTerminal, Write as _},
};

pub use log::{LevelFilter, debug, error, info, warn};

const ENV_FILTER: &str = "JJ_GH_LOG";

/// Print a fatal error directly, bypassing log filters and logger startup.
pub fn fatal(error: impl Display) {
    let mut stderr = std::io::stderr().lock();
    if std::io::stderr().is_terminal() {
        let message = indent_continuations(&error.to_string(), 8);
        let (tag, color) = level_palette(log::Level::Error);
        let tag_style = Style::new().fg_color(Some(Color::Ansi(color))).bold();
        let msg_style = Style::new().fg_color(Some(Color::Ansi(color)));
        let _ = writeln!(
            stderr,
            "{}{tag}{} {}{message}{}",
            tag_style.render(),
            tag_style.render_reset(),
            msg_style.render(),
            msg_style.render_reset(),
        );
    } else {
        let message = indent_continuations(&error.to_string(), 6);
        let _ = writeln!(stderr, "ERROR {message}");
    }
}

/// Initialize the global logger. Holding onto the returned handle keeps the
/// logger alive for the duration of the program.
///
/// # Errors
///
/// Returns a `FlexiLoggerError` if the `JJ_GH_LOG` env spec is invalid or the
/// logger backend fails to start.
pub fn init(level: LevelFilter) -> Result<LoggerHandle, FlexiLoggerError> {
    let spec = match std::env::var(ENV_FILTER) {
        Ok(filter) if !filter.is_empty() => LogSpecification::parse(&filter)?,
        _ => LogSpecification::builder().default(level).build(),
    };

    Logger::with(spec)
        .log_to_stderr()
        .adaptive_format_for_stderr(AdaptiveFormat::Custom(plain_format, pretty_format))
        .start()
}

fn level_palette(level: log::Level) -> (&'static str, AnsiColor) {
    match level {
        log::Level::Error => (" ERROR ", AnsiColor::Red),
        log::Level::Warn => (" WARN  ", AnsiColor::Yellow),
        log::Level::Info => (" INFO  ", AnsiColor::Blue),
        log::Level::Debug => (" DEBUG ", AnsiColor::Magenta),
        log::Level::Trace => (" TRACE ", AnsiColor::BrightBlack),
    }
}

fn pretty_format(
    w: &mut dyn std::io::Write,
    _now: &mut DeferredNow,
    record: &Record,
) -> std::io::Result<()> {
    let (tag, color) = level_palette(record.level());
    let message = indent_continuations(&record.args().to_string(), 8);
    let tag_style = Style::new().fg_color(Some(Color::Ansi(color))).bold();
    let msg_style = Style::new().fg_color(Some(Color::Ansi(color)));
    write!(
        w,
        "{}{tag}{} {}{}{}",
        tag_style.render(),
        tag_style.render_reset(),
        msg_style.render(),
        message,
        msg_style.render_reset(),
    )?;
    if matches!(record.level(), log::Level::Debug | log::Level::Trace)
        && let Some(m) = record.module_path()
    {
        let dim = Style::new().dimmed();
        write!(w, " {}({m}){}", dim.render(), dim.render_reset())?;
    }
    Ok(())
}

fn plain_format(
    w: &mut dyn std::io::Write,
    _now: &mut DeferredNow,
    record: &Record,
) -> std::io::Result<()> {
    write!(
        w,
        "{:5} {}",
        record.level(),
        indent_continuations(&record.args().to_string(), 6)
    )
}

fn indent_continuations(message: &str, width: usize) -> String {
    message.replace('\n', &format!("\n{}", " ".repeat(width)))
}

#[cfg(test)]
mod tests {
    use super::indent_continuations;

    #[test]
    fn continuation_lines_align_after_prefix() {
        assert_eq!(
            indent_continuations("first\nsecond\nthird", 6),
            "first\n      second\n      third"
        );
    }
}
