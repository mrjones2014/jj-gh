use crate::{
    commands::pr::create::TitleCandidate,
    ui::tui::{
        InlineSession, ListFrame, ListNav, ListState, VERTICAL_SEPARATOR, bounded_region_rows,
        handle_list_nav, render_list_rows, truncate,
    },
};
use anyhow::{Result, anyhow};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
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
    match handle_list_nav(key, state, candidates.len(), visible) {
        Some(ListNav::Quit) => return Outcome::Abort,
        Some(ListNav::Moved) => return Outcome::Continue,
        None => {}
    }
    if key.code == KeyCode::Enter
        && let Some(title) = candidates
            .get(state.selected)
            .and_then(TitleCandidate::valid_title)
    {
        return Outcome::Select(title.to_string());
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
    let cols = usize::from(terminal::size()?.0);
    render_list_rows(
        out,
        session,
        &ListFrame {
            state,
            len: candidates.len(),
            cols,
            footer_rows: FOOTER_LINES,
            empty_placeholder: "",
        },
        |idx| render_candidate(&candidates[idx], cols),
    )?;
    queue!(out, style::Print(KEYMAP))?;
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
    use crossterm::event::KeyModifiers;

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
