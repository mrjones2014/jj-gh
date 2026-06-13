//! Crossterm-based TUI for `jj-gh pr restack`.
//!
//! Layout:
//!
//! ```text
//! +----------------------------------------+
//! | <scrollable rendered `jj log`>         |
//! | ...                                    |
//! +----------------------------------------+
//! | <pinned keymap bar / inline prompt>    |
//! +----------------------------------------+
//! ```
//!
//! The log is captured via the user's configured template (either
//! `pr_restack_template`, `pr_log_template`, or the built-in default) so it
//! can be freely customized. To highlight which row in the graph belongs to
//! the focused PR, the user template is wrapped with two invisible Unicode
//! Private-Use markers (`U+E000` open, `U+E001` close) bracketing the
//! commit id; the wrapper sits *outside* the user's body so the user can
//! template anything inside it. After capture we strip the markers from
//! each line and record the line index → commit id mapping, which drives
//! reverse-video on the focused PR's commit row.
//! Nothing is sent to the GitHub API until the user accepts the summary
//! screen.

use crate::{
    commands::pr::{
        log::{PR_LOG_TEMPLATE, build_aliases},
        restack::{Decision, PrPlan, RestackArgs, RestackContext},
    },
    jj::{Jj, jj_argv},
    ui::tui::{
        HORIZONTAL_SEPARATOR, InlineSession, bounded_region_rows, ensure_visible, move_index,
        print_highlighted_row, truncate,
    },
};
use anyhow::{Context, Result, anyhow};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    queue, style, terminal,
};
use std::collections::HashMap;
use std::io::{Stdout, Write};

const CONTROL_BAR_LINES: u16 = 2;
const MAX_CONTENT_HEIGHT: usize = 20;
const ARROW_LEFT: &str = " ← ";
const ARROW_RIGHT: &str = "  → ";

/// Unicode Private-Use Area markers wrapping the commit id in our
/// restack-specific template. Invisible in terminals (no glyph), so users
/// who render the captured log directly (unlikely, since we strip them)
/// would not see them either. PUA chars round-trip through TOML and jj's
/// template-string parser as plain UTF-8.
const SENTINEL_OPEN: char = '\u{E000}';
const SENTINEL_CLOSE: char = '\u{E001}';

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
    viewport_height: usize,
}

pub async fn run(
    jj: &impl Jj,
    ctx: &RestackContext,
    args: &RestackArgs,
) -> Result<HashMap<u64, Decision>> {
    let stdout_bytes = capture_log(ctx, args).await?;
    let (log_lines, commit_to_line) = parse_sentinel_lines(&stdout_bytes);
    let pr_to_line = commit_map_to_pr_map(&commit_to_line, &ctx.plans);
    let pr_order = order_prs_topologically(jj, &ctx.plans).await?;

    let cursor_pr_idx = args
        .number_or_rev
        .as_ref()
        .and_then(|s| s.parse::<u64>().ok())
        .and_then(|num| pr_order.iter().position(|n| *n == num))
        .unwrap_or(0);

    let decisions = ctx
        .plans
        .iter()
        .map(|p| (p.pr_number, Decision::Unset))
        .collect();

    let requested_rows = bounded_region_rows(
        log_lines.len().max(ctx.plans.len() + 2),
        CONTROL_BAR_LINES,
        MAX_CONTENT_HEIGHT,
    );
    let mut session = InlineSession::enter(requested_rows)?;
    let viewport_height = usize::from(session.rows().saturating_sub(CONTROL_BAR_LINES));
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
        viewport_height,
    };

    let mut out = std::io::stdout();

    let result = loop {
        render(&mut out, &mut session, &state)?;
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

    drop(session);

    match result {
        Ok(()) => Ok(state.decisions),
        Err(e) => Err(e),
    }
}

