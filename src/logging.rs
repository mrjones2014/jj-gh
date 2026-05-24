//! CLI Logging

use flexi_logger::{
    AdaptiveFormat, DeferredNow, FlexiLoggerError, LogSpecification, Logger, LoggerHandle,
};
use log::Record;
use nu_ansi_term::{Color, Style};

pub use log::{LevelFilter, debug, error, info, warn};

const ENV_FILTER: &str = "JJ_GH_LOG";

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

fn level_palette(level: log::Level) -> (&'static str, Color) {
    match level {
        log::Level::Error => (" ERROR ", Color::Red),
        log::Level::Warn => (" WARN  ", Color::Yellow),
        log::Level::Info => (" INFO  ", Color::Blue),
        log::Level::Debug => (" DEBUG ", Color::Magenta),
        log::Level::Trace => (" TRACE ", Color::DarkGray),
    }
}

fn pretty_format(
    w: &mut dyn std::io::Write,
    _now: &mut DeferredNow,
    record: &Record,
) -> std::io::Result<()> {
    let (tag, color) = level_palette(record.level());
    let tag_fg = if matches!(record.level(), log::Level::Warn) {
        Color::Black
    } else {
        Color::White
    };
    let tag_style = tag_fg.on(color).bold();
    let msg_style = color.normal();
    write!(
        w,
        "{} {}",
        tag_style.paint(tag),
        msg_style.paint(format!("{}", record.args()))
    )?;
    if matches!(record.level(), log::Level::Debug | log::Level::Trace)
        && let Some(m) = record.module_path()
    {
        write!(w, " {}", Style::new().dimmed().paint(format!("({m})")))?;
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
