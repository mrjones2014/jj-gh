//! Tiny braille spinner for stderr. No-op when stderr is not a TTY (CI, piped
//! output) so logs stay clean.
//!
//! Uses `anstyle` for the dim color escape and `tokio::time` for the tick
//! loop.

use std::io::{IsTerminal, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TICK: Duration = Duration::from_millis(100);

/// Animated spinner that runs on a background tokio task. Dropping without
/// calling [`Spinner::stop`] is safe (the task exits on the next tick) but
/// the final cleared line is best-effort in that case.
pub struct Spinner {
    handle: Option<JoinHandle<()>>,
    stop_tx: Option<oneshot::Sender<()>>,
    msg: Arc<Mutex<String>>,
}

impl Spinner {
    /// Start a spinner with `msg`. When stderr is not a terminal, returns a
    /// no-op spinner so callers can use it unconditionally.
    pub fn start(msg: impl Into<String>) -> Self {
        let msg = Arc::new(Mutex::new(msg.into()));
        if !std::io::stderr().is_terminal() {
            return Self {
                handle: None,
                stop_tx: None,
                msg,
            };
        }
        let task_msg = Arc::clone(&msg);
        let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            let dim = anstyle::Style::new().dimmed();
            let mut interval = tokio::time::interval(TICK);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut frame = 0usize;
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    _ = interval.tick() => {
                        let glyph = FRAMES[frame % FRAMES.len()];
                        frame = frame.wrapping_add(1);
                        let current = task_msg.lock().expect("spinner msg poisoned").clone();
                        let mut err = std::io::stderr().lock();
                        let _ = write!(err, "\r\x1b[2K{dim}{glyph} {current}{dim:#}");
                        let _ = err.flush();
                    }
                }
            }
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\r\x1b[2K");
            let _ = err.flush();
        });
        Self {
            handle: Some(handle),
            stop_tx: Some(stop_tx),
            msg,
        }
    }

    /// Replace the message shown next to the spinner glyph. No-op when stderr
    /// is not a terminal.
    pub fn set_message(&self, msg: String) {
        *self.msg.lock().expect("spinner msg poisoned") = msg;
    }

    /// Stop the spinner and clear its line. Awaiting ensures the cleared line
    /// is flushed before subsequent output.
    pub async fn stop(mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.handle.take() {
            let _ = h.await;
        }
    }
}
