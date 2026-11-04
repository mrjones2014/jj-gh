//! Terminal UI helpers (status line, hyperlinks etc.). Everything here writes
//! to, or decorates output bound for, a terminal; anything that has to survive
//! a pipe belongs on stdout as plain text.

mod links;
pub mod status;
pub mod tui;

pub use links::PrLinks;

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
    /// Emitted by default on a terminal: a VT-conformant emulator that does
    /// not support OSC 8 consumes the sequence invisibly, so there is no
    /// allowlist of known-good terminals to maintain. The gate only covers
    /// the cases where emitting would do real harm, plus honesty for the
    /// underline cue (see [`Stream::underline`]):
    ///
    /// - not a terminal: a pipe must get plain text (`jj-gh pr url 1 |
    ///   pbcopy`), which is also why `FORCE_HYPERLINK` is honored only on a
    ///   TTY;
    /// - `NO_COLOR`: links imply underline decoration;
    /// - the known-broken environments listed in [`hyperlinks_safe`].
    fn hyperlinks_supported(self) -> bool {
        if !self.is_terminal() || std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return false;
        }
        if let Ok(force) = std::env::var("FORCE_HYPERLINK") {
            return force.trim() != "0";
        }
        let var = |name| std::env::var(name).ok();
        hyperlinks_safe(
            &var("TERM").unwrap_or_default(),
            std::env::var_os("TMUX").is_some_and(|value| !value.is_empty()),
            var("TERM_PROGRAM_VERSION").as_deref(),
            var("VTE_VERSION").as_deref(),
        )
    }
}

/// The blocklist behind [`Stream::hyperlinks_supported`]: environments where
/// OSC 8 is known to misbehave rather than being invisibly ignored.
///
/// - `TERM=dumb` does not parse escapes at all and shows the bytes verbatim;
/// - tmux consumes OSC 8 itself but only renders it since 3.4, so on older
///   tmux the underline would promise a link that cannot work (tmux has set
///   `TERM_PROGRAM_VERSION` since 3.2; absent means older still). Checked
///   before the `screen` prefix because tmux is often configured with
///   `TERM=screen-256color`;
/// - GNU screen mangles OSC 8 rather than ignoring it;
/// - VTE before 0.50 crashed on OSC 8 (the bug that spawned the various
///   terminal-detection crates in the first place).
fn hyperlinks_safe(
    term: &str,
    tmux: bool,
    tmux_version: Option<&str>,
    vte_version: Option<&str>,
) -> bool {
    if term == "dumb" {
        return false;
    }
    if tmux {
        return tmux_version.is_some_and(tmux_version_has_hyperlinks);
    }
    if term.starts_with("screen") {
        return false;
    }
    if let Some(vte) = vte_version {
        return vte.parse::<u32>().unwrap_or(0) >= 5000;
    }
    true
}

/// Whether a tmux version string ("3.4", "3.5a", "next-3.6") is at least 3.4,
/// the release that added OSC 8 support.
fn tmux_version_has_hyperlinks(version: &str) -> bool {
    let version = version.trim_start_matches("next-");
    let mut parts = version.split('.');
    let Some(major) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
        return false;
    };
    let minor = parts
        .next()
        .map(|s| s.trim_end_matches(|c: char| !c.is_ascii_digit()))
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    major > 3 || (major == 3 && minor >= 4)
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
    status::println_stdout(&url.hyperlink(Stream::Stdout, url));
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

    #[test]
    fn hyperlinks_default_on_with_a_short_blocklist() {
        // Unknown terminals are fine: unsupported OSC 8 is invisibly ignored.
        assert!(hyperlinks_safe("xterm-256color", false, None, None));
        assert!(hyperlinks_safe("made-up-term", false, None, None));
        // Dumb terminals print the bytes verbatim.
        assert!(!hyperlinks_safe("dumb", false, None, None));
        // GNU screen mangles OSC 8.
        assert!(!hyperlinks_safe("screen-256color", false, None, None));
        // tmux wins over its legacy screen-flavored TERM, by version.
        assert!(hyperlinks_safe("screen-256color", true, Some("3.8"), None));
        assert!(!hyperlinks_safe("tmux-256color", true, Some("3.3a"), None));
        assert!(!hyperlinks_safe("tmux-256color", true, None, None));
        // VTE crashed on OSC 8 before 0.50.
        assert!(!hyperlinks_safe(
            "xterm-256color",
            false,
            None,
            Some("4903")
        ));
        assert!(hyperlinks_safe("xterm-256color", false, None, Some("6003")));
    }

    #[test]
    fn tmux_hyperlink_support_starts_at_3_4() {
        assert!(tmux_version_has_hyperlinks("3.4"));
        assert!(tmux_version_has_hyperlinks("3.5a"));
        assert!(tmux_version_has_hyperlinks("3.8"));
        assert!(tmux_version_has_hyperlinks("next-3.6"));
        assert!(tmux_version_has_hyperlinks("4.0"));
        assert!(!tmux_version_has_hyperlinks("3.3a"));
        assert!(!tmux_version_has_hyperlinks("2.9"));
        assert!(!tmux_version_has_hyperlinks("garbage"));
    }
}
