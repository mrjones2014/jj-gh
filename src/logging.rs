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

const CRATE_MODULE: &str = env!("CARGO_CRATE_NAME");

/// Log levels split by origin. Dependencies (`octocrab`, `rustls`, `hyper`,
/// etc.) are far chattier than they are useful, so they get their own level
/// rather than riding along with ours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogLevels {
    /// Level for `jj_gh::*`.
    pub own: LevelFilter,
    /// Level for every other crate.
    pub dependencies: LevelFilter,
}

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
/// `JJ_GH_LOG` bypasses [`LogLevels`] entirely and takes the spec verbatim, so
/// `JJ_GH_LOG=debug` still shows dependency output.
///
/// # Errors
///
/// Returns a `FlexiLoggerError` if the `JJ_GH_LOG` env spec is invalid or the
/// logger backend fails to start.
pub fn init(levels: LogLevels) -> Result<LoggerHandle, FlexiLoggerError> {
    let spec = if let Ok(filter) = std::env::var(ENV_FILTER)
        && !filter.is_empty()
    {
        LogSpecification::parse(&filter)?
    } else {
        spec_for(levels)
    };

    Logger::with(spec)
        .log_to_stderr()
        .adaptive_format_for_stderr(AdaptiveFormat::Custom(plain_format, pretty_format))
        .start()
}

/// A spec that filters `jj_gh::*` and everything else independently.
fn spec_for(LogLevels { own, dependencies }: LogLevels) -> LogSpecification {
    LogSpecification::builder()
        .default(dependencies)
        .module(CRATE_MODULE, own)
        .build()
}

const fn level_palette(level: log::Level) -> (&'static str, AnsiColor) {
    match level {
        log::Level::Error => ("ERROR", AnsiColor::Red),
        log::Level::Warn => ("WARN", AnsiColor::Yellow),
        log::Level::Info => ("INFO", AnsiColor::Blue),
        log::Level::Debug => ("DEBUG", AnsiColor::Magenta),
        log::Level::Trace => ("TRACE", AnsiColor::BrightBlack),
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
    use super::*;
    use log::Level;

    #[test]
    fn crate_module_matches_what_log_reports() {
        // `module_path!()` here is `jj_gh::logging::tests`, so this fails loudly
        // if `CARGO_CRATE_NAME` ever stops lining up with the logged prefix.
        assert!(
            module_path!().starts_with(CRATE_MODULE),
            "module_path!() `{}` does not start with CRATE_MODULE `{CRATE_MODULE}`",
            module_path!(),
        );
        assert!(
            spec_for(LogLevels {
                own: LevelFilter::Debug,
                dependencies: LevelFilter::Off,
            })
            .enabled(Level::Debug, module_path!())
        );
    }

    #[test]
    fn own_modules_log_while_dependencies_stay_silent() {
        let spec = spec_for(LogLevels {
            own: LevelFilter::Debug,
            dependencies: LevelFilter::Off,
        });

        assert!(spec.enabled(Level::Debug, "jj_gh"));
        assert!(spec.enabled(Level::Debug, "jj_gh::jj::real"));
        assert!(spec.enabled(Level::Error, "jj_gh::gh::real"));

        for dependency in ["octocrab", "rustls::client", "hyper_util", "mio::poll"] {
            assert!(!spec.enabled(Level::Error, dependency));
            assert!(!spec.enabled(Level::Trace, dependency));
        }
    }

    #[test]
    fn own_level_still_bounds_our_own_modules() {
        let spec = spec_for(LogLevels {
            own: LevelFilter::Info,
            dependencies: LevelFilter::Off,
        });

        assert!(spec.enabled(Level::Info, "jj_gh::jj"));
        assert!(!spec.enabled(Level::Debug, "jj_gh::jj"));
    }

    #[test]
    fn trace_dependencies_let_everything_through() {
        let spec = spec_for(LogLevels {
            own: LevelFilter::Trace,
            dependencies: LevelFilter::Trace,
        });

        assert!(spec.enabled(Level::Trace, "jj_gh::jj"));
        assert!(spec.enabled(Level::Trace, "octocrab"));
    }

    #[test]
    fn quiet_own_level_is_not_clamped_by_silent_dependencies() {
        let spec = spec_for(LogLevels {
            own: LevelFilter::Error,
            dependencies: LevelFilter::Off,
        });

        assert!(spec.enabled(Level::Error, "jj_gh"));
        assert!(!spec.enabled(Level::Warn, "jj_gh"));
        assert!(!spec.enabled(Level::Error, "octocrab"));
    }

    #[test]
    fn continuation_lines_align_after_prefix() {
        assert_eq!(
            indent_continuations("first\nsecond\nthird", 6),
            "first\n      second\n      third"
        );
    }
}
