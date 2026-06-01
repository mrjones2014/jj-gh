//! Crossterm-based TUI for `jj-gh pr restack`.
//!
//! Layout:
//!
//! ```text
//! +----------------------------------------+
//! | <scrollable rendered `jj pr log`>      |
//! | ...                                    |
//! +----------------------------------------+
//! | <pinned keymap bar / inline prompt>    |
//! +----------------------------------------+
//! ```
//!
//! The log is captured once via the `restack_log` template (which prepends a
//! sentinel `<OPEN><commit_id><CLOSE>` to each commit's rendering). The
//! captured bytes drive cursor navigation, scrolling, and per-PR decisions;
//! nothing is sent to the GitHub API until the user accepts the summary
//! screen.

use super::{Decision, PrPlan, RestackContext};
use crate::{
    config::Config,
    jj::Jj,
    pr::pr_log::{RESTACK_SENTINEL_CLOSE, RESTACK_SENTINEL_OPEN, build_aliases},
};
use anyhow::{Context, Result, anyhow};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute, queue, style, terminal,
};
use std::collections::HashMap;
use std::io::{Stdout, Write};
use tokio::process::Command;

const CONTROL_BAR_LINES: u16 = 2;
const DEFAULT_LOG_HEIGHT: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Browse,
    Picker,
    Summary,
    QuitConfirm,
}

enum LoopOutcome {
    Continue,
    Submit,
    Abort,
}

struct PickerState {
    target_pr: u64,
    input: String,
    selected: usize,
    candidates: Vec<String>,
}

struct UiState {
    plans: Vec<PrPlan>,
    decisions: HashMap<u64, Decision>,
    log_lines: Vec<String>,
    pr_to_line: HashMap<u64, usize>,
    pr_order: Vec<u64>,
    cursor_pr_idx: usize,
    scroll_offset: usize,
    mode: Mode,
    picker: Option<PickerState>,
    base_candidates: Vec<String>,
    summary_scroll: usize,
}

pub(super) async fn run<J: Jj>(
    jj: &J,
    ctx: &RestackContext,
    config: &Config,
    number_or_rev: Option<&str>,
) -> Result<HashMap<u64, Decision>> {
    let stdout_bytes = capture_log(jj, ctx, config).await?;
    let (log_lines, commit_to_line) = parse_sentinel_lines(&stdout_bytes);
    let pr_to_line = commit_map_to_pr_map(&commit_to_line, &ctx.plans);
    let pr_order = order_prs_by_line(&pr_to_line);

    let cursor_pr_idx = number_or_rev
        .and_then(|s| s.parse::<u64>().ok())
        .and_then(|num| pr_order.iter().position(|n| *n == num))
        .unwrap_or(0);

    let decisions: HashMap<u64, Decision> = ctx
        .plans
        .iter()
        .map(|p| (p.pr_number, Decision::Unset))
        .collect();

    let mut state = UiState {
        plans: ctx.plans.clone(),
        decisions,
        log_lines,
        pr_to_line,
        pr_order,
        cursor_pr_idx,
        scroll_offset: 0,
        mode: Mode::Browse,
        picker: None,
        base_candidates: build_base_candidates(ctx),
        summary_scroll: 0,
    };

    let guard = AltScreenGuard::enter()?;
    let mut out = std::io::stdout();

    let result = loop {
        render(&mut out, &state)?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match handle_key(&mut state, key) {
            LoopOutcome::Continue => {}
            LoopOutcome::Submit => break Ok(()),
            LoopOutcome::Abort => break Err(anyhow!("restack aborted")),
        }
    };

    drop(guard);

    match result {
        Ok(()) => Ok(state.decisions),
        Err(e) => Err(e),
    }
}

struct AltScreenGuard;

impl AltScreenGuard {
    fn enter() -> Result<Self> {
        terminal::enable_raw_mode().context("enabling raw mode")?;
        execute!(
            std::io::stdout(),
            terminal::EnterAlternateScreen,
            cursor::Hide,
        )
        .context("entering alt screen")?;
        Ok(Self)
    }
}

impl Drop for AltScreenGuard {
    fn drop(&mut self) {
        let _ = execute!(
            std::io::stdout(),
            cursor::Show,
            terminal::LeaveAlternateScreen,
        );
        let _ = terminal::disable_raw_mode();
    }
}

