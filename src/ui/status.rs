//! Ambient status line for stderr. No-op when stderr is not a TTY (CI, piped
//! output) so logs stay clean.
//!
//! Operations announce themselves with [`push`]; the returned guard removes
//! the entry on drop, so `?`/`bail!` paths never leave a stale line. The
//! outermost entry is displayed: a handler's phase label wins over the I/O
//! layer's per-operation label. [`suspend`] clears the line while another
//! program owns the terminal (editors, passthrough subprocesses, prompts).

use std::io::{IsTerminal, Write};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

/// Braille spinner frames, shared with other liveness indicators (e.g. the
/// `pr rerun --watch` footer).
pub const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TICK: Duration = Duration::from_millis(100);
const CLEAR: &str = "\r\x1b[2K";

/// All status-line state behind one lock. Lock order is always `STATE` first,
/// stderr second.
struct State {
    /// Live entries, outermost first. The first entry is displayed.
    stack: Vec<(u64, String)>,
    next_id: u64,
    /// Depth of [`suspend`] guards; nothing is drawn while non-zero.
    suspended: usize,
    /// The line currently drawn on stderr, or `None` when the line is clear.
    painted: Option<String>,
    /// Whether the ticker thread is running.
    ticking: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    stack: Vec::new(),
    next_id: 0,
    suspended: 0,
    painted: None,
    ticking: false,
});

fn lock() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().expect("status state poisoned")
}

/// Announce an operation on the status line. The entry lives until the guard
/// drops; nested pushes stack, with the outermost entry displayed.
#[must_use]
pub fn push(msg: impl Into<String>) -> StatusGuard {
    let mut state = lock();
    let id = state.next_id;
    state.next_id += 1;
    state.stack.push((id, msg.into()));
    if !state.ticking && std::io::stderr().is_terminal() {
        state.ticking = true;
        thread::spawn(tick_loop);
    }
    StatusGuard { id }
}

/// Hand the terminal to someone else (editor, passthrough subprocess, prompt):
/// clears the status line and stops drawing until the guard drops.
#[must_use]
pub fn suspend() -> SuspendGuard {
    let mut state = lock();
    state.suspended += 1;
    clear(&mut state);
    SuspendGuard(())
}

/// Print `text` as its own line(s) on stderr, clearing and repainting a live
/// status line around it so the status line stays on the bottom line.
pub fn print_above(text: &str) {
    let state = lock();
    let mut err = std::io::stderr().lock();
    if state.painted.is_some() {
        let _ = write!(err, "{CLEAR}");
    }
    let _ = writeln!(err, "{text}");
    if let Some(line) = state.painted.as_deref() {
        let _ = write!(err, "{line}");
    }
    let _ = err.flush();
}

/// Print a line to stdout without colliding with the status line on stderr;
/// when both streams are the same terminal a raw `println!` would land on the
/// status line.
pub fn println_stdout(text: &str) {
    let state = lock();
    if state.painted.is_some() {
        let mut err = std::io::stderr().lock();
        let _ = write!(err, "{CLEAR}");
        let _ = err.flush();
    }
    {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{text}");
        let _ = out.flush();
    }
    if let Some(line) = state.painted.as_deref() {
        let mut err = std::io::stderr().lock();
        let _ = write!(err, "{line}");
        let _ = err.flush();
    }
}

/// Keeps a status entry alive; dropping removes it and clears the line when it
/// was the last one.
pub struct StatusGuard {
    id: u64,
}

impl StatusGuard {
    /// Replace this entry's message, e.g. for progress counters.
    pub fn set_message(&self, msg: impl Into<String>) {
        let mut state = lock();
        let id = self.id;
        if let Some(entry) = state.stack.iter_mut().find(|(eid, _)| *eid == id) {
            entry.1 = msg.into();
        }
    }
}

impl Drop for StatusGuard {
    fn drop(&mut self) {
        let mut state = lock();
        state.stack.retain(|(id, _)| *id != self.id);
        if state.stack.is_empty() {
            clear(&mut state);
        }
    }
}

/// Keeps the status line suspended; dropping resumes drawing on the next tick.
pub struct SuspendGuard(());

impl Drop for SuspendGuard {
    fn drop(&mut self) {
        lock().suspended -= 1;
    }
}

/// Clear the drawn line, if any. Caller holds the state lock.
fn clear(state: &mut State) {
    if state.painted.take().is_some() {
        let mut err = std::io::stderr().lock();
        let _ = write!(err, "{CLEAR}");
        let _ = err.flush();
    }
}

fn tick_loop() {
    let dim = anstyle::Style::new().dimmed();
    let mut frame = 0usize;
    loop {
        thread::sleep(TICK);
        let mut state = lock();
        if state.stack.is_empty() {
            clear(&mut state);
            state.ticking = false;
            return;
        }
        if state.suspended > 0 {
            continue;
        }
        let glyph = SPINNER_FRAMES[frame % SPINNER_FRAMES.len()];
        frame = frame.wrapping_add(1);
        let msg = &state.stack[0].1;
        let line = format!("{dim}{glyph} {msg}{dim:#}");
        let mut err = std::io::stderr().lock();
        let _ = write!(err, "{CLEAR}{line}");
        let _ = err.flush();
        drop(err);
        state.painted = Some(line);
    }
}