fn render(out: &mut Stdout, session: &mut InlineSession, state: &UiState) -> Result<()> {
    let cols = terminal::size().context("reading terminal size")?.0;
    session.begin_frame(out)?;

    let rendered_rows = match state.mode {
        Mode::Summary => render_summary(out, state, state.viewport_height)?,
        _ => render_log(out, state, state.viewport_height)?,
    };
    if rendered_rows < state.viewport_height {
        queue!(
            out,
            cursor::MoveDown(
                u16::try_from(state.viewport_height - rendered_rows).unwrap_or(u16::MAX)
            ),
            cursor::MoveToColumn(0),
        )?;
    }

    render_control_bar(out, state, cols)?;
    out.flush()?;
    Ok(())
}

fn render_log(out: &mut Stdout, state: &UiState, log_height: usize) -> Result<usize> {
    let cursor_line = state
        .pr_order
        .get(state.cursor_pr_idx)
        .and_then(|n| state.pr_to_line.get(n))
        .copied();
    let cols = terminal::size().ok().map_or(80, |(c, _)| usize::from(c));
    let viewport_end = (state.scroll_offset + log_height).min(state.log_lines.len());
    for (i, line) in state.log_lines[state.scroll_offset..viewport_end]
        .iter()
        .enumerate()
    {
        let absolute = state.scroll_offset + i;
        let highlighted = Some(absolute) == cursor_line;
        if highlighted {
            print_highlighted_row(out, line, cols)?;
        } else {
            queue!(out, style::Print(line))?;
        }
        queue!(out, cursor::MoveDown(1), cursor::MoveToColumn(0))?;
    }
    Ok(viewport_end.saturating_sub(state.scroll_offset))
}

fn render_summary(out: &mut Stdout, state: &UiState, height: usize) -> Result<usize> {
    let total_rows = state.plans.len() + 2;
    let visible_rows = height.min(total_rows.saturating_sub(state.summary_scroll));
    let mut row = 0_u16;
    let mut idx = state.summary_scroll;
    while idx < state.summary_scroll + visible_rows {
        if idx == 0 {
            queue!(
                out,
                style::SetAttribute(style::Attribute::Bold),
                style::Print("Summary"),
                style::SetAttribute(style::Attribute::Reset),
                style::SetForegroundColor(style::Color::DarkGrey),
                style::Print("  (y = submit, Esc/q = back)"),
                style::ResetColor,
            )?;
        } else if idx == 1 {
            // blank spacer
        } else {
            let p = &state.plans[idx - 2];
            let decision = state
                .decisions
                .get(&p.pr_number)
                .unwrap_or(&Decision::Unset);
            render_summary_row(out, p, decision)?;
        }
        queue!(out, cursor::MoveDown(1), cursor::MoveToColumn(0))?;
        row = row.saturating_add(1);
        idx += 1;
    }
    Ok(usize::from(row))
}

fn render_summary_row(out: &mut Stdout, p: &PrPlan, decision: &Decision) -> Result<()> {
    let title = truncate(&p.title, 40);
    queue!(
        out,
        style::SetForegroundColor(style::Color::Cyan),
        style::Print(format!("#{:<5} ", p.pr_number)),
        style::SetForegroundColor(style::Color::White),
        style::Print(format!("{title:<41} ")),
        style::ResetColor,
    )?;

    let final_base = decision.final_base(p);
    let (base, changed) = match final_base {
        Some(b) if b != p.current_base => (b, true),
        _ => (p.current_base.as_str(), false),
    };
    let base_color = if changed {
        style::Color::Green
    } else {
        style::Color::Blue
    };
    queue!(
        out,
        style::SetForegroundColor(base_color),
        style::Print(base),
        style::SetForegroundColor(style::Color::DarkGrey),
        style::Print(ARROW_LEFT),
        style::SetForegroundColor(style::Color::Magenta),
        style::Print(&p.bookmark),
        style::ResetColor,
    )?;
    if changed {
        queue!(
            out,
            style::SetForegroundColor(style::Color::DarkGrey),
            style::Print(format!("  (was: {})", p.current_base)),
            style::ResetColor,
        )?;
    }

    queue!(out, style::Print("  "))?;
    render_badge(out, decision, p)?;
    Ok(())
}