fn render(out: &mut Stdout, state: &UiState) -> Result<()> {
    let (cols, rows) = terminal::size().context("reading terminal size")?;
    let log_height = rows.saturating_sub(CONTROL_BAR_LINES) as usize;

    queue!(
        out,
        terminal::Clear(terminal::ClearType::All),
        cursor::MoveTo(0, 0),
    )?;

    match state.mode {
        Mode::Summary => render_summary(out, state, log_height)?,
        _ => render_log(out, state, log_height)?,
    }

    render_control_bar(out, state, cols, rows)?;
    out.flush()?;
    Ok(())
}

fn render_log(out: &mut Stdout, state: &UiState, log_height: usize) -> Result<()> {
    let cursor_line = state
        .pr_order
        .get(state.cursor_pr_idx)
        .and_then(|n| state.pr_to_line.get(n))
        .copied();

    let viewport_end = (state.scroll_offset + log_height).min(state.log_lines.len());
    for (i, line) in state.log_lines[state.scroll_offset..viewport_end]
        .iter()
        .enumerate()
    {
        let absolute = state.scroll_offset + i;
        let highlighted = Some(absolute) == cursor_line;
        queue!(out, cursor::MoveTo(0, u16::try_from(i).unwrap_or(u16::MAX)))?;
        if highlighted {
            queue!(
                out,
                style::SetAttribute(style::Attribute::Reverse),
                style::Print(line),
                style::SetAttribute(style::Attribute::Reset),
            )?;
        } else {
            queue!(out, style::Print(line))?;
        }
    }
    Ok(())
}

fn render_summary(out: &mut Stdout, state: &UiState, height: usize) -> Result<()> {
    let mut lines: Vec<String> = Vec::with_capacity(state.plans.len() + 2);
    lines.push("Summary  (y = submit, Esc/q = back)".to_string());
    lines.push(String::new());
    for p in &state.plans {
        let decision = state.decisions.get(&p.pr_number).unwrap_or(&Decision::Unset);
        let final_base = decision.final_base(p);
        let badge = decision_badge(decision, p);
        let action = match final_base {
            Some(b) if b != p.current_base => format!("update -> {b}"),
            _ => "no update".to_string(),
        };
        lines.push(format!(
            "#{n:<6} {bookmark:<20}  {current:<20} {action}  [{badge}]",
            n = p.pr_number,
            bookmark = p.bookmark,
            current = p.current_base,
        ));
    }

    let viewport_end = (state.summary_scroll + height).min(lines.len());
    for (i, line) in lines[state.summary_scroll..viewport_end].iter().enumerate() {
        queue!(
            out,
            cursor::MoveTo(0, u16::try_from(i).unwrap_or(u16::MAX)),
            style::Print(line),
        )?;
    }
    Ok(())
}

fn render_control_bar(
    out: &mut Stdout,
    state: &UiState,
    cols: u16,
    rows: u16,
) -> Result<()> {
    let bar_top = rows.saturating_sub(CONTROL_BAR_LINES);
    queue!(
        out,
        cursor::MoveTo(0, bar_top),
        style::SetAttribute(style::Attribute::Reset),
        style::Print("-".repeat(cols as usize)),
        cursor::MoveTo(0, bar_top + 1),
        terminal::Clear(terminal::ClearType::UntilNewLine),
    )?;

    match state.mode {
        Mode::Browse => queue!(out, style::Print(format_browse_status(state)))?,
        Mode::Picker => {
            if let Some(picker) = &state.picker {
                let candidate = picker
                    .candidates
                    .get(picker.selected)
                    .map_or("(no candidate)", String::as_str);
                let line = format!(
                    "edit #{pr}: {input}_  -> {candidate}  [Enter=apply Esc=cancel C-n/C-p]",
                    pr = picker.target_pr,
                    input = picker.input,
                );
                queue!(out, style::Print(line))?;
            }
        }
        Mode::Summary => {
            let line = format!(
                "summary  y=submit  Esc/q=back  ({} change(s) ready)",
                count_submittable(state),
            );
            queue!(out, style::Print(line))?;
        }
        Mode::QuitConfirm => queue!(
            out,
            style::Print("Discard decisions and quit? (y/N)"),
        )?,
    }
    Ok(())
}

