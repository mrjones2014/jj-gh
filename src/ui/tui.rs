//! Shared crossterm helpers for interactive terminal UIs.

use anyhow::{Context, Result, bail};
use crossterm::{cursor, execute, queue, terminal};
use std::io::{IsTerminal, Stdout, Write};

pub const HIGHLIGHT_BG: &str = "\x1b[48;5;236m";
pub const HORIZONTAL_SEPARATOR: &str = "─";
pub const VERTICAL_SEPARATOR: &str = "│";
const ANSI_CSI: &str = "\x1b[";
const ANSI_RESET: &str = "\x1b[0m";
const ELLIPSIS: char = '…';

/// Require an interactive terminal on both stdin and stdout.
pub fn require_tty(feature: &str) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("{feature} requires an interactive terminal");
    }
    Ok(())
}

/// Raw-mode inline terminal region restored automatically on drop.
pub struct InlineSession {
    rows: u16,
    frame_started: bool,
}

impl InlineSession {
    pub fn enter(requested_rows: usize) -> Result<Self> {
        let terminal_rows = terminal::size().context("reading terminal size")?.1;
        let rows = u16::try_from(requested_rows).unwrap_or(u16::MAX).max(1);
        if rows > terminal_rows {
            bail!("terminal is too short for interactive UI");
        }
        terminal::enable_raw_mode().context("enabling raw mode")?;
        let mut out = std::io::stdout();
        execute!(out, cursor::Hide).context("hiding cursor")?;
        for _ in 1..rows {
            write!(out, "\r\n")?;
        }
        execute!(
            out,
            cursor::MoveUp(rows.saturating_sub(1)),
            cursor::MoveToColumn(0)
        )?;
        Ok(Self {
            rows,
            frame_started: false,
        })
    }

    #[must_use]
    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn begin_frame(&mut self, out: &mut Stdout) -> Result<()> {
        if self.frame_started {
            queue!(out, cursor::MoveUp(self.rows.saturating_sub(1)))?;
        }
        for row in 0..self.rows {
            queue!(
                out,
                cursor::MoveToColumn(0),
                terminal::Clear(terminal::ClearType::CurrentLine),
            )?;
            if row + 1 < self.rows {
                queue!(out, cursor::MoveDown(1))?;
            }
        }
        queue!(
            out,
            cursor::MoveUp(self.rows.saturating_sub(1)),
            cursor::MoveToColumn(0),
        )?;
        self.frame_started = true;
        Ok(())
    }
}

impl Drop for InlineSession {
    fn drop(&mut self) {
        let mut out = std::io::stdout();
        if self.frame_started {
            let _ = execute!(out, cursor::MoveUp(self.rows.saturating_sub(1)));
        }
        let _ = execute!(out, cursor::MoveToColumn(0));
        for row in 0..self.rows {
            let _ = execute!(out, terminal::Clear(terminal::ClearType::CurrentLine));
            if row + 1 < self.rows {
                let _ = execute!(out, cursor::MoveDown(1));
            }
        }
        let _ = execute!(
            out,
            cursor::MoveUp(self.rows.saturating_sub(1)),
            cursor::Show
        );
        let _ = terminal::disable_raw_mode();
    }
}

#[must_use]
pub fn bounded_region_rows(
    content_rows: usize,
    footer_rows: u16,
    max_content_rows: usize,
) -> usize {
    let terminal_rows = terminal::size().ok().map(|(_, rows)| usize::from(rows));
    calculate_region_rows(
        content_rows,
        usize::from(footer_rows),
        max_content_rows,
        terminal_rows,
    )
}

fn calculate_region_rows(
    content_rows: usize,
    footer_rows: usize,
    max_content_rows: usize,
    terminal_rows: Option<usize>,
) -> usize {
    let available_content =
        terminal_rows.map_or(max_content_rows, |rows| rows.saturating_sub(footer_rows));
    content_rows
        .min(max_content_rows)
        .min(available_content)
        .max(1)
        + footer_rows
}

