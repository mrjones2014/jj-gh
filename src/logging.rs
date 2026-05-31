//! CLI Logging

use anstyle::{AnsiColor, Color, Style};
use flexi_logger::{
    AdaptiveFormat, DeferredNow, FlexiLoggerError, LogSpecification, Logger, LoggerHandle,
};
use log::Record;
use std::fmt::Display;

pub use log::{LevelFilter, debug, error, info, warn};

const ENV_FILTER: &str = "JJ_GH_LOG";

pub trait ResultExt {
    #[must_use]
    fn log_err(self) -> Self;
}

impl<T, E> ResultExt for Result<T, E>
where
    E: Display,
{
    fn log_err(self) -> Self {
        if let Err(e) = &self {
            log::error!("{e}");
        }
        self
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
    let (tag, bg) = level_palette(record.level());
    let fg = if matches!(record.level(), log::Level::Warn) {
        AnsiColor::Black
    } else {
        AnsiColor::White
    };
    let tag_style = Style::new()
        .fg_color(Some(Color::Ansi(fg)))
        .bg_color(Some(Color::Ansi(bg)))
        .bold();
    let msg_style = Style::new().fg_color(Some(Color::Ansi(bg)));
    write!(
        w,
        "{}{tag}{} {}{}{}",
        tag_style.render(),
        tag_style.render_reset(),
        msg_style.render(),
        record.args(),
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
    write!(w, "{:5} {}", record.level(), record.args())
}