fn format_browse_status(state: &UiState) -> String {
    let Some(idx) = state.pr_order.get(state.cursor_pr_idx) else {
        return "no PRs in stack  (q=quit)".into();
    };
    let plan = state
        .plans
        .iter()
        .find(|p| p.pr_number == *idx)
        .expect("cursor pr must have a plan");
    let decision = state.decisions.get(idx).unwrap_or(&Decision::Unset);
    let badge = decision_badge(decision, plan);
    let summary = if plan.is_no_change() {
        format!("no change ({})", plan.current_base)
    } else {
        format!("{} -> {}", plan.current_base, plan.proposed_base)
    };
    format!(
        "#{pr} {summary} [{badge}]   c=confirm e=edit s=skip j/k=move Enter=summary q=quit",
        pr = plan.pr_number,
    )
}

fn decision_badge(decision: &Decision, plan: &PrPlan) -> &'static str {
    match decision {
        Decision::Unset if plan.is_no_change() => "no-change",
        Decision::Unset => "unset",
        Decision::Confirm => "confirm",
        Decision::EditedTo(_) => "edited",
        Decision::Skip => "skip",
    }
}

fn count_submittable(state: &UiState) -> usize {
    state
        .plans
        .iter()
        .filter(|p| {
            state
                .decisions
                .get(&p.pr_number)
                .and_then(|d| d.final_base(p))
                .is_some_and(|b| b != p.current_base)
        })
        .count()
}

fn handle_key(state: &mut UiState, key: KeyEvent) -> LoopOutcome {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return LoopOutcome::Abort;
    }
    match state.mode {
        Mode::Browse => handle_browse(state, key),
        Mode::Picker => handle_picker(state, key),
        Mode::Summary => handle_summary(state, key),
        Mode::QuitConfirm => handle_quit_confirm(state, key),
    }
}

fn handle_browse(state: &mut UiState, key: KeyEvent) -> LoopOutcome {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match (key.code, ctrl) {
        (KeyCode::Char('j') | KeyCode::Down, false) => move_cursor(state, 1),
        (KeyCode::Char('k') | KeyCode::Up, false) => move_cursor(state, -1),
        (KeyCode::Char('d'), true) => scroll_viewport(state, 8),
        (KeyCode::Char('u'), true) => scroll_viewport(state, -8),
        (KeyCode::Char('g'), false) => move_cursor_to(state, 0),
        (KeyCode::Char('G'), _) => {
            let last = state.pr_order.len().saturating_sub(1);
            move_cursor_to(state, last);
        }
        (KeyCode::Char('c'), false) => set_decision_at_cursor(state, Decision::Confirm),
        (KeyCode::Char('s'), false) => set_decision_at_cursor(state, Decision::Skip),
        (KeyCode::Char('e'), false) => open_picker(state),
        (KeyCode::Enter, _) => state.mode = Mode::Summary,
        (KeyCode::Char('q'), false) | (KeyCode::Esc, _) => {
            let any_set = state
                .decisions
                .values()
                .any(|d| !matches!(d, Decision::Unset));
            if any_set {
                state.mode = Mode::QuitConfirm;
            } else {
                return LoopOutcome::Abort;
            }
        }
        _ => {}
    }
    LoopOutcome::Continue
}

fn handle_picker(state: &mut UiState, key: KeyEvent) -> LoopOutcome {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let Some(picker) = state.picker.as_mut() else {
        state.mode = Mode::Browse;
        return LoopOutcome::Continue;
    };
    match (key.code, ctrl) {
        (KeyCode::Esc, _) => {
            state.picker = None;
            state.mode = Mode::Browse;
        }
        (KeyCode::Enter, _) => {
            if let Some(chosen) = picker.candidates.get(picker.selected).cloned() {
                let bookmark = extract_bookmark(&chosen);
                state
                    .decisions
                    .insert(picker.target_pr, Decision::EditedTo(bookmark));
            }
            state.picker = None;
            state.mode = Mode::Browse;
        }
        (KeyCode::Char('n'), true) | (KeyCode::Down, _)
            if !picker.candidates.is_empty() =>
        {
            picker.selected = (picker.selected + 1) % picker.candidates.len();
        }
        (KeyCode::Char('p'), true) | (KeyCode::Up, _) if !picker.candidates.is_empty() => {
            picker.selected = picker
                .selected
                .checked_sub(1)
                .unwrap_or(picker.candidates.len() - 1);
        }
        (KeyCode::Backspace, _) => {
            picker.input.pop();
            refresh_picker_candidates(picker, &state.base_candidates);
        }
        (KeyCode::Char(c), false) => {
            picker.input.push(c);
            refresh_picker_candidates(picker, &state.base_candidates);
        }
        _ => {}
    }
    LoopOutcome::Continue
}