/// Cursor and viewport state shared by selectable lists.
#[derive(Debug, Default)]
pub struct ListState {
    pub selected: usize,
    pub scroll_offset: usize,
}

impl ListState {
    pub fn move_by(&mut self, delta: isize, len: usize, visible_rows: usize) {
        self.selected = move_index(self.selected, delta, len);
        ensure_visible(self.selected, &mut self.scroll_offset, visible_rows);
    }

    pub fn move_to(&mut self, idx: usize, len: usize, visible_rows: usize) {
        self.selected = idx.min(len.saturating_sub(1));
        ensure_visible(self.selected, &mut self.scroll_offset, visible_rows);
    }
}

#[must_use]
pub fn move_index(current: usize, delta: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let len = i64::try_from(len).unwrap_or(i64::MAX);
    let delta = i64::try_from(delta).expect("cursor delta out of range");
    let next = (i64::try_from(current).unwrap_or(0) + delta).clamp(0, len - 1);
    usize::try_from(next).unwrap_or(0)
}

pub fn ensure_visible(selected: usize, scroll_offset: &mut usize, visible_rows: usize) {
    let visible_rows = visible_rows.max(1);
    if selected < *scroll_offset {
        *scroll_offset = selected;
    } else if selected >= *scroll_offset + visible_rows {
        *scroll_offset = selected + 1 - visible_rows;
    }
}

#[must_use]
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out = s.chars().take(max.saturating_sub(1)).collect::<String>();
    out.push(ELLIPSIS);
    out
}

/// Apply a background highlight that survives ANSI reset sequences.
#[must_use]
pub fn apply_bg_highlight(line: &str, bg_code: &str) -> (String, usize) {
    let mut out = String::with_capacity(line.len() + bg_code.len() * 4);
    out.push_str(bg_code);
    let mut visible = 0usize;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            let mut esc = String::from(ANSI_CSI);
            chars.next();
            for ch in chars.by_ref() {
                esc.push(ch);
                if ch.is_ascii_alphabetic() {
                    break;
                }
            }
            out.push_str(&esc);
            if esc.ends_with('m') {
                let body = &esc[2..esc.len() - 1];
                let is_reset = body.is_empty() || body.split(';').any(|p| p == "0" || p == "00");
                if is_reset {
                    out.push_str(bg_code);
                }
            }
        } else {
            out.push(c);
            visible += 1;
        }
    }
    out.push_str(ANSI_RESET);
    (out, visible)
}

pub fn print_highlighted_row(out: &mut Stdout, line: &str, cols: usize) -> Result<()> {
    use std::io::Write as _;

    let (painted, visible) = apply_bg_highlight(line, HIGHLIGHT_BG);
    let pad = cols.saturating_sub(visible);
    write!(
        out,
        "{painted}{HIGHLIGHT_BG}{:pad$}{ANSI_RESET}",
        "",
        pad = pad
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_state_clamps_and_scrolls() {
        let mut state = ListState::default();
        state.move_to(8, 10, 4);
        assert_eq!(state.selected, 8);
        assert_eq!(state.scroll_offset, 5);
        state.move_by(-7, 10, 4);
        assert_eq!(state.selected, 1);
        assert_eq!(state.scroll_offset, 1);
    }

    #[test]
    fn highlight_survives_ansi_resets() {
        let input = format!("a{ANSI_RESET}b");
        let (painted, visible) = apply_bg_highlight(&input, HIGHLIGHT_BG);
        assert_eq!(visible, 2);
        assert!(painted.matches(HIGHLIGHT_BG).count() >= 2);
    }

    #[test]
    fn region_height_fits_content_and_terminal() {
        assert_eq!(calculate_region_rows(5, 2, 20, Some(40)), 7);
        assert_eq!(calculate_region_rows(50, 2, 20, Some(40)), 22);
        assert_eq!(calculate_region_rows(50, 2, 20, Some(10)), 10);
        assert_eq!(calculate_region_rows(0, 2, 20, Some(40)), 3);
    }
}
