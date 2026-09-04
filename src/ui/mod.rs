//! Terminal UI helpers (spinners, hyperlinks etc.). Everything here writes to,
//! or decorates output bound for, a terminal; anything that has to survive a
//! pipe belongs on stdout as plain text.

mod links;
pub mod spinner;
pub mod tui;

pub use links::PrLinks;
pub use spinner::Spinner;

use std::io::IsTerminal;

const UNDERLINE: &str = "\x1b[4m";
const UNDERLINE_OFF: &str = "\x1b[24m";

/// Which stream a piece of output is headed for. Decoration is gated on the
/// stream so escape codes never reach a pipe: `jj-gh pr url 1 | pbcopy` has to
/// yield a bare URL.
#[derive(Debug, Clone, Copy)]
pub enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    fn is_terminal(self) -> bool {
        match self {
            Self::Stdout => std::io::stdout().is_terminal(),
            Self::Stderr => std::io::stderr().is_terminal(),
        }
    }

    /// Emit an ANSI code only when the stream is a terminal.
    #[must_use]
    pub fn on(self, code: &'static str) -> &'static str {
        if self.is_terminal() { code } else { "" }
    }

    /// Underline `text` to mark it as the clickable part of a line.
    ///
    /// Gated on hyperlink support rather than on being a terminal: most
    /// terminals only reveal an OSC 8 link on hover, so the underline is the
    /// only static cue that one is there, and it would be a lie without it.
    ///
    /// Turns underline back off rather than emitting a full reset, so this
    /// composes inside an already-colored span.
    #[must_use]
    pub fn underline(self, text: &str) -> String {
        if self.hyperlinks_supported() {
            format!("{UNDERLINE}{text}{UNDERLINE_OFF}")
        } else {
            text.to_string()
        }
    }

    /// Whether OSC 8 hyperlinks render on this stream.
    ///
    /// Narrower than [`Stream::on`]: color support is near-universal, but
    /// `screen` mangles OSC 8 and `TERM=dumb` shows it verbatim.
    /// `supports_hyperlinks::on` lets `FORCE_HYPERLINK` override the TTY check,
    /// which would put escapes into a pipe, so the TTY check is repeated here.
    fn hyperlinks_supported(self) -> bool {
        self.is_terminal()
            && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
            && supports_hyperlinks::on(match self {
                Self::Stdout => supports_hyperlinks::Stream::Stdout,
                Self::Stderr => supports_hyperlinks::Stream::Stderr,
            })
    }
}

pub trait Hyperlink {
    /// Render as an OSC 8 terminal hyperlink pointing at `url`, or unchanged
    /// when `stream` cannot render one.
    fn hyperlink(&self, stream: Stream, url: impl AsRef<str>) -> String;
}

impl<T: AsRef<str>> Hyperlink for T {
    fn hyperlink(&self, stream: Stream, url: impl AsRef<str>) -> String {
        render_hyperlink(self.as_ref(), url.as_ref(), stream.hyperlinks_supported())
    }
}

/// Print a PR URL as the command's result: its own line on stdout, hyperlinked
/// when the terminal allows. A pipe gets the bare URL, which is the point of
/// keeping it on stdout at all (`jj-gh pr url 1 | pbcopy`).
#[inline]
pub fn print_url(url: &str) {
    let url = url.trim();
    println!("{}", url.hyperlink(Stream::Stdout, url));
}

fn render_hyperlink(text: &str, url: &str, supported: bool) -> String {
    if supported {
        format!("\u{1b}]8;;{url}\u{1b}\\{text}\u{1b}]8;;\u{1b}\\")
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_terminals_get_the_bare_text() {
        assert_eq!(render_hyperlink("#12", "https://x/12", false), "#12");
    }

    #[test]
    fn underline_ends_with_underline_off_not_a_full_reset() {
        // A full reset here would drop the color of the span this sits inside.
        assert_eq!(
            format!("{UNDERLINE}#12{UNDERLINE_OFF}"),
            "\u{1b}[4m#12\u{1b}[24m"
        );
    }

    #[test]
    fn supported_terminals_get_osc_8_around_the_text() {
        assert_eq!(
            render_hyperlink("#12", "https://x/12", true),
            "\u{1b}]8;;https://x/12\u{1b}\\#12\u{1b}]8;;\u{1b}\\"
        );
    }
}