fn handle_summary(state: &mut UiState, key: KeyEvent) -> LoopOutcome {
    match (key.code, key.modifiers.contains(KeyModifiers::CONTROL)) {
        (KeyCode::Char('y'), false) => LoopOutcome::Submit,
        (KeyCode::Esc, _) | (KeyCode::Char('q'), false) => {
            state.mode = Mode::Browse;
            LoopOutcome::Continue
        }
        (KeyCode::Char('j') | KeyCode::Down, false) => {
            state.summary_scroll = state.summary_scroll.saturating_add(1);
            LoopOutcome::Continue
        }
        (KeyCode::Char('k') | KeyCode::Up, false) => {
            state.summary_scroll = state.summary_scroll.saturating_sub(1);
            LoopOutcome::Continue
        }
        _ => LoopOutcome::Continue,
    }
}

fn handle_quit_confirm(state: &mut UiState, key: KeyEvent) -> LoopOutcome {
    match key.code {
        KeyCode::Char('y' | 'Y') => LoopOutcome::Abort,
        KeyCode::Char('n' | 'N') | KeyCode::Esc => {
            state.mode = Mode::Browse;
            LoopOutcome::Continue
        }
        _ => LoopOutcome::Continue,
    }
}

fn move_cursor(state: &mut UiState, delta: isize) {
    if state.pr_order.is_empty() {
        return;
    }
    let len = i64::try_from(state.pr_order.len()).unwrap_or(i64::MAX);
    let next = (i64::try_from(state.cursor_pr_idx).unwrap_or(0) + delta as i64).clamp(0, len - 1);
    move_cursor_to(state, usize::try_from(next).unwrap_or(0));
}

fn move_cursor_to(state: &mut UiState, idx: usize) {
    state.cursor_pr_idx = idx.min(state.pr_order.len().saturating_sub(1));
    let Some(pr) = state.pr_order.get(state.cursor_pr_idx) else {
        return;
    };
    let Some(target_line) = state.pr_to_line.get(pr).copied() else {
        return;
    };
    let height = visible_log_rows().unwrap_or(DEFAULT_LOG_HEIGHT);
    if target_line < state.scroll_offset {
        state.scroll_offset = target_line;
    } else if target_line >= state.scroll_offset + height {
        state.scroll_offset = target_line + 1 - height;
    }
}

fn visible_log_rows() -> Option<usize> {
    terminal::size()
        .ok()
        .map(|(_, r)| r.saturating_sub(CONTROL_BAR_LINES) as usize)
}

fn scroll_viewport(state: &mut UiState, delta: isize) {
    let height = visible_log_rows().unwrap_or(DEFAULT_LOG_HEIGHT);
    let max_offset = state.log_lines.len().saturating_sub(height);
    let next = i64::try_from(state.scroll_offset).unwrap_or(0) + delta as i64;
    let clamped = next.max(0);
    state.scroll_offset = usize::try_from(clamped).unwrap_or(0).min(max_offset);
}

fn set_decision_at_cursor(state: &mut UiState, decision: Decision) {
    let Some(pr) = state.pr_order.get(state.cursor_pr_idx).copied() else {
        return;
    };
    state.decisions.insert(pr, decision);
    if state.cursor_pr_idx + 1 < state.pr_order.len() {
        move_cursor_to(state, state.cursor_pr_idx + 1);
    }
}

fn open_picker(state: &mut UiState) {
    let Some(pr) = state.pr_order.get(state.cursor_pr_idx).copied() else {
        return;
    };
    if state.base_candidates.is_empty() {
        return;
    }
    let mut picker = PickerState {
        target_pr: pr,
        input: String::new(),
        selected: 0,
        candidates: state.base_candidates.clone(),
    };
    refresh_picker_candidates(&mut picker, &state.base_candidates);
    state.picker = Some(picker);
    state.mode = Mode::Picker;
}

