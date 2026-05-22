//! CLI Logging

use crate::cli::GlobalOpts;
use flexi_logger::{FlexiLoggerError, LogSpecification, Logger, LoggerHandle};
use log::LevelFilter;
use std::io::IsTerminal;

const ENV_FILTER: &str = "JJ_GH_LOG";

/// Initialize the global logger. Holding onto the returned handle keeps the
/// logger alive for the duration of the program.
pub fn init(opts: &GlobalOpts) -> Result<LoggerHandle, FlexiLoggerError> {
    let spec = match std::env::var(ENV_FILTER) {
        Ok(filter) if !filter.is_empty() => LogSpecification::parse(&filter)?,
        _ => LogSpecification::builder()
            .default(resolve_level(opts))
            .build(),
    };

    Logger::with(spec)
        .log_to_stderr()
        .format(flexi_logger::colored_default_format)
        .start()
}

fn resolve_level(opts: &GlobalOpts) -> LevelFilter {
    if let Some(level) = opts.log_level {
        return level;
    }

    if opts.quiet {
        return LevelFilter::Error;
    }

    let base = if std::io::stdout().is_terminal() {
        LevelFilter::Info
    } else {
        LevelFilter::Error
    };

    match opts.verbose {
        0 => base,
        1 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    }
}