const BROWSE_KEYMAP: &str = "c=confirm e=edit s=skip j/k=move Enter=summary q=quit";
const SUMMARY_KEYMAP: &str = "y=submit  Esc/q=back";
const PICKER_KEYMAP: &str = "Enter=apply Esc=cancel C-n/C-p";

fn render_control_bar(out: &mut Stdout, state: &UiState, cols: u16) -> Result<()> {
    let border = HORIZONTAL_SEPARATOR.repeat(cols.into());
    queue!(
        out,
        style::SetAttribute(style::Attribute::Reset),
        style::SetForegroundColor(style::Color::DarkGrey),
        style::Print(border),
        style::ResetColor,
        cursor::MoveDown(1),
        cursor::MoveToColumn(0),
        terminal::Clear(terminal::ClearType::UntilNewLine),
    )?;

    match state.mode {
        Mode::Browse => render_browse_status(out, state, cols)?,
        Mode::Picker => render_picker_status(out, state, cols)?,
        Mode::Summary => render_summary_status(out, state, cols)?,
        Mode::QuitConfirm => queue!(
            out,
            style::SetForegroundColor(style::Color::Yellow),
            style::Print("Discard decisions and quit? (y/N)"),
            style::ResetColor,
        )?,
    }
    Ok(())
}

fn render_browse_status(out: &mut Stdout, state: &UiState, cols: u16) -> Result<()> {
    let Some(idx) = state.pr_order.get(state.cursor_pr_idx) else {
        queue!(out, style::Print("no PRs in stack  (q=quit)"))?;
        return Ok(());
    };
    let plan = state
        .plans
        .iter()
        .find(|p| p.pr_number == *idx)
        .expect("cursor pr must have a plan");
    let decision = state.decisions.get(idx).unwrap_or(&Decision::Unset);

    let n = state.cursor_pr_idx + 1;
    let total = state.pr_order.len();
    let title = truncate(&plan.title, 50);
    let base = display_base(plan);
    let left_plain = format!(
        "[{n}/{total}] #{pr} {title}  {base}{ARROW_LEFT}{bookmark} [{badge}]",
        pr = plan.pr_number,
        bookmark = plan.bookmark,
        badge = decision_badge(decision, plan),
    );

    queue!(
        out,
        style::SetForegroundColor(style::Color::DarkGrey),
        style::Print(format!("[{n}/{total}] ")),
        style::SetForegroundColor(style::Color::Cyan),
        style::Print(format!("#{} ", plan.pr_number)),
        style::SetForegroundColor(style::Color::White),
        style::Print(title),
        style::Print("  "),
        style::ResetColor,
    )?;
    render_base_arrow(out, plan)?;
    queue!(out, style::Print(" "))?;
    render_badge(out, decision, plan)?;

    let right = BROWSE_KEYMAP;
    let used = left_plain.chars().count();
    let right_w = right.chars().count();
    let cols = usize::from(cols);
    if used + 2 + right_w <= cols {
        let pad = cols - used - right_w;
        queue!(
            out,
            style::Print(" ".repeat(pad)),
            style::SetForegroundColor(style::Color::DarkGrey),
            style::Print(right),
            style::ResetColor,
        )?;
    }
    Ok(())
}

fn render_picker_status(out: &mut Stdout, state: &UiState, cols: u16) -> Result<()> {
    let Some(picker) = &state.picker else {
        return Ok(());
    };
    let candidate = picker
        .candidates
        .get(picker.selected)
        .map_or("(no candidate)", String::as_str);
    let left_plain = format!(
        "edit #{pr}: {input}_  -> {candidate}",
        pr = picker.target_pr,
        input = picker.input,
    );
    queue!(
        out,
        style::SetForegroundColor(style::Color::Cyan),
        style::Print(format!("edit #{}: ", picker.target_pr)),
        style::ResetColor,
        style::Print(format!("{}_", picker.input)),
        style::SetForegroundColor(style::Color::DarkGrey),
        style::Print(ARROW_RIGHT),
        style::ResetColor,
        style::Print(candidate),
    )?;
    let used = left_plain.chars().count();
    let right_w = PICKER_KEYMAP.chars().count();
    let cols = usize::from(cols);
    if used + 2 + right_w <= cols {
        let pad = cols - used - right_w;
        queue!(
            out,
            style::Print(" ".repeat(pad)),
            style::SetForegroundColor(style::Color::DarkGrey),
            style::Print(PICKER_KEYMAP),
            style::ResetColor,
        )?;
    }
    Ok(())
}

