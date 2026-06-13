use crate::{
    commands::pr::create::TitleCandidate,
    ui::tui::{
        HORIZONTAL_SEPARATOR, InlineSession, ListState, VERTICAL_SEPARATOR, bounded_region_rows,
        print_highlighted_row, truncate,
    },
};
use anyhow::{Result, anyhow};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    queue,
    style::{self, Stylize as _},
    terminal,
};
use std::io::{Stdout, Write};

const FOOTER_LINES: u16 = 2;
const KEYMAP: &str = "j/k=move  g/G=first/last  Enter=select  Esc/q=cancel";

pub(crate) fn pick(candidates: &[TitleCandidate]) -> Result<String> {
    let mut session = InlineSession::enter(bounded_region_rows(
        candidates.len(),
        FOOTER_LINES,
        candidates.len(),
    ))?;
    let mut out = std::io::stdout();
    let mut state = ListState::default();

    loop {
        render(&mut out, &mut session, candidates, &state)?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match handle_key(&mut state, candidates, visible_rows(&session), key) {
            Outcome::Continue => {}
            Outcome::Select(title) => return Ok(title),
            Outcome::Abort => return Err(anyhow!("title selection aborted")),
        }
    }
}

enum Outcome {
    Continue,
    Select(String),
    Abort,
}

fn handle_key(
    state: &mut ListState,
    candidates: &[TitleCandidate],
    visible: usize,
    key: KeyEvent,
) -> Outcome {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Outcome::Abort;
    }
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => state.move_by(1, candidates.len(), visible),
        KeyCode::Char('k') | KeyCode::Up => state.move_by(-1, candidates.len(), visible),
        KeyCode::Char('g') => state.move_to(0, candidates.len(), visible),
        KeyCode::Char('G') => {
            state.move_to(
                candidates.len().saturating_sub(1),
                candidates.len(),
                visible,
            );
        }
        KeyCode::Enter => {
            if let Some(title) = candidates
                .get(state.selected)
                .and_then(TitleCandidate::valid_title)
            {
                return Outcome::Select(title.to_string());
            }
        }
        KeyCode::Esc | KeyCode::Char('q') => return Outcome::Abort,
        _ => {}
    }
    Outcome::Continue
}

fn visible_rows(session: &InlineSession) -> usize {
    usize::from(session.rows().saturating_sub(FOOTER_LINES))
}

fn render(
    out: &mut Stdout,
    session: &mut InlineSession,
    candidates: &[TitleCandidate],
    state: &ListState,
) -> Result<()> {
    let cols = terminal::size()?.0;
    let height = visible_rows(session);
    session.begin_frame(out)?;
    let end = (state.scroll_offset + height).min(candidates.len());
    for (idx, candidate) in candidates[state.scroll_offset..end].iter().enumerate() {
        let absolute = state.scroll_offset + idx;
        let line = render_candidate(candidate, usize::from(cols));
        if absolute == state.selected {
            print_highlighted_row(out, &line, usize::from(cols))?;
        } else {
            queue!(out, style::Print(line))?;
        }
        queue!(out, cursor::MoveDown(1), cursor::MoveToColumn(0))?;
    }
    let rendered_rows = end.saturating_sub(state.scroll_offset);
    if rendered_rows < height {
        queue!(
            out,
            cursor::MoveDown(u16::try_from(height - rendered_rows).unwrap_or(u16::MAX)),
            cursor::MoveToColumn(0),
        )?;
    }
    queue!(
        out,
        style::SetForegroundColor(style::Color::DarkGrey),
        style::Print(HORIZONTAL_SEPARATOR.repeat(cols.into())),
        style::ResetColor,
        cursor::MoveDown(1),
        cursor::MoveToColumn(0),
        style::Print(KEYMAP),
    )?;
    out.flush()?;
    Ok(())
}

fn render_candidate(candidate: &TitleCandidate, cols: usize) -> String {
    let prefix = format!("{} {VERTICAL_SEPARATOR} ", candidate.change_id);
    let available = cols.saturating_sub(prefix.chars().count());
    if let Some(title) = candidate.valid_title() {
        format!(
            "{}{}{}",
            candidate.change_id.as_str().cyan(),
            format!(" {VERTICAL_SEPARATOR} ").dark_grey(),
            truncate(title, available)
        )
    } else {
        let placeholder = if candidate.title.trim().is_empty() {
            "(no description set)"
        } else {
            "(title must be one line)"
        };
        format!(
            "{}{}{}",
            candidate.change_id.as_str().cyan(),
            format!(" {VERTICAL_SEPARATOR} ").dark_grey(),
            format!("{placeholder} [invalid]").red()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(title: &str) -> TitleCandidate {
        TitleCandidate {
            change_id: "abcdefgh".into(),
            title: title.into(),
        }
    }

    #[test]
    fn enter_does_not_select_invalid_candidate() {
        let mut state = ListState::default();
        let outcome = handle_key(
            &mut state,
            &[candidate("")],
            20,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert!(matches!(outcome, Outcome::Continue));
    }

    #[test]
    fn enter_selects_trimmed_valid_candidate() {
        let mut state = ListState::default();
        let outcome = handle_key(
            &mut state,
            &[candidate(" title ")],
            20,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert!(matches!(outcome, Outcome::Select(title) if title == "title"));
    }

    #[test]
    fn invalid_rows_explain_empty_and_multiline_titles() {
        assert!(render_candidate(&candidate(""), 80).contains("(no description set)"));
        assert!(render_candidate(&candidate("a\nb"), 80).contains("(title must be one line)"));
    }
}