fn refresh_picker_candidates(picker: &mut PickerState, all: &[String]) {
    let needle = picker.input.to_lowercase();
    picker.candidates = all
        .iter()
        .filter(|c| needle.is_empty() || c.to_lowercase().contains(&needle))
        .cloned()
        .collect();
    if picker.selected >= picker.candidates.len() {
        picker.selected = 0;
    }
}

pub(crate) fn extract_bookmark(candidate: &str) -> String {
    candidate
        .split_once(" (")
        .map_or(candidate, |(name, _)| name)
        .to_string()
}

pub(crate) fn build_base_candidates(ctx: &RestackContext) -> Vec<String> {
    let mut by_bookmark: HashMap<&str, u64> = HashMap::new();
    for pr in &ctx.prs {
        by_bookmark.insert(pr.head_ref_name.as_str(), pr.number);
    }
    let mut out: Vec<String> = ctx
        .bookmarks
        .iter()
        .map(|b| match by_bookmark.get(b.name.as_str()) {
            Some(n) => format!("{} (#{n})", b.name),
            None => b.name.clone(),
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

async fn capture_log<J: Jj>(
    jj: &J,
    ctx: &RestackContext,
    config: &Config,
) -> Result<String> {
    let _ = jj;
    let branch_to_local: HashMap<String, String> = ctx
        .bookmarks
        .iter()
        .map(|b| (b.name.clone(), b.local_commit_id.clone()))
        .collect();
    let aliases = build_aliases(&ctx.prs, &branch_to_local, config);
    let tmp = aliases.write_temp_config()?;

    let mut cmd = Command::new("jj");
    cmd.arg("--config-file")
        .arg(tmp.path())
        .arg("log")
        .arg("--color=always")
        .args(["-T", "restack_log"]);
    let output = cmd.output().await.context("spawning `jj log` for restack")?;
    if !output.status.success() {
        return Err(anyhow!(
            "`jj log` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    drop(tmp);
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Walk `raw` line-by-line; for each line that starts with the sentinel
/// marker, extract the embedded commit id (40-hex) and record its line index.
/// The sentinel is stripped from the line before it is pushed to the output
/// buffer.
pub(crate) fn parse_sentinel_lines(raw: &str) -> (Vec<String>, HashMap<String, usize>) {
    let mut out_lines = Vec::new();
    let mut commit_to_line: HashMap<String, usize> = HashMap::new();
    for line in raw.lines() {
        let (cleaned, commit) = strip_sentinel(line);
        if let Some(id) = commit {
            commit_to_line.entry(id).or_insert(out_lines.len());
        }
        out_lines.push(cleaned);
    }
    (out_lines, commit_to_line)
}

fn strip_sentinel(line: &str) -> (String, Option<String>) {
    let open = RESTACK_SENTINEL_OPEN;
    let close = RESTACK_SENTINEL_CLOSE;
    let Some(open_idx) = line.find(open) else {
        return (line.to_string(), None);
    };
    let after_open = open_idx + open.len();
    let Some(rel_close) = line[after_open..].find(close) else {
        return (line.to_string(), None);
    };
    let close_idx = after_open + rel_close;
    let id = &line[after_open..close_idx];
    if id.len() != 40 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return (line.to_string(), None);
    }
    let mut cleaned = String::with_capacity(line.len());
    cleaned.push_str(&line[..open_idx]);
    cleaned.push_str(&line[close_idx + close.len()..]);
    (cleaned, Some(id.to_string()))
}

pub(crate) fn commit_map_to_pr_map(
    commits: &HashMap<String, usize>,
    plans: &[PrPlan],
) -> HashMap<u64, usize> {
    let mut out = HashMap::new();
    for p in plans {
        if let Some(line) = commits.get(&p.local_commit_id) {
            out.insert(p.pr_number, *line);
        }
    }
    out
}

pub(crate) fn order_prs_by_line(pr_to_line: &HashMap<u64, usize>) -> Vec<u64> {
    let mut pairs: Vec<(u64, usize)> = pr_to_line.iter().map(|(k, v)| (*k, *v)).collect();
    pairs.sort_by_key(|(_, line)| *line);
    pairs.into_iter().map(|(pr, _)| pr).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(num: u64, commit: &str) -> PrPlan {
        PrPlan {
            pr_number: num,
            pr_node_id: format!("ID{num}"),
            bookmark: format!("b-{num}"),
            local_commit_id: commit.to_string(),
            current_base: "master".into(),
            proposed_base: "master".into(),
        }
    }

    const OPEN: &str = RESTACK_SENTINEL_OPEN;
    const CLOSE: &str = RESTACK_SENTINEL_CLOSE;

    #[test]
    fn strip_sentinel_recognizes_valid_marker() {
        let id = "a".repeat(40);
        let raw = format!("{OPEN}{id}{CLOSE}  description");
        let (cleaned, commit) = strip_sentinel(&raw);
        assert_eq!(cleaned, "  description");
        assert_eq!(commit, Some(id));
    }

    #[test]
    fn strip_sentinel_keeps_prefix_text() {
        let id = "b".repeat(40);
        let raw = format!("@  {OPEN}{id}{CLOSE} header");
        let (cleaned, _) = strip_sentinel(&raw);
        assert_eq!(cleaned, "@   header");
    }

    #[test]
    fn strip_sentinel_passes_through_non_marker_lines() {
        let (cleaned, commit) = strip_sentinel("│  no marker here");
        assert_eq!(cleaned, "│  no marker here");
        assert!(commit.is_none());
    }

    #[test]
    fn strip_sentinel_rejects_short_id() {
        let id = "a".repeat(10);
        let raw = format!("{OPEN}{id}{CLOSE}tail");
        let (cleaned, commit) = strip_sentinel(&raw);
        assert_eq!(cleaned, raw);
        assert!(commit.is_none());
    }

    #[test]
    fn parse_sentinel_lines_builds_commit_map() {
        let id1 = "a".repeat(40);
        let id2 = "b".repeat(40);
        let raw = format!(
            "@ {OPEN}{id1}{CLOSE} header\n  description\n│ {OPEN}{id2}{CLOSE} other\n",
        );
        let (lines, map) = parse_sentinel_lines(&raw);
        assert_eq!(lines.len(), 3);
        assert_eq!(map.get(&id1), Some(&0));
        assert_eq!(map.get(&id2), Some(&2));
    }

    #[test]
    fn commit_map_to_pr_map_resolves_via_plans() {
        let id1 = "a".repeat(40);
        let id2 = "b".repeat(40);
        let mut commits = HashMap::new();
        commits.insert(id1.clone(), 0);
        commits.insert(id2.clone(), 4);
        let plans = vec![plan(10, &id1), plan(20, &id2)];
        let map = commit_map_to_pr_map(&commits, &plans);
        assert_eq!(map.get(&10), Some(&0));
        assert_eq!(map.get(&20), Some(&4));
    }

    #[test]
    fn order_prs_by_line_sorts_ascending() {
        let mut map = HashMap::new();
        map.insert(10u64, 5usize);
        map.insert(20u64, 2usize);
        map.insert(30u64, 8usize);
        assert_eq!(order_prs_by_line(&map), vec![20, 10, 30]);
    }

    #[test]
    fn picker_filter_substring_matches_bookmark_and_pr_number() {
        let mut picker = PickerState {
            target_pr: 1,
            input: "1234".into(),
            selected: 0,
            candidates: vec![],
        };
        let all = vec![
            "master".to_string(),
            "feature-a (#1234)".to_string(),
            "feature-b (#999)".to_string(),
        ];
        refresh_picker_candidates(&mut picker, &all);
        assert_eq!(picker.candidates, vec!["feature-a (#1234)"]);

        picker.input = "feature".into();
        refresh_picker_candidates(&mut picker, &all);
        assert_eq!(
            picker.candidates,
            vec!["feature-a (#1234)", "feature-b (#999)"],
        );

        picker.input = String::new();
        refresh_picker_candidates(&mut picker, &all);
        assert_eq!(picker.candidates.len(), 3);
    }

    #[test]
    fn extract_bookmark_strips_pr_suffix() {
        assert_eq!(extract_bookmark("master"), "master");
        assert_eq!(extract_bookmark("feature-a (#1234)"), "feature-a");
    }
}