fn render_summary_status(out: &mut Stdout, state: &UiState, cols: u16) -> Result<()> {
    let ready = count_submittable(state);
    let left_plain = format!("summary  ({ready} change(s) ready)");
    queue!(
        out,
        style::SetForegroundColor(style::Color::Green),
        style::Print("summary"),
        style::ResetColor,
        style::Print(format!("  ({ready} change(s) ready)")),
    )?;
    let used = left_plain.chars().count();
    let right_w = SUMMARY_KEYMAP.chars().count();
    let cols = usize::from(cols);
    if used + 2 + right_w <= cols {
        let pad = cols - used - right_w;
        queue!(
            out,
            style::Print(" ".repeat(pad)),
            style::SetForegroundColor(style::Color::DarkGrey),
            style::Print(SUMMARY_KEYMAP),
            style::ResetColor,
        )?;
    }
    Ok(())
}

/// Base ref to render as the merge target. Proposed for change PRs (the
/// new base after restack), current for no-change PRs (unchanged target).
fn display_base(plan: &PrPlan) -> &str {
    if plan.is_no_change() {
        &plan.current_base
    } else {
        &plan.proposed_base
    }
}

/// Render `<base> ← <bookmark>` mirroring GitHub's PR header layout
/// (base on the left, head/bookmark on the right). Base is colored
/// green for a change, blue for no-change.
fn render_base_arrow(out: &mut Stdout, plan: &PrPlan) -> Result<()> {
    let base_color = if plan.is_no_change() {
        style::Color::Blue
    } else {
        style::Color::Green
    };
    queue!(
        out,
        style::SetForegroundColor(base_color),
        style::Print(display_base(plan)),
        style::SetForegroundColor(style::Color::DarkGrey),
        style::Print(ARROW_LEFT),
        style::SetForegroundColor(style::Color::Magenta),
        style::Print(&plan.bookmark),
        style::ResetColor,
    )?;
    Ok(())
}

fn render_badge(out: &mut Stdout, decision: &Decision, plan: &PrPlan) -> Result<()> {
    let (label, color) = match decision {
        Decision::Unset if plan.is_no_change() => ("no-change", style::Color::DarkGrey),
        Decision::Unset => ("unset", style::Color::Yellow),
        Decision::Confirm => ("confirm", style::Color::Green),
        Decision::EditedTo(_) => ("edited", style::Color::Cyan),
        Decision::Skip => ("skip", style::Color::Red),
    };
    queue!(
        out,
        style::SetForegroundColor(color),
        style::Print(format!("[{label}]")),
        style::ResetColor,
    )?;
    Ok(())
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
        (KeyCode::Char('n'), true) | (KeyCode::Down, _) if !picker.candidates.is_empty() => {
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
    let next = move_index(state.cursor_pr_idx, delta, state.pr_order.len());
    move_cursor_to(state, next);
}

fn move_cursor_to(state: &mut UiState, idx: usize) {
    state.cursor_pr_idx = idx.min(state.pr_order.len().saturating_sub(1));
    let Some(pr) = state.pr_order.get(state.cursor_pr_idx) else {
        return;
    };
    let Some(target_line) = state.pr_to_line.get(pr).copied() else {
        return;
    };
    ensure_visible(target_line, &mut state.scroll_offset, state.viewport_height);
}

fn scroll_viewport(state: &mut UiState, delta: isize) {
    let height = state.viewport_height;
    let max_offset = state.log_lines.len().saturating_sub(height);
    let delta = i64::try_from(delta).expect("Scroll delta out of range");
    let next = i64::try_from(state.scroll_offset).unwrap_or(0) + delta;
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
    let mut by_bookmark = HashMap::new();
    for pr in &ctx.prs {
        by_bookmark.insert(pr.head_ref_name.as_str(), pr.number);
    }
    let mut out = ctx
        .bookmarks
        .iter()
        .map(|b| match by_bookmark.get(b.name.as_str()) {
            Some(n) => format!("{} (#{n})", b.name),
            None => b.name.clone(),
        })
        .collect::<Vec<_>>();
    out.sort();
    out.dedup();
    out
}

/// Capture `jj log` rendered with the user's chosen template wrapped in our
/// commit-id sentinel markers. Template resolution chain:
/// `pr_restack_template` -> `pr_log_template` -> built-in [`PR_LOG_TEMPLATE`].
/// The wrapper sits outside the user body so anything the user templates
/// renders as-is; we only consume the markers when post-processing the
/// captured stream.
async fn capture_log(ctx: &RestackContext, args: &RestackArgs) -> Result<String> {
    let branch_to_local = ctx
        .bookmarks
        .iter()
        .map(|b| (b.name.clone(), b.local_commit_id.clone()))
        .collect::<HashMap<String, String>>();
    let user_body = args
        .template
        .as_deref()
        .or(args.pr_log_template.as_deref())
        .unwrap_or(PR_LOG_TEMPLATE);
    let wrapped = format!(
        "\"{SENTINEL_OPEN}\" ++ commit_id.short(40) ++ \"{SENTINEL_CLOSE}\" ++ ({user_body})"
    );
    let aliases = build_aliases(
        &ctx.prs,
        &branch_to_local,
        args.nerdfonts,
        args.pr_log_template.as_deref(),
    )
    .alias("pr_log", wrapped);
    let tmp = aliases.write_temp_config()?;

    // Force color through the pipe; we parse the captured output for the TUI.
    let cfg = tmp.path().to_string_lossy().into_owned();
    let cmd = jj_argv(&[
        "--config-file",
        cfg.as_str(),
        "log",
        "--color=always",
        "-T",
        "pr_log",
    ]);
    let stdout = crate::proc::capture(&cmd)
        .await
        .context("spawning `jj log` for restack")?;
    drop(tmp);
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

/// Walk `raw` line-by-line; for each line containing the sentinel pair,
/// extract the embedded commit id (40-hex, with any ANSI color escapes
/// stripped from the captured bytes), record its line index, and strip the
/// sentinels (and any color escapes that landed between them) from the
/// displayed line.
fn parse_sentinel_lines(raw: &str) -> (Vec<String>, HashMap<String, usize>) {
    let mut out_lines = Vec::new();
    let mut commit_to_line = HashMap::<String, usize>::new();
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
    let Some(open_idx) = line.find(SENTINEL_OPEN) else {
        return (line.to_string(), None);
    };
    let after_open = open_idx + SENTINEL_OPEN.len_utf8();
    let Some(rel_close) = line[after_open..].find(SENTINEL_CLOSE) else {
        return (line.to_string(), None);
    };
    let close_idx = after_open + rel_close;
    let id_raw = &line[after_open..close_idx];
    let id_clean = strip_ansi(id_raw);
    if id_clean.len() != 40 || !id_clean.bytes().all(|b| b.is_ascii_hexdigit()) {
        return (line.to_string(), None);
    }
    let mut cleaned = String::with_capacity(line.len());
    cleaned.push_str(&line[..open_idx]);
    cleaned.push_str(&line[close_idx + SENTINEL_CLOSE.len_utf8()..]);
    (cleaned, Some(id_clean))
}

/// Strip ANSI CSI escapes (`ESC [ ... letter`) from `s`. jj's `--color=always`
/// wraps `commit_id.short(40)` in color codes; we need a clean 40-hex string
/// to validate and key on.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.next() == Some('[') {
                for ch in chars.by_ref() {
                    if ch.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn commit_map_to_pr_map(commits: &HashMap<String, usize>, plans: &[PrPlan]) -> HashMap<u64, usize> {
    let mut out = HashMap::new();
    for p in plans {
        if let Some(line) = commits.get(&p.local_commit_id) {
            out.insert(p.pr_number, *line);
        }
    }
    out
}

/// Order PRs by topology (newest first) so cursor navigation matches what
/// the user sees in the rendered log. Falls back to plan order for any PRs
/// the jj query does not emit.
async fn order_prs_topologically(jj: &impl Jj, plans: &[PrPlan]) -> Result<Vec<u64>> {
    if plans.is_empty() {
        return Ok(Vec::new());
    }
    let revset = plans
        .iter()
        .map(|p| format!("({})", p.local_commit_id))
        .collect::<Vec<_>>()
        .join("|");
    let template = r#"commit_id.short(40) ++ "\n""#;
    let stdout = jj
        .eval_template(&revset, template, None, false)
        .await
        .context("ordering PRs by topology")?;
    let commit_to_pr = plans
        .iter()
        .map(|p| (p.local_commit_id.as_str(), p.pr_number))
        .collect::<HashMap<&str, u64>>();
    let mut order = Vec::<u64>::with_capacity(plans.len());
    for line in stdout.lines() {
        let id = line.trim();
        if let Some(&pr) = commit_to_pr.get(id)
            && !order.contains(&pr)
        {
            order.push(pr);
        }
    }
    for p in plans {
        if !order.contains(&p.pr_number) {
            order.push(p.pr_number);
        }
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn strip_ansi_removes_csi_escapes() {
        assert_eq!(strip_ansi("\x1b[38;5;4mhello\x1b[39m"), "hello");
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("\x1b[1m\x1b[31mx\x1b[0m"), "x");
    }

    #[test]
    fn strip_sentinel_recognizes_plain_id() {
        let id = "a".repeat(40);
        let raw = format!("{SENTINEL_OPEN}{id}{SENTINEL_CLOSE}  description");
        let (cleaned, commit) = strip_sentinel(&raw);
        assert_eq!(cleaned, "  description");
        assert_eq!(commit, Some(id));
    }

    #[test]
    fn strip_sentinel_recognizes_ansi_wrapped_id() {
        let id = "b".repeat(40);
        let raw = format!("@  {SENTINEL_OPEN}\x1b[38;5;4m{id}\x1b[39m{SENTINEL_CLOSE} header");
        let (cleaned, commit) = strip_sentinel(&raw);
        assert_eq!(cleaned, "@   header");
        assert_eq!(commit, Some(id));
    }

    #[test]
    fn strip_sentinel_passes_through_non_marker_lines() {
        let input = format!("{}  no marker here", crate::ui::tui::VERTICAL_SEPARATOR);
        let (cleaned, commit) = strip_sentinel(&input);
        assert_eq!(cleaned, input);
        assert!(commit.is_none());
    }

    #[test]
    fn strip_sentinel_rejects_short_id() {
        let id = "a".repeat(10);
        let raw = format!("{SENTINEL_OPEN}{id}{SENTINEL_CLOSE}tail");
        let (_cleaned, commit) = strip_sentinel(&raw);
        assert!(commit.is_none());
    }

    #[test]
    fn parse_sentinel_lines_builds_commit_map() {
        let id1 = "a".repeat(40);
        let id2 = "b".repeat(40);
        let raw = format!(
            "@ {SENTINEL_OPEN}{id1}{SENTINEL_CLOSE} header\n  description\n{} {SENTINEL_OPEN}{id2}{SENTINEL_CLOSE} other\n",
            crate::ui::tui::VERTICAL_SEPARATOR,
        );
        let (lines, map) = parse_sentinel_lines(&raw);
        assert_eq!(lines.len(), 3);
        assert_eq!(map.get(&id1), Some(&0));
        assert_eq!(map.get(&id2), Some(&2));
    }
}
